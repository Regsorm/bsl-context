//! Высокоуровневое чтение hbk: TOC из PackBlock + html-страницы из FileStorage.
//!
//! Порт `HbkContentReader.kt` (alkoleft).
//!
//! Pipeline:
//! 1. `HbkContainer::read(path)` — entities-таблица + сырые байты.
//! 2. `PackBlock` (entity name `"PackBlock"`) — это zip-контейнер с одним entry,
//!    внутри которого UTF-8 текст TOC. Распаковываем, парсим, получаем `Toc`.
//! 3. `FileStorage` (entity name `"FileStorage"`) — это zip-архив с html-страницами.
//!    Складываем в `ZipArchive<Cursor<Vec<u8>>>`, страницы достаём по `htmlPath`.

use std::io::{Cursor, Read};
use std::path::Path;

use zip::result::ZipError;
use zip::ZipArchive;

use crate::container::HbkContainer;
use crate::error::{HbkError, Result};
use crate::models::Toc;
use crate::toc::parse_toc;

const PACK_BLOCK_NAME: &str = "PackBlock";
const FILE_STORAGE_NAME: &str = "FileStorage";

/// Предел на распакованный размер одной html-страницы. Заявленный и
/// фактический размеры zip-entry недоверенные (zip-бомба), поэтому чтение
/// ограничивается. Реальные страницы платформы — единицы-сотни КБ
/// (замер по 8.3.17–8.5.1: максимум ≈ 182 КБ), 32 МБ — с большим запасом.
const MAX_PAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Предел на распакованный размер TOC PackBlock: реальный TOC — единицы МБ
/// (~0,5 млн токенов), 64 МБ — с запасом.
const MAX_TOC_BYTES: u64 = 64 * 1024 * 1024;

/// Содержимое hbk-файла, готовое для парсинга html-страниц.
pub struct HbkContent {
    pub toc: Toc,
    file_storage: ZipArchive<Cursor<Vec<u8>>>,
}

impl HbkContent {
    /// Прочитать hbk-файл целиком: распаковать TOC, открыть FileStorage.
    ///
    /// # Пример
    ///
    /// ```no_run
    /// use std::path::Path;
    /// use hbk_reader::HbkContent;
    ///
    /// let content = HbkContent::read(Path::new(
    ///     r"C:\Program Files\1cv8\8.3.27.2342\bin\shcntx_ru.hbk",
    /// ))?;
    /// println!("корневых страниц: {}", content.toc.pages.len());
    /// # Ok::<(), hbk_reader::HbkError>(())
    /// ```
    pub fn read(path: &Path) -> Result<Self> {
        let container = HbkContainer::read(path)?;

        // PackBlock — zip с одним entry, внутри UTF-8 текст TOC.
        let pack_block = container.get_entity(PACK_BLOCK_NAME)?;
        let toc_text = inflate_pack_block(&pack_block)?;
        let toc = parse_toc(&toc_text)?;

        // FileStorage — zip с html-страницами.
        let file_storage_bytes = container.get_entity(FILE_STORAGE_NAME)?;
        let cursor = Cursor::new(file_storage_bytes);
        let file_storage = ZipArchive::new(cursor)?;

        Ok(Self { toc, file_storage })
    }

