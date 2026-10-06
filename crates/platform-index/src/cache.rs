//! Дисковый кэш собранного платформенного индекса.
//!
//! Сборка `PlatformIndex` из `shcntx_ru.hbk` — это холодный старт сервера:
//! разбор десятков тысяч html-страниц справки занимает секунды (замер на
//! 8.3.27: ~2,2 с разбора html из ~3,5 с общих до распараллеливания, ~1,3 с
//! после), а сам файл платформы между запусками не меняется. Кэш сохраняет
//! готовый индекс рядом (по умолчанию — `<log_dir>/platform-index.cache`) и на
//! следующем старте отдаёт его без повторного разбора (~0,09 с).
//!
//! Контракт кэша — оптимизация, а не источник правды:
//!
//! - **Годность** проверяется отпечатком hbk (путь, размер, время изменения,
//!   проба содержимого), версией формата и версией сборки сервера: файл
//!   платформы обновился, сервер обновился — кэш молча пересобирается.
//! - **Любая ошибка чтения кэша — не ошибка запуска.** Битый, обрезанный или
//!   чужой файл — предупреждение в журнал и обычная сборка из hbk. Без кэша
//!   сервер работает ровно так же, как до его появления, только дольше стартует.
//! - **Запись атомарна** (временный файл рядом + переименование): падение
//!   процесса посреди записи не оставляет полуфайл на месте рабочего кэша, а
//!   при любой неудаче временный файл убирается.
//! - **Содержимое не теряется.** Кэшируется тот же индекс, что отдал бы
//!   `load_from_hbk`: индекс из кэша обязан быть равен собранному (это
//!   проверяет интеграционный тест на реальном hbk).
//!
//! Формат файла: строка JSON с заголовком (версия + отпечаток), затем одним
//! значением — сам индекс. Заголовок отдельной строкой читается до разбора
//! полезной нагрузки: устаревший кэш отбрасывается, не тратя время на разбор
//! многих мегабайт.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::entities::{Method, Property, Type};
use crate::loader::load_from_hbk;
use crate::storage::PlatformIndex;

/// Имя файла кэша по умолчанию — в каталоге логов сервера.
pub const DEFAULT_CACHE_FILE_NAME: &str = "platform-index.cache";

/// Версия формата кэша. Поднимается при ЛЮБОМ изменении сохраняемых данных —
/// полей `Snapshot`, `Fingerprint` или `PlatformIndex`, — иначе старый файл был
/// бы прочитан как новый и молча дал другой индекс. Отдельно от версии крейта:
/// та меняется и на правках, к кэшу отношения не имеющих.
///
/// 3 — `note` у типов, методов, свойств и значений перечислений плюс `name_en`
/// у конструкторов: без подъёма версии старый кэш отдал бы пустые примечания.
const FORMAT_VERSION: u32 = 3;

/// Предел на строку заголовка кэша: файл без переводов строк иначе вычитал бы
/// в память гигабайты (путь кэша приходит из конфига).
const MAX_HEADER_BYTES: u64 = 64 * 1024;

/// Откуда взят индекс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSource {
    /// Готовый индекс прочитан из файла кэша.
    Cache,
    /// Индекс собран разбором hbk (кэша не было, устарел или был битым).
    Hbk,
}

