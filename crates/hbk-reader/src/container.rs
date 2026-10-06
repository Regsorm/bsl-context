//! Чтение бинарного hbk-контейнера: entities-таблица + извлечение тел.
//!
//! Порт `HbkContainerReader.kt` (alkoleft/mcp-bsl-platform-context, исходно
//! на основе bsl-context от 1c-syntax).
//!
//! Формат контейнера (little-endian, mmap/`Vec<u8>`):
//! - skip 16 байт (int*4 заголовок)
//! - skip 2 байта (short)
//! - payloadSize: long-string (8 ASCII hex + 1 разделитель) → размер `fileInfos`
//! - blockSize:   long-string                                 → шаг до конца блока
//! - skip 11 байт (long + byte + short)
//! - читаем payloadSize байт в `fileInfos`; перепрыгиваем на `position + blockSize`
//! - в `fileInfos` каждые 12 байт = (headerAddress: i32, bodyAddress: i32, reserved: i32)
//!   reserved должен быть `i32::MAX` (`0x7FFFFFFF`), иначе формат битый
//! - имя файла читается у `headerAddress`: skip 2, payloadSize=long-string, skip 40,
//!   читаем `payloadSize - 24` байт UTF-16LE → имя
//! - тело читается у `bodyAddress`: skip 2, payloadSize=long-string, skip 20,
//!   читаем `payloadSize` байт

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use byteorder::{LittleEndian, ReadBytesExt};

use crate::error::{HbkError, Result};

const BYTES_BY_FILE_INFOS: usize = 12; // i32 * 3

/// Распарсенный hbk-контейнер: исходные байты + таблица «имя → адрес тела».
pub struct HbkContainer {
    /// Сырые байты файла: по адресам из `entities` читаются тела сущностей.
    buffer: Vec<u8>,
    /// Таблица «имя entity → адрес тела» (чтение — [`Self::get_entity`]).
    entities: HashMap<String, u32>,
}

impl HbkContainer {
    /// Прочитать hbk-файл с диска и распарсить таблицу entities.
    pub fn read(path: &Path) -> Result<Self> {
        // Без предварительного `exists()`: между проверкой и открытием файл
        // может исчезнуть (TOCTOU); отсутствие различаем по коду ошибки open.
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(HbkError::NotFound(path.to_path_buf()));
            }
            Err(e) => return Err(e.into()),
        };
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        let entities = Self::parse_entities(&buffer)?;
        Ok(Self { buffer, entities })
    }

    /// Прочитать тело entity по имени.
    pub fn get_entity(&self, name: &str) -> Result<Vec<u8>> {
        let addr = *self
            .entities
            .get(name)
            .ok_or_else(|| HbkError::EntityNotFound(name.to_string()))?;
        Self::read_body(&self.buffer, addr as usize)
    }

    fn parse_entities(buffer: &[u8]) -> Result<HashMap<String, u32>> {
        // Курсор на разрезе позиций — повторяет ByteBuffer.position() из Kotlin.
        let mut pos = 0usize;

        skip(&mut pos, 16)?; // int*4 — заголовок контейнера
        skip(&mut pos, 2)?; // short

        let payload_size = read_long_string(buffer, &mut pos)? as usize; // размер блока fileInfos
        let _block_size = read_long_string(buffer, &mut pos)?; // шаг до конца блока; далее не нужен

        skip(&mut pos, 11)?; // long + byte + short

        let file_infos = read_slice(buffer, pos, payload_size)?;

        // file_infos: блок длины payload_size, состоит из записей по 12 байт (i32 * 3).
        // На больших размерах могут быть лишние байты — округляем вниз.
        let count = file_infos.len() / BYTES_BY_FILE_INFOS;
        let mut entities = HashMap::with_capacity(count);
        for i in 0..count {
            let base = i * BYTES_BY_FILE_INFOS;
            let mut rdr = &file_infos[base..base + BYTES_BY_FILE_INFOS];
            let header_address = rdr.read_i32::<LittleEndian>()?;
            let body_address = rdr.read_i32::<LittleEndian>()? as i64; // может быть отрицательным как сырой i32
            let reserved = rdr.read_i32::<LittleEndian>()?;
            // В Kotlin: `if (reserved != Int.MAX_VALUE) throw`. Это `0x7FFFFFFF`.
            if reserved != i32::MAX {
                return Err(HbkError::BadFormat(format!(
                    "запись #{i}: reserved = {reserved:#x}, ожидалось {:#x}",
                    i32::MAX
                )));
            }
            // Отрицательный адрес — битый формат: `as usize` дал бы usize::MAX
            // и переполнение позиции при skip (в dev-профиле — паника).
            let header_address = usize::try_from(header_address).map_err(|_| {
                HbkError::BadFormat(format!("запись #{i}: headerAddress = {header_address}"))
            })?;
            let name = read_file_name(buffer, header_address)?;
            // body_address хранится как i32, но указывает на смещение в файле — приводим к u32.
            entities.insert(name, body_address as u32);
        }
        Ok(entities)
    }

    fn read_body(buffer: &[u8], body_address: usize) -> Result<Vec<u8>> {
        let mut pos = body_address;
        skip(&mut pos, 2)?;
        let payload_size = read_long_string(buffer, &mut pos)? as usize;
        skip(&mut pos, 20)?; // служебные поля заголовка тела (эмпирически 20 байт)
        let body = read_slice(buffer, pos, payload_size)?;
        Ok(body.to_vec())
    }
}