    /// Прочитать html-страницу по её `htmlPath` из TOC.
    ///
    /// `htmlPath` в TOC начинается с `/`, а записи zip FileStorage хранятся
    /// без ведущего слэша — нормализуем, как апстрим.
    pub fn get_entry(&mut self, html_path: &str) -> Result<Vec<u8>> {
        if html_path.is_empty() {
            return Err(HbkError::BadFormat("пустой htmlPath".into()));
        }
        let name = html_path.trim_start_matches('/');
        let entry = match self.file_storage.by_name(name) {
            Ok(entry) => entry,
            // Отсутствие записи — штатный случай; остальные ошибки zip
            // (шифрование, неподдерживаемый метод) — настоящий отказ.
            Err(ZipError::FileNotFound) => {
                return Err(HbkError::HtmlEntryNotFound(html_path.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        let declared = entry.size();
        // Заявленный размер и фактическое содержимое entry недоверенные:
        // ёмкость ограничиваем, читаем с лимитом, излишек — ошибка.
        match read_limited(entry, declared, MAX_PAGE_BYTES).map_err(|e| HbkError::EntryRead {
            path: html_path.to_string(),
            source: e,
        })? {
            Some(buf) => Ok(buf),
            None => Err(HbkError::PageTooLarge {
                path: html_path.to_string(),
                limit: MAX_PAGE_BYTES,
            }),
        }
    }

    /// Прочитать html-страницу как UTF-8 строку.
    ///
    /// Платформа 1С хранит страницы в UTF-8 с BOM (`EF BB BF`); BOM срезается,
    /// чтобы текст начинался с `<`. Если когда-нибудь встретится cp1251 —
    /// расширим через `encoding_rs`.
    pub fn get_entry_text(&mut self, html_path: &str) -> Result<String> {
        let mut bytes = self.get_entry(html_path)?;
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            bytes.drain(..3);
        }
        String::from_utf8(bytes)
            .map_err(|e| HbkError::BadFormat(format!("страница {html_path}: не UTF-8 ({e})")))
    }
}

/// Распаковать PackBlock: это zip stream без central directory (только
/// Local File Header + deflate-данные). Апстрим использует `ZipInputStream`,
/// который как раз streaming. Rust `ZipArchive::new` требует EOCD и тут не
/// работает («Could not find EOCD»). Используем
/// `zip::read::read_zipfile_from_stream` — streaming-API без EOCD.
fn inflate_pack_block(data: &[u8]) -> Result<String> {
    let mut cursor = Cursor::new(data);
    let entry = zip::read::read_zipfile_from_stream(&mut cursor)
        .map_err(|e| HbkError::Inflate(format!("PackBlock не zip-stream: {e}")))?
        .ok_or_else(|| HbkError::Inflate("PackBlock не содержит entry".into()))?;
    let declared = entry.size();
    let Some(buf) = read_limited(entry, declared, MAX_TOC_BYTES)
        .map_err(|e| HbkError::Inflate(format!("PackBlock: ошибка чтения: {e}")))?
    else {
        return Err(HbkError::Inflate(format!(
            "PackBlock превышает лимит {MAX_TOC_BYTES} байт"
        )));
    };
    String::from_utf8(buf).map_err(|e| HbkError::Inflate(format!("PackBlock не UTF-8: {e}")))
}

/// Прочитать `reader` целиком, не доверяя заявленному размеру: ёмкость
/// буфера ограничена `limit`, чтение обрывается на `limit + 1` байте.
///
/// `Ok(None)` — данных больше лимита (zip-бомба); вызывающий решает, какой
/// ошибкой это оформить.
fn read_limited(reader: impl Read, declared: u64, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::with_capacity(declared.min(limit) as usize);
    reader.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        Ok(None)
    } else {
        Ok(Some(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_limited_passes_data_within_limit() {
        let data = vec![7u8; 100];
        let got = read_limited(Cursor::new(&data), 100, 1000)
            .unwrap()
            .unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn read_limited_rejects_data_over_limit() {
        let data = vec![7u8; 100];
        assert!(read_limited(Cursor::new(&data), 100, 50).unwrap().is_none());
    }

    /// Заявленный размер недоверенный: даже `size()=0` (или заведомо
    /// огромный) не спасает от обнаружения реального превышения лимита.
    #[test]
    fn read_limited_ignores_declared_size() {
        let data = vec![7u8; 100];
        assert!(read_limited(Cursor::new(&data), 0, 50).unwrap().is_none());
        assert!(read_limited(Cursor::new(&data), u64::MAX, 50)
            .unwrap()
            .is_none());
    }
}