/// Загрузить индекс с дисковым кэшем: кэш → при промахе hbk → запись кэша.
///
/// Ошибка возвращается только тогда, когда индекс не удалось собрать из hbk.
/// Проблемы самого кэша (нет файла, устарел, битый, не записался) — запись в
/// журнал, и работа продолжается разбором hbk.
pub fn load_cached(hbk: &Path, cache_path: &Path) -> Result<(PlatformIndex, LoadSource)> {
    let started = Instant::now();

    // Отпечаток снимается ДО сборки и используется и для чтения кэша, и для
    // записи нового. Если файл платформы заменят, пока идёт разбор, кэш не
    // должен получить НОВЫЙ отпечаток со СТАРЫМ содержимым — иначе следующий
    // старт молча прочитал бы устаревший индекс.
    let fingerprint = Fingerprint::of(hbk).ok();

    if let Some(fingerprint) = &fingerprint {
        match read_cache(cache_path, fingerprint) {
            Ok(Some(index)) => {
                // Симметрично записи: если hbk заменён между снятием отпечатка
                // и чтением, кэш уже неактуален — пересобираем.
                if Fingerprint::of(hbk).ok().as_ref() == Some(fingerprint) {
                    info!(
                        cache = %cache_path.display(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "платформенный индекс загружен из кэша"
                    );
                    return Ok((index, LoadSource::Cache));
                }
                warn!(
                    hbk = %hbk.display(),
                    "файл платформы изменился между снятием отпечатка и чтением кэша — собираю заново"
                );
            }
            Ok(None) => {}
            Err(e) => {
                // {:#} — вся цепочка контекстов: без неё в журнале остаётся
                // только верхний уровень причины.
                warn!(
                    cache = %cache_path.display(),
                    error = %format!("{e:#}"),
                    "кэш платформенного индекса не прочитан — собираю индекс из hbk"
                );
            }
        }
    }

    let index = load_from_hbk(hbk)?;
    let rebuilt_ms = started.elapsed().as_millis() as u64;

    // До сюда можно дойти с несостоявшимся отпечатком, только если файл
    // платформы пропадал и вернулся за время сборки — ошибка чтения hbk в этом
    // случае уже вернулась бы из `load_from_hbk`. Без отпечатка кэш не пишем.
    let Some(fingerprint) = fingerprint else {
        return Ok((index, LoadSource::Hbk));
    };

    // Файл мог измениться, пока шёл разбор: кэш по прежнему отпечатку уже
    // неактуален, и записывать его нет смысла.
    match Fingerprint::of(hbk) {
        Ok(now) if now == fingerprint => {}
        Ok(_) => {
            warn!(
                hbk = %hbk.display(),
                "файл платформы изменился во время сборки — кэш не сохранён"
            );
            return Ok((index, LoadSource::Hbk));
        }
        Err(e) => {
            warn!(
                hbk = %hbk.display(),
                error = %format!("{e:#}"),
                "отпечаток hbk не снялся после сборки — кэш не сохранён"
            );
            return Ok((index, LoadSource::Hbk));
        }
    }

    match write_cache(hbk, cache_path, &fingerprint, &index) {
        Ok(bytes) => info!(
            cache = %cache_path.display(),
            bytes,
            rebuilt_ms,
            "индекс собран из hbk, кэш сохранён"
        ),
        Err(e) => warn!(
            cache = %cache_path.display(),
            error = %format!("{e:#}"),
            rebuilt_ms,
            "кэш платформенного индекса не сохранён — старт продолжается без него"
        ),
    }
    Ok((index, LoadSource::Hbk))
}

/// Прочитать кэш. `Ok(None)` — кэша нет или он не относится к этому hbk
/// (штатный случай, молча). `Err` — файл есть, но испорчен.
fn read_cache(cache_path: &Path, fingerprint: &Fingerprint) -> Result<Option<PlatformIndex>> {
    let file = match File::open(cache_path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("открытие {}", cache_path.display()));
        }
    };

    let mut reader = BufReader::new(file);
    let mut header_line = String::new();
    let read = reader
        .by_ref()
        .take(MAX_HEADER_BYTES)
        .read_line(&mut header_line)
        .context("чтение заголовка кэша")?;
    if read == 0 {
        return Err(anyhow::anyhow!("файл кэша пуст"));
    }
    if !header_line.ends_with('\n') {
        return Err(anyhow::anyhow!(
            "заголовок кэша превышает {MAX_HEADER_BYTES} байт или оборван"
        ));
    }

    let header: Header = serde_json::from_str(&header_line).context("разбор заголовка кэша")?;

    if header.version != FORMAT_VERSION {
        return Ok(None);
    }
    if header.server_version != env!("CARGO_PKG_VERSION") {
        // Кэш собран другим бинарём: поля индекса могли измениться без смены
        // версии формата (правки парсера/маппера) — пересобираем.
        return Ok(None);
    }
    if header.fingerprint != *fingerprint {
        return Ok(None);
    }

    let snapshot: Snapshot =
        serde_json::from_reader(reader).context("разбор полезной нагрузки кэша")?;
    Ok(Some(snapshot.into_index()))
}