/// Прочитать имя entity у `header_address`.
///
/// Структура (порт `getHbkFileName`):
/// - skip 2 байта (short)
/// - payloadSize = long-string (8 ASCII hex + 1 разделитель)
/// - skip 40 байт (8 + 1 + 8 + 1 + 2 + 8 + 8 + 4)
/// - читаем `payloadSize - 24` байт UTF-16LE → имя
fn read_file_name(buffer: &[u8], header_address: usize) -> Result<String> {
    let mut pos = header_address;
    skip(&mut pos, 2)?;
    let payload_size = read_long_string(buffer, &mut pos)? as usize;
    skip(&mut pos, 40)?;

    let str_len = payload_size
        .checked_sub(24)
        .ok_or_else(|| HbkError::BadFormat("name payloadSize < 24".into()))?;
    let raw = read_slice(buffer, pos, str_len)?;
    Ok(decode_utf16le(raw))
}

/// Декодировать UTF-16LE-строку (имя файла в hbk-контейнере).
///
/// Декодирование lossy, как `new String(bytes, UTF_16LE)` в эталоне: битые
/// пары и нечётный хвост дают U+FFFD, а не отказ всего разбора контейнера.
fn decode_utf16le(raw: &[u8]) -> String {
    let (chunks, tail) = raw.as_chunks::<2>();
    let units: Vec<u16> = chunks
        .iter()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let mut decoded = String::from_utf16_lossy(&units);
    if !tail.is_empty() {
        // Неполный последний символ: Java-декодер отдал бы U+FFFD.
        decoded.push('\u{FFFD}');
    }
    decoded
}

/// Прочитать long-string: 8 байт ASCII hex (например "00000010") + 1 байт-разделитель.
/// Это размер блока, парсится как hex → i32.
///
/// Порт `getLongString` из `HbkContainerReader.kt`.
fn read_long_string(buffer: &[u8], pos: &mut usize) -> Result<i32> {
    let raw = read_slice(buffer, *pos, 8)?;
    *pos += 8;
    // отдельный байт-разделитель (часто пробел или '\n')
    skip(pos, 1)?;
    // Без trim(): эталон (Long.parseLong(s, 16)) пробелы не принимает.
    let s =
        std::str::from_utf8(raw).map_err(|e| HbkError::BadFormat(format!("long-string: {e}")))?;
    let v = i64::from_str_radix(s, 16)
        .map_err(|e| HbkError::BadFormat(format!("long-string не hex '{s}': {e}")))?;
    i32::try_from(v).map_err(|_| HbkError::BadFormat(format!("long-string вне i32: {s}")))
}

#[inline]
fn skip(pos: &mut usize, n: usize) -> Result<()> {
    *pos = pos
        .checked_add(n)
        .ok_or_else(|| HbkError::BadFormat("position overflow".into()))?;
    Ok(())
}

fn read_slice(buffer: &[u8], pos: usize, len: usize) -> Result<&[u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| HbkError::BadFormat("slice overflow".into()))?;
    buffer
        .get(pos..end)
        .ok_or_else(|| HbkError::BadFormat(format!("read {len} bytes at {pos}: out of bounds")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Минимальный контейнер: заголовок 16+2, два long-string (payloadSize,
    /// blockSize), 11 служебных байт и блок fileInfos.
    fn container_with(file_infos: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 16 + 2];
        buf.extend_from_slice(b"0000000C "); // payloadSize = 12
        buf.extend_from_slice(b"0000000C "); // blockSize = 12
        buf.extend_from_slice(&[0u8; 11]);
        buf.extend_from_slice(file_infos);
        buf
    }

    /// Отрицательный headerAddress раньше приводил к `as usize` = usize::MAX
    /// и переполнению позиции (паника в dev-профиле) — теперь BadFormat.
    #[test]
    fn negative_header_address_is_bad_format() {
        let mut infos = Vec::new();
        infos.extend_from_slice(&(-1i32).to_le_bytes());
        infos.extend_from_slice(&0i32.to_le_bytes());
        infos.extend_from_slice(&i32::MAX.to_le_bytes());
        let err = HbkContainer::parse_entities(&container_with(&infos))
            .expect_err("отрицательный headerAddress должен быть ошибкой");
        assert!(matches!(err, HbkError::BadFormat(_)), "{err}");
    }

    /// Декодирование имён lossy, как `new String(bytes, UTF_16LE)`: битые
    /// пары и нечётный хвост не роняют разбор всего контейнера.
    #[test]
    fn utf16le_decoding_is_lossy() {
        // lone high surrogate D800 + 'A' → U+FFFD + 'A'
        assert_eq!(decode_utf16le(&[0x00, 0xD8, 0x41, 0x00]), "\u{FFFD}A");
        // нечётная длина: хвостовой байт → U+FFFD
        assert_eq!(decode_utf16le(&[0x41, 0x00, 0x7F]), "A\u{FFFD}");
    }
}