/// Сохранить индекс в кэш: временный файл рядом + переименование. Возвращает
/// размер файла в байтах.
///
/// Записывается ПЕРЕДАННЫЙ отпечаток, а не снятый с файла здесь: вызывающий
/// снял его до сборки, и именно с тем содержимым сохранённый индекс обязан
/// совпадать (см. `load_cached`).
///
/// Отказывается писать, если путь кэша указывает на сам файл платформы: опечатка
/// в конфиге иначе уничтожила бы справку `hbk` на первом же запуске.
fn write_cache(
    hbk: &Path,
    cache_path: &Path,
    fingerprint: &Fingerprint,
    index: &PlatformIndex,
) -> Result<u64> {
    let hbk_real = fs::canonicalize(hbk).unwrap_or_else(|_| hbk.to_path_buf());
    let cache_real = fs::canonicalize(cache_path).unwrap_or_else(|_| cache_path.to_path_buf());
    if cache_real == hbk_real {
        anyhow::bail!(
            "путь кэша {} совпадает с файлом платформы — запись кэша отменена",
            cache_path.display()
        );
    }

    if let Some(dir) = cache_path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)
                .with_context(|| format!("создание каталога {}", dir.display()))?;
        }
    }

    cleanup_stale_tmps(cache_path);

    let tmp = tmp_path(cache_path);
    let written = write_cache_tmp(&tmp, fingerprint, index);
    let bytes = match written {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };

    if let Err(e) = fs::rename(&tmp, cache_path) {
        // Полный, но ненужный временный файл в каталоге логов — мусор.
        let _ = fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e)).with_context(|| {
            format!(
                "переименование {} → {}",
                tmp.display(),
                cache_path.display()
            )
        });
    }
    Ok(bytes)
}

/// Записать заголовок и индекс во временный файл. Уборка файла при неудаче — на
/// вызывающем (`write_cache`).
fn write_cache_tmp(tmp: &Path, fingerprint: &Fingerprint, index: &PlatformIndex) -> Result<u64> {
    let file = File::create(tmp).with_context(|| format!("создание {}", tmp.display()))?;
    let mut writer = BufWriter::new(file);

    let header = Header {
        version: FORMAT_VERSION,
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        fingerprint: fingerprint.clone(),
    };
    serde_json::to_writer(&mut writer, &header).context("запись заголовка кэша")?;
    writer.write_all(b"\n").context("запись заголовка кэша")?;
    serde_json::to_writer(&mut writer, &SnapshotRef::of(index))
        .context("запись платформенного индекса в кэш")?;
    writer.flush().context("сброс буфера кэша")?;
    let file = writer.into_inner().context("закрытие файла кэша")?;
    // fsync до rename: переименованный файл не должен оказаться пустым или
    // частичным при потере питания.
    file.sync_all().context("fsync временного файла кэша")?;
    let bytes = file.metadata().context("размер файла кэша")?.len();
    Ok(bytes)
}

/// Best-effort уборка временных файлов, оставшихся от аварийно убитых
/// процессов (свои ошибки убирает `write_cache`). Удаляем только файлы
/// старше часа — временный файл живого писателя трогать нельзя.
fn cleanup_stale_tmps(cache_path: &Path) {
    use std::time::{Duration, SystemTime};
    let Some(dir) = cache_path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let Some(base) = cache_path.file_name().and_then(|s| s.to_str()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(base) || !name.ends_with(".tmp") {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Путь временного файла рядом с кэшем: переименование обязано быть атомарным,
/// а это гарантируется только в пределах одной файловой системы.
///
/// В имени — PID и счётчик: временные файлы не совпадут ни у двух процессов
/// (разные PID), ни у двух потоков одного процесса (разный счётчик).
fn tmp_path(cache_path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TMP: AtomicU64 = AtomicU64::new(1);
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let mut name = cache_path.as_os_str().to_owned();
    name.push(format!(".{}.{}.tmp", std::process::id(), n));
    PathBuf::from(name)
}

/// Заголовок файла кэша: версия формата + версия сборки + отпечаток hbk.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    version: u32,
    /// Версия сборки, записавшей кэш: правки парсера/маппера меняют содержимое
    /// индекса без смены структуры — такой кэш обязан быть отброшен.
    server_version: String,
    fingerprint: Fingerprint,
}

/// Отпечаток hbk-файла: путь + размер + время изменения + проба содержимого.
///
/// Полный хэш 40 МБ на каждом старте — та самая цена, которую кэш и убирает.
/// Поэтому считаем хэш только первых и последних 4 КиБ: подмена файла той же
/// длины с сохранённым mtime иначе осталась бы незамеченной.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    /// Канонический путь; при недоступности канонизации — как передали.
    hbk: String,
    len: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
    head_tail_hash: u64,
}

impl Fingerprint {
    fn of(hbk: &Path) -> Result<Self> {
        let meta = fs::metadata(hbk).with_context(|| format!("метаданные {}", hbk.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok());
        let hbk_path = fs::canonicalize(hbk).unwrap_or_else(|_| hbk.to_path_buf());
        Ok(Self {
            hbk: hbk_path.display().to_string(),
            len: meta.len(),
            mtime_secs: mtime.map(|d| d.as_secs() as i64).unwrap_or(0),
            mtime_nanos: mtime.map(|d| d.subsec_nanos()).unwrap_or(0),
            head_tail_hash: content_probe(&hbk_path)?,
        })
    }
}

/// Хэш содержимого первых и последних 4 КиБ (≈8 КиБ чтения — дёшево).
fn content_probe(path: &Path) -> Result<u64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::io::{Read, Seek, SeekFrom};

    const CHUNK: usize = 4096;
    let mut file = File::open(path).with_context(|| format!("открытие {}", path.display()))?;
    let mut head = vec![0u8; CHUNK];
    let n = file.read(&mut head)?;
    head.truncate(n);

    let mut hasher = DefaultHasher::new();
    head.hash(&mut hasher);
    let len = file.metadata().context("размер hbk")?.len();
    if len > (2 * CHUNK) as u64 {
        file.seek(SeekFrom::Start(len - CHUNK as u64))?;
        let mut tail = vec![0u8; CHUNK];
        let n = file.read(&mut tail)?;
        tail.truncate(n);
        tail.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

/// Индекс в кэше (десериализация).
#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    global_methods: Vec<Method>,
    global_properties: Vec<Property>,
    /// Отсортированы по `name_ru` — см. [`SnapshotRef::of`].
    types: Vec<Type>,
    /// `name_en` в нижнем регистре → ключ в `types`, сохранён как есть: при
    /// столкновении английских имён победителя определяет порядок вставки,
    /// который кэш не воспроизводит.
    types_en: BTreeMap<String, String>,
}

impl Snapshot {
    /// Собрать `PlatformIndex`: типы кладутся по ключу `name_ru` в нижнем
    /// регистре, затем вторичные карты берутся из кэша (не пересчитываются).
    fn into_index(self) -> PlatformIndex {
        let mut index = PlatformIndex::new();
        index.global_methods = self.global_methods;
        index.global_properties = self.global_properties;
        for ty in self.types {
            index.insert_type(ty);
        }
        index.types_en = self.types_en.into_iter().collect();
        index
    }
}

/// Индекс для записи в кэш — по ссылкам, без клонирования десятков мегабайт.
#[derive(Debug, Serialize)]
struct SnapshotRef<'a> {
    global_methods: &'a [Method],
    global_properties: &'a [Property],
    types: Vec<&'a Type>,
    types_en: BTreeMap<&'a str, &'a str>,
}

impl<'a> SnapshotRef<'a> {
    /// Типы сортируются по `name_ru`, английские имена — по своему ключу: файл
    /// кэша детерминирован, порядок обхода `HashMap` в него не протекает.
    fn of(index: &'a PlatformIndex) -> Self {
        let mut types: Vec<&Type> = index.types.values().collect();
        types.sort_by(|a, b| a.name_ru.cmp(&b.name_ru));
        Self {
            global_methods: &index.global_methods,
            global_properties: &index.global_properties,
            types,
            types_en: index
                .types_en
                .iter()
                .map(|(en, ru)| (en.as_str(), ru.as_str()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{Constructor, EnumValue, Parameter, Signature};

    fn fake_hbk(dir: &Path) -> PathBuf {
        let path = dir.join("shcntx_ru.hbk");
        fs::write(&path, b"not a real hbk, but a real file").expect("запись hbk");
        path
    }

    /// Запись кэша «как из load_cached»: отпечаток снимается до записи.
    fn save(hbk: &Path, cache: &Path, index: &PlatformIndex) -> Result<u64> {
        write_cache(hbk, cache, &Fingerprint::of(hbk)?, index)
    }

    /// Индекс со всеми группами полей: метод с параметром, свойство,
    /// конструктор, тип с методами/свойствами/конструкторами и перечисление.
    /// Круг «запись → чтение» обязан сохранить их все — новый забытый в
    /// `Snapshot`/`SnapshotRef` поле сломает тест.
    fn sample_index() -> PlatformIndex {
        let mut index = PlatformIndex::new();
        index.global_methods.push(Method {
            name_ru: "Сообщить".to_string(),
            name_en: "Message".to_string(),
            description: "Выводит сообщение".to_string(),
            return_type: "Число".to_string(),
            signatures: vec![Signature {
                name: "Основная".to_string(),
                syntax: "Сообщить(Текст)".to_string(),
                description: "Описание варианта".to_string(),
                parameters: vec![Parameter {
                    name: "Текст".to_string(),
                    type_name: "Строка".to_string(),
                    required: true,
                    description: "Что выводить".to_string(),
                }],
            }],
            note: Some("Примечание метода".to_string()),
        });
        index.global_properties.push(Property {
            name_ru: "Справочники".to_string(),
            name_en: "Catalogs".to_string(),
            description: "Менеджеры справочников".to_string(),
            type_name: "СправочникиМенеджер".to_string(),
            readonly: true,
            note: Some("Примечание свойства".to_string()),
        });
        index.insert_type(Type {
            name_ru: "Массив".to_string(),
            name_en: "Array".to_string(),
            description: "Динамический массив".to_string(),
            methods: vec![Method {
                name_ru: "Добавить".to_string(),
                name_en: "Add".to_string(),
                description: "Добавляет элемент".to_string(),
                return_type: String::new(),
                signatures: vec![Signature {
                    name: "Основная".to_string(),
                    syntax: String::new(),
                    description: String::new(),
                    parameters: vec![Parameter {
                        name: "Значение".to_string(),
                        type_name: "Произвольный".to_string(),
                        required: false,
                        description: String::new(),
                    }],
                }],
                note: None,
            }],
            properties: vec![Property {
                name_ru: "Количество".to_string(),
                name_en: "Count".to_string(),
                description: String::new(),
                type_name: "Число".to_string(),
                readonly: true,
                note: None,
            }],
            constructors: vec![Constructor {
                name: "Массив".to_string(),
                syntax: "Новый Массив(Фиксированный)".to_string(),
                description: "Пустой массив".to_string(),
                parameters: vec![Parameter {
                    name: "Фиксированный".to_string(),
                    type_name: "Булево".to_string(),
                    required: false,
                    description: String::new(),
                }],
                name_en: "Array".to_string(),
                note: Some("Примечание конструктора".to_string()),
            }],
            enum_values: Vec::new(),
            note: Some("Примечание типа".to_string()),
        });
        index.insert_type(Type {
            name_ru: "Цвет".to_string(),
            name_en: "Color".to_string(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![EnumValue {
                name_ru: "Красный".to_string(),
                name_en: "Red".to_string(),
                description: "Цвет".to_string(),
                note: Some("Примечание значения".to_string()),
            }],
            note: None,
        });
        index
    }

    #[test]
    fn round_trip_preserves_content() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        let index = sample_index();

        save(&hbk, &cache, &index).expect("запись кэша");
        let loaded = read_cache(&cache, &Fingerprint::of(&hbk).unwrap())
            .expect("чтение кэша")
            .expect("кэш обязан подойти");

        assert_eq!(loaded, index, "индекс из кэша разошёлся с исходным");
        // Производные карты пересчитаны: поиск по русскому и английскому имени.
        assert!(loaded.find_type("массив").is_some());
        assert!(loaded.find_type("Array").is_some());
        assert!(loaded.find_global_method("Message").is_some());
    }

    #[test]
    fn changed_hbk_invalidates_cache() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        save(&hbk, &cache, &sample_index()).expect("запись кэша");

        // Другой файл платформы: размер изменился.
        fs::write(&hbk, b"another platform build, longer than the first")
            .expect("новая версия hbk");

        assert!(
            read_cache(&cache, &Fingerprint::of(&hbk).unwrap())
                .expect("чтение")
                .is_none(),
            "кэш от другого hbk обязан быть отброшен"
        );
    }

    /// MINOR-1 из аудита: запись обязана сохранять ПЕРЕДАННЫЙ отпечаток, а не
    /// снятый с файла после сборки. Здесь файл заменён после снятия отпечатка —
    /// ровно та гонка, что была возможна между `load_from_hbk` и `write_cache`.
    #[test]
    fn write_cache_stores_given_fingerprint_not_current_file() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        let fingerprint_before = Fingerprint::of(&hbk).unwrap();

        fs::write(&hbk, b"another platform build, longer than the first")
            .expect("подмена hbk после снятия отпечатка");
        write_cache(&hbk, &cache, &fingerprint_before, &sample_index()).expect("запись кэша");
        let fingerprint_after = Fingerprint::of(&hbk).unwrap();
        assert_ne!(fingerprint_after, fingerprint_before);

        assert!(
            read_cache(&cache, &fingerprint_before)
                .expect("чтение")
                .is_some(),
            "кэш обязан соответствовать отпечатку на момент сборки"
        );
        assert!(
            read_cache(&cache, &fingerprint_after)
                .expect("чтение")
                .is_none(),
            "подменённый файл не должен получать чужое содержимое"
        );
    }

    #[test]
    fn cache_path_pointing_at_hbk_is_refused() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let payload = fs::read(&hbk).expect("чтение hbk");
        let fingerprint = Fingerprint::of(&hbk).unwrap();

        let err = write_cache(&hbk, &hbk, &fingerprint, &sample_index())
            .expect_err("запись поверх hbk запрещена");

        assert!(
            format!("{err:#}").contains("совпадает"),
            "причина обязана называть конфликт путей: {err:#}"
        );
        assert_eq!(
            fs::read(&hbk).expect("hbk на месте"),
            payload,
            "файл платформы обязан остаться нетронутым"
        );
    }

    /// NIT-5 из аудита: при неудаче временный файл не остаётся мусором.
    /// Переименование файла поверх каталога невозможно и на Windows, и на
    /// Linux — этим и создаётся отказ.
    #[test]
    fn failed_write_leaves_no_tmp() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join("cache-as-directory");
        fs::create_dir(&cache).expect("каталог на месте кэша");

        let result = save(&hbk, &cache, &sample_index());

        assert!(
            result.is_err(),
            "переименование поверх каталога обязано упасть"
        );
        assert!(
            !tmp_path(&cache).exists(),
            "временный файл обязан быть убран за собой"
        );
    }

    #[test]
    fn foreign_format_version_discarded() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        let fp = Fingerprint::of(&hbk).unwrap();
        let header = serde_json::json!({
            "version": FORMAT_VERSION + 1,
            "server_version": env!("CARGO_PKG_VERSION"),
            "fingerprint": serde_json::to_value(&fp).unwrap(),
        });
        fs::write(&cache, format!("{header}\n{{}}\n")).expect("запись кэша");

        assert!(read_cache(&cache, &fp).expect("чтение").is_none());
    }

    #[test]
    fn foreign_server_version_discarded() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        let fp = Fingerprint::of(&hbk).unwrap();
        let header = serde_json::json!({
            "version": FORMAT_VERSION,
            "server_version": "0.0.0-other-build",
            "fingerprint": serde_json::to_value(&fp).unwrap(),
        });
        fs::write(&cache, format!("{header}\n{{}}\n")).expect("запись кэша");

        assert!(
            read_cache(&cache, &fp).expect("чтение").is_none(),
            "кэш чужой сборки обязан быть отброшен"
        );
    }

    #[test]
    fn truncated_payload_is_error_not_silent_miss() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        save(&hbk, &cache, &sample_index()).expect("запись кэша");

        // Обрезаем полезную нагрузку: заголовок цел, payload — нет.
        let full = fs::read(&cache).expect("чтение файла");
        let cut = full.len() * 3 / 4;
        fs::write(&cache, &full[..cut]).expect("обрезка кэша");

        assert!(
            read_cache(&cache, &Fingerprint::of(&hbk).unwrap()).is_err(),
            "битый кэш обязан отличаться от устаревшего"
        );
    }

    #[test]
    fn missing_cache_is_silent_miss() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);

        assert!(read_cache(&cache, &Fingerprint::of(&hbk).unwrap())
            .expect("чтение")
            .is_none());
    }

    #[test]
    fn empty_cache_file_is_error_not_silent_miss() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = fake_hbk(dir.path());
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);
        fs::write(&cache, b"").expect("пустой файл");

        assert!(
            read_cache(&cache, &Fingerprint::of(&hbk).unwrap()).is_err(),
            "пустой файл — это испорченный кэш, а не его отсутствие"
        );
    }

    /// Без файла платформы кэша не появляется, а ошибка объясняет причину
    /// (`load_from_hbk`), а не «метаданные не снялись».
    #[test]
    fn missing_hbk_fails_with_platform_error_and_no_cache() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let hbk = dir.path().join("shcntx_ru.hbk"); // намеренно не создаём
        let cache = dir.path().join(DEFAULT_CACHE_FILE_NAME);

        let err = load_cached(&hbk, &cache).expect_err("без hbk индекса нет");

        assert!(
            format!("{err:#}").contains("hbk"),
            "ошибка обязана говорить о файле платформы: {err:#}"
        );
        assert!(!cache.exists(), "кэш не создаётся без файла платформы");
    }
}
