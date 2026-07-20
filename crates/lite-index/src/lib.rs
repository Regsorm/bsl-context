//! Облегчённый индекс методов конфигурации 1С: только объявления процедур и
//! функций по модулям выгрузки, без графов вызовов и без платформенных
//! метаданных. Назначение — быстрый ответ на вопросы «есть ли в конфигурации
//! такой метод» и «это экспортный метод глобального общего модуля», без
//! разбора контекста платформы (в отличие от `platform-index`/`bsl-validator`).
//!
//! **Почему крейт существует, хотя дублирует `code-index`.** Ответ на вопрос
//! «есть ли в конфигурации такой метод» уже даёт база `code-index`
//! (`SELECT name FROM functions`), поэтому мысль «убрать дублирование» возникает
//! регулярно. Она не проходит: `bsl-context` публикуется как самостоятельный
//! MCP-сервер, и у стороннего пользователя `code-index` не установлен.
//! Обязательная зависимость одного продукта от другого недопустима —
//! дублирование здесь сознательная цена автономности, а не недосмотр.
//! Источники поверх чужой базы (`CodeIndexDbSource`, `CodeIndexMcpSource`)
//! остаются опциональными: они не знают про `<Global>true</Global>`, поэтому
//! понижают уверенность находки вместо того, чтобы снять её совсем.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bsl_parse::MethodDecl;
use rayon::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use walkdir::WalkDir;

/// Схема облегчённого индекса. Индексы создаются сразу — база одноразовая,
/// пересобирается целиком на каждый `build`, поэтому цену вставки под уже
/// созданными индексами закладываем сознательно.
const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = OFF;      -- при сборке журнал не нужен: база одноразовая
PRAGMA synchronous = OFF;

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Модуль конфигурации: один .bsl-файл выгрузки.
CREATE TABLE modules (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,   -- относительный путь от корня выгрузки
    scope        TEXT NOT NULL,          -- base | extensions | external | other
    collection   TEXT,                   -- CommonModules | Documents | Catalogs | ...
    object_name  TEXT,                   -- имя объекта метаданных
    module_type  TEXT NOT NULL,          -- Module | ManagerModule | ObjectModule | RecordSetModule | Form | Command | Other
    is_global    INTEGER NOT NULL DEFAULT 0,  -- <Global>true</Global> у общего модуля
    is_extension INTEGER NOT NULL DEFAULT 0,  -- модуль расширения (директивы &Перед/&После/&Вместо/&ИзменениеИКонтроль)
    owner_path   TEXT                    -- для модуля формы внешней обработки — путь модуля объекта-владельца
);

-- Объявленный метод. name_lower считается в Rust: SQLite lower() НЕ сворачивает кириллицу.
CREATE TABLE methods (
    id          INTEGER PRIMARY KEY,
    module_id   INTEGER NOT NULL REFERENCES modules(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    name_lower  TEXT NOT NULL,
    is_function INTEGER NOT NULL,
    is_export   INTEGER NOT NULL,
    directive   TEXT,
    line_start  INTEGER NOT NULL,
    params      TEXT
);

CREATE INDEX idx_methods_name_lower ON methods(name_lower);
CREATE INDEX idx_methods_module     ON methods(module_id);
CREATE INDEX idx_modules_global     ON modules(is_global);

-- Объект конфигурации: один XML-файл выгрузки <Коллекция>/<Имя>.xml.
-- Модулей у объекта может не быть вовсе (в УТ 909 из 1069 перечислений),
-- поэтому список объектов строится по XML, а не по таблице modules.
CREATE TABLE objects (
    id            INTEGER PRIMARY KEY,
    collection    TEXT NOT NULL,       -- CommonModules | Catalogs | Enums | ...
    name          TEXT NOT NULL,       -- имя в исходном регистре
    name_lower    TEXT NOT NULL,       -- считается в Rust: SQLite lower() НЕ сворачивает кириллицу
    register_type TEXT                 -- Balance | Turnovers, только у регистров накопления
);
CREATE INDEX idx_objects_lookup ON objects(collection, name_lower);

-- Состав объекта: реквизиты, измерения, ресурсы. Нужен правилам оптимальности
-- запросов: по нему видно, есть ли отбор по измерению виртуальной таблицы и
-- индексировано ли поле, попавшее в условие.
--
-- Заполняется только для коллекций, которые могут быть источником запроса
-- (см. COLLECTIONS_WITH_FIELDS): читать XML всех тысяч объектов ради, скажем,
-- общих картинок незачем.
CREATE TABLE object_fields (
    id         INTEGER PRIMARY KEY,
    object_id  INTEGER NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    name_lower TEXT NOT NULL,
    kind       TEXT NOT NULL,          -- attribute | dimension | resource
    indexing   TEXT                    -- Index | IndexWithAdditionalOrder; NULL — не индексировано
);
CREATE INDEX idx_object_fields_object ON object_fields(object_id);

-- Экспортная переменная модуля приложения (`Перем Имя Экспорт;`).
-- Видна БЕЗ префикса из любого клиентского модуля, поэтому для проверяющего
-- кода это глобальное имя наравне с экспортным методом глобального общего
-- модуля. Общий модуль переменных иметь не может (проверено на УТ: в
-- base/CommonModules ни одной), так что источник таких имён — только модули
-- приложения. Без них `ПараметрыПриложения.Вставить(...)` выглядит обращением
-- к несуществующему общему модулю (замер на УТ: 123 ложные находки).
CREATE TABLE global_vars (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    name_lower TEXT NOT NULL
);
CREATE INDEX idx_global_vars_name ON global_vars(name_lower);
"#;

/// Известные каталоги-коллекции объектов метаданных (сегмент пути).
const KNOWN_COLLECTIONS: &[&str] = &[
    "CommonModules",
    "Documents",
    "Catalogs",
    "InformationRegisters",
    "AccumulationRegisters",
    "AccountingRegisters",
    "CalculationRegisters",
    "Reports",
    "DataProcessors",
    "ChartsOfAccounts",
    "ChartsOfCharacteristicTypes",
    "ChartsOfCalculationTypes",
    "BusinessProcesses",
    "Tasks",
    "ExchangePlans",
    "Constants",
    "DocumentJournals",
    "Enums",
    "CommonForms",
    "WebServices",
    "HTTPServices",
    "SettingsStorages",
    "FilterCriteria",
    "ScheduledJobs",
    "Sequences",
    "ExternalDataSources",
];

/// Директивы подключения расширения (регистронезависимо) — признак модуля расширения.
const EXTENSION_DIRECTIVES: &[&str] = &[
    "перед",
    "после",
    "вместо",
    "изменениеиконтроль",
    "before",
    "after",
    "around",
    "changeandvalidate",
];

/// Файлы модулей по имени → тип модуля.
const MODULE_TYPE_BY_FILE_NAME: &[(&str, &str)] = &[
    ("Module.bsl", "Module"),
    ("ManagerModule.bsl", "ManagerModule"),
    ("ObjectModule.bsl", "ObjectModule"),
    ("RecordSetModule.bsl", "RecordSetModule"),
    ("ValueManagerModule.bsl", "ValueManagerModule"),
    ("RecalculationModule.bsl", "RecalculationModule"),
    ("CommandModule.bsl", "CommandModule"),
];

/// Модули приложения: их экспортные переменные видны без префикса отовсюду.
/// Имена файлов — как в выгрузке конфигурации (каталог `Ext` в корне).
const APPLICATION_MODULE_FILE_NAMES: &[&str] = &[
    "ManagedApplicationModule.bsl",
    "OrdinaryApplicationModule.bsl",
    "SessionModule.bsl",
    "ExternalConnectionModule.bsl",
];

pub struct BuildStats {
    pub modules: usize,
    pub methods: usize,
    pub global_modules: usize,
    pub objects: usize,
    pub global_vars: usize,
    pub elapsed_ms: u128,
}

/// Построить индекс из каталога выгрузки. Если `db_path` существует — перезаписывается.
pub fn build(root: &Path, db_path: &Path, jobs: usize) -> Result<BuildStats> {
    let start = Instant::now();

    if jobs > 0 {
        // Игнорируем ошибку: глобальный пул rayon можно построить только один
        // раз за процесс (актуально при повторном build() внутри одного теста).
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global();
    }

    if db_path.exists() {
        std::fs::remove_file(db_path)
            .with_context(|| format!("не удалось удалить старую базу {}", db_path.display()))?;
    }

    // 1. Список .bsl-файлов выгрузки, минуя служебные каталоги.
    let bsl_files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            if !e.file_type().is_dir() {
                return true;
            }
            !matches!(e.file_name().to_str(), Some(".code-index") | Some(".git"))
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bsl"))
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    // 2. Факты XML: глобальные общие модули и полный список объектов конфигурации —
    // единым обходом (см. collect_xml_facts). Второй обход дерева ради одних
    // объектов не заводим.
    let xml_facts = collect_xml_facts(root);

    // 2a. Экспортные переменные модулей приложения: видны без префикса отовсюду.
    let global_var_names = collect_global_vars(&bsl_files);

    // 3. Параллельный разбор каждого модуля.
    let parsed: Vec<ParsedModule> = bsl_files
        .par_iter()
        .filter_map(|path| parse_module(root, path, &xml_facts.globals))
        .collect();

    // 4-5. Запись в SQLite: схема + одна транзакция для данных.
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("не удалось создать базу {}", db_path.display()))?;
    conn.execute_batch(SCHEMA_SQL)?;

    let mut modules_count = 0usize;
    let mut methods_count = 0usize;
    let mut global_modules_count = 0usize;
    let mut objects_count = 0usize;
    let mut global_vars_count = 0usize;

    {
        let tx = conn.transaction()?;
        {
            let mut insert_module = tx.prepare(
                "INSERT INTO modules (path, scope, collection, object_name, module_type, is_global, is_extension, owner_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let mut insert_method = tx.prepare(
                "INSERT INTO methods (module_id, name, name_lower, is_function, is_export, directive, line_start, params)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let mut insert_object = tx.prepare(
                "INSERT INTO objects (collection, name, name_lower, register_type) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut insert_field = tx.prepare(
                "INSERT INTO object_fields (object_id, name, name_lower, kind, indexing) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;

            for module in &parsed {
                insert_module.execute(params![
                    module.path,
                    module.scope,
                    module.collection,
                    module.object_name,
                    module.module_type,
                    module.is_global as i64,
                    module.is_extension as i64,
                    module.owner_path,
                ])?;
                let module_id = tx.last_insert_rowid();
                modules_count += 1;
                if module.is_global {
                    global_modules_count += 1;
                }

                for method in &module.methods {
                    insert_method.execute(params![
                        module_id,
                        method.name,
                        method.name.to_lowercase(),
                        method.is_function as i64,
                        method.is_export as i64,
                        method.directive,
                        method.line_start,
                        method.params,
                    ])?;
                    methods_count += 1;
                }
            }

            for object in &xml_facts.objects {
                insert_object.execute(params![
                    object.collection,
                    object.name,
                    object.name.to_lowercase(),
                    object.register_type,
                ])?;
                objects_count += 1;

                let object_id = tx.last_insert_rowid();
                for field in &object.fields {
                    insert_field.execute(params![
                        object_id,
                        field.name,
                        field.name.to_lowercase(),
                        field.kind,
                        field.indexing,
                    ])?;
                }
            }

            let mut insert_global_var = tx.prepare(
                "INSERT INTO global_vars (name, name_lower) VALUES (?1, ?2)",
            )?;
            for name in &global_var_names {
                insert_global_var.execute(params![name, name.to_lowercase()])?;
                global_vars_count += 1;
            }
        }
        tx.commit()?;
    }

    let elapsed_ms = start.elapsed().as_millis();

    // 6. meta.
    let root_abs = std::fs::canonicalize(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| root.display().to_string());
    let built_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    conn.execute_batch(&format!(
        "INSERT INTO meta (key, value) VALUES
            ('schema_version', '3'),
            ('root', '{}'),
            ('built_at', '{}'),
            ('modules', '{}'),
            ('methods', '{}'),
            ('objects', '{}'),
            ('global_vars', '{}'),
            ('elapsed_ms', '{}');",
        root_abs.replace('\'', "''"),
        built_at,
        modules_count,
        methods_count,
        objects_count,
        global_vars_count,
        elapsed_ms,
    ))?;

    Ok(BuildStats {
        modules: modules_count,
        methods: methods_count,
        global_modules: global_modules_count,
        objects: objects_count,
        global_vars: global_vars_count,
        elapsed_ms,
    })
}

/// Имя файла — модуль приложения? Его экспортные переменные видны без префикса
/// из любого клиентского модуля.
pub fn is_application_module_file(file_name: &str) -> bool {
    APPLICATION_MODULE_FILE_NAMES
        .iter()
        .any(|a| a.eq_ignore_ascii_case(file_name))
}

/// Экспортные переменные уровня модуля из ТЕКСТА (`Перем ПараметрыПриложения Экспорт;`).
///
/// Экспортной может быть только переменная УРОВНЯ МОДУЛЯ: внутри процедуры
/// `Экспорт` у `Перем` синтаксически невозможен. Поэтому проверять область
/// объявления не нужно — достаточно самого слова `Экспорт` в строке.
///
/// Разбор строковый, а не деревом: `AssignFact` из `bsl-parse` не хранит
/// признак экспортности, а заводить его ради четырёх файлов на конфигурацию
/// дороже, чем разобрать сами строки. Строковых литералов в объявлении `Перем`
/// не бывает, поэтому комментарий достаточно отрезать по `//`.
///
/// Публично: те же строки разбирают источники поверх `code-index`, у которых
/// файла на диске нет — только текст, полученный по сети. Правило чтения
/// объявления должно жить в одном месте.
pub fn global_export_vars_from_text(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let Some(sep) = code.find(char::is_whitespace) else {
            continue;
        };
        let (keyword, tail) = code.split_at(sep);
        // Сравнение через to_lowercase: eq_ignore_ascii_case кириллицу не сворачивает.
        if keyword.to_lowercase() != "перем" {
            continue;
        }
        // `Перем А Экспорт, Б Экспорт;` — `Экспорт` ставится у каждого имени.
        for part in tail.trim_end().trim_end_matches(';').split(',') {
            let mut words = part.split_whitespace();
            let Some(name) = words.next() else {
                continue;
            };
            if words.any(|w| w.to_lowercase() == "экспорт") {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Экспортные переменные всех модулей приложения выгрузки.
fn collect_global_vars(bsl_files: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    for path in bsl_files {
        let is_application_module = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_application_module_file);
        if !is_application_module {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        names.extend(global_export_vars_from_text(&text));
    }
    names
}

/// Факты по XML-выгрузке, извлекаемые ОДНИМ обходом дерева: имена глобальных
/// общих модулей и полный список объектов конфигурации по коллекциям.
struct XmlFacts {
    /// Имена глобальных общих модулей (`<Global>true</Global>`).
    globals: HashSet<String>,
    /// Объекты конфигурации — по одной записи на XML-файл выгрузки.
    objects: Vec<XmlObject>,
}

/// Разбор XML объекта для интеграционных тестов: (вид регистра, поля).
///
/// Сам разбор приватен — снаружи с ним работать незачем, но проверять его на
/// фрагментах настоящей выгрузки необходимо: раскладка тегов оказалась не той,
/// какой выглядела на первый взгляд.
pub fn parse_object_xml_for_tests(content: &str) -> (Option<String>, Vec<(String, String, Option<String>)>) {
    let (register_type, fields) = parse_object_xml(content);
    let fields = fields
        .into_iter()
        .map(|f| (f.name, f.kind.to_string(), f.indexing))
        .collect();
    (register_type, fields)
}

/// Состав объекта, как он лежит в индексе.
///
/// Тип нарочно «плоский», без зависимости от валидатора: `lite-index` о
/// проверках ничего не знает, а перекладку в `ObjectSchema` делает источник.
pub struct ObjectFields {
    /// `Balance` / `Turnovers` — только у регистров накопления.
    pub register_type: Option<String>,
    /// (имя, вид, признак индексирования): вид — `attribute` | `dimension` | `resource`.
    pub fields: Vec<(String, String, Option<String>)>,
}

/// Объект конфигурации со составом, если состав читался.
struct XmlObject {
    collection: String,
    name: String,
    /// `Balance` / `Turnovers` — только у регистров накопления.
    register_type: Option<String>,
    fields: Vec<XmlField>,
}

struct XmlField {
    name: String,
    /// `attribute` | `dimension` | `resource`.
    kind: &'static str,
    /// `<Indexing>`, если не `DontIndex` (последнее в выгрузку не пишется).
    indexing: Option<String>,
}

/// Коллекции, у которых читается состав: только те, что могут стоять
/// источником запроса. Для остальных достаточно имени объекта, а чтение XML —
/// это тысячи файлов на ровном месте.
const COLLECTIONS_WITH_FIELDS: &[&str] = &[
    "Catalogs",
    "Documents",
    "InformationRegisters",
    "AccumulationRegisters",
    "AccountingRegisters",
    "CalculationRegisters",
    "ChartsOfCharacteristicTypes",
    "ChartsOfAccounts",
    "ChartsOfCalculationTypes",
    "BusinessProcesses",
    "Tasks",
    "ExchangePlans",
    "DocumentJournals",
];

/// Собрать `XmlFacts` одним обходом дерева выгрузки — второй полный обход ради
/// одних лишь объектов не заводим, `<Коллекция>/<Имя>.xml` и так проходит мимо
/// при поиске глобальных общих модулей.
fn collect_xml_facts(root: &Path) -> XmlFacts {
    let mut globals = HashSet::new();
    let mut objects = Vec::new();

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_xml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"));
        if !is_xml {
            continue;
        }
        let Some(parent_name) = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str())
        else {
            continue;
        };
        // Коллекция — КАНОНИЧЕСКОЕ имя из KNOWN_COLLECTIONS (не то, что на диске),
        // сравнение регистронезависимое, как в parse_path.
        let Some(collection) = KNOWN_COLLECTIONS
            .iter()
            .find(|k| k.eq_ignore_ascii_case(parent_name))
        else {
            continue;
        };
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        // Содержимое читаем у общих модулей (нужен признак глобальности) и у
        // коллекций, которые бывают источником запроса (нужен состав). Для
        // остальных достаточно имени файла.
        let mut register_type = None;
        let mut fields = Vec::new();

        if *collection == "CommonModules" {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            if content.contains("<Global>true</Global>") {
                globals.insert(stem.to_string());
            }
        } else if COLLECTIONS_WITH_FIELDS.contains(collection) {
            if let Ok(content) = std::fs::read_to_string(path) {
                let parsed = parse_object_xml(&content);
                register_type = parsed.0;
                fields = parsed.1;
            }
        }

        objects.push(XmlObject {
            collection: collection.to_string(),
            name: stem.to_string(),
            register_type,
            fields,
        });
    }

    XmlFacts { globals, objects }
}

/// Разобрать XML объекта: вид регистра и состав полей.
///
/// Событийный разбор, а не поиск подстрок. Раскладка проверена на реальной
/// выгрузке УТ и НЕ такая, какой кажется:
///
/// ```xml
/// <Attribute uuid="…">
///     <Properties>
///         <Name>Контрагент</Name>
///         <Indexing>DontIndex</Indexing>
///     </Properties>
/// </Attribute>
/// ```
///
/// То есть `<Name>` и `<Indexing>` — внуки `<Attribute>`, а не прямые потомки;
/// отсюда `depth == field_depth + 2`. Глубина нужна ещё и потому, что `<Name>`
/// встречается во вложенных элементах (`ChoiceParameterLinks`, `Synonym`).
///
/// Реквизиты табличных частей пропускаются: `<TabularSection>` содержит свои
/// `<Attribute>`, и в составе самого объекта им не место.
fn parse_object_xml(content: &str) -> (Option<String>, Vec<XmlField>) {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut register_type: Option<String> = None;
    let mut fields: Vec<XmlField> = Vec::new();

    // Текущее поле: (глубина элемента, вид, имя, indexing).
    let mut current: Option<(usize, &'static str, Option<String>, Option<String>)> = None;
    // Глубина `<TabularSection>`, пока мы внутри неё.
    let mut tabular_depth: Option<usize> = None;
    // Что писать в ближайший текстовый узел.
    let mut want: Option<&'static str> = None;
    let mut depth = 0usize;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                match tag.as_str() {
                    "TabularSection" if tabular_depth.is_none() => tabular_depth = Some(depth),
                    "Attribute" | "Dimension" | "Resource" if tabular_depth.is_none() => {
                        let kind = match tag.as_str() {
                            "Attribute" => "attribute",
                            "Dimension" => "dimension",
                            _ => "resource",
                        };
                        current = Some((depth, kind, None, None));
                    }
                    "Name" if current.as_ref().is_some_and(|(d, ..)| depth == d + 2) => {
                        want = Some("name");
                    }
                    "Indexing" if current.as_ref().is_some_and(|(d, ..)| depth == d + 2) => {
                        want = Some("indexing");
                    }
                    // Вид регистра лежит в свойствах самого объекта, не в поле.
                    "RegisterType" if current.is_none() => want = Some("register_type"),
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let Some(target) = want.take() else { continue };
                let text = e.unescape().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                match target {
                    "register_type" => register_type = Some(text),
                    "name" => {
                        if let Some((_, _, name, _)) = current.as_mut() {
                            *name = Some(text);
                        }
                    }
                    "indexing" => {
                        if let Some((_, _, _, indexing)) = current.as_mut() {
                            // `DontIndex` означает «не индексировано» — не храним,
                            // чтобы отсутствие значения читалось однозначно.
                            if text != "DontIndex" {
                                *indexing = Some(text);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                if let Some((field_depth, kind, name, indexing)) = current.take() {
                    if depth == field_depth {
                        if let Some(name) = name {
                            fields.push(XmlField {
                                name,
                                kind,
                                indexing,
                            });
                        }
                    } else {
                        current = Some((field_depth, kind, name, indexing));
                    }
                }
                if tabular_depth == Some(depth) {
                    tabular_depth = None;
                }
                depth = depth.saturating_sub(1);
                want = None;
            }
            Ok(Event::Eof) => break,
            // Битый XML — отдаём то, что успели собрать: правило само решит,
            // хватает ли ему этого. Ронять сборку индекса из-за одного файла нельзя.
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    (register_type, fields)
}

/// Модуль после разбора — то, что пишется в строки `modules`/`methods`.
struct ParsedModule {
    path: String,
    scope: String,
    collection: Option<String>,
    object_name: Option<String>,
    module_type: String,
    is_global: bool,
    is_extension: bool,
    owner_path: Option<String>,
    methods: Vec<MethodDecl>,
}

fn parse_module(root: &Path, path: &Path, global_modules: &HashSet<String>) -> Option<ParsedModule> {
    let rel = path.strip_prefix(root).ok()?;
    let rel_path = rel.to_string_lossy().replace('\\', "/");

    // Лосси-чтение: двоичные/повреждённые по кодировке модули не теряют строку
    // в `modules` целиком — просто получают methods=0 (collect_methods сама
    // отбраковывает бинарный ввод по NUL-байту).
    let bytes = std::fs::read(path).ok()?;
    let source = String::from_utf8_lossy(&bytes);

    let methods = bsl_parse::collect_methods(&source);
    let masked = bsl_parse::mask_strings_and_comments(&source);
    let is_extension = has_extension_directive(&masked);

    let parsed_path = parse_path(&rel_path);

    let is_global = parsed_path.collection.as_deref() == Some("CommonModules")
        && parsed_path
            .object_name
            .as_ref()
            .is_some_and(|name| global_modules.contains(name));

    Some(ParsedModule {
        path: rel_path,
        scope: parsed_path.scope,
        collection: parsed_path.collection,
        object_name: parsed_path.object_name,
        module_type: parsed_path.module_type,
        is_global,
        is_extension,
        owner_path: parsed_path.owner_path,
        methods,
    })
}

/// Хотя бы одна директива подключения расширения (`&Перед`, `&После`, `&Вместо`,
/// `&ИзменениеИКонтроль`) на строке текста, замаскированного от строк/комментариев.
fn has_extension_directive(masked: &str) -> bool {
    masked.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix('&') else {
            return false;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_lowercase();
        EXTENSION_DIRECTIVES.contains(&name.as_str())
    })
}

/// Разобранный относительный путь модуля.
struct ParsedPath {
    scope: String,
    collection: Option<String>,
    object_name: Option<String>,
    module_type: String,
    owner_path: Option<String>,
}

/// Путь модуля объекта-владельца для модуля обычной формы внешней обработки.
///
/// `external/<Обработка>/Form/<Имя>/Form.obj.bsl` →
/// `external/<Обработка>/ExternalDataProcessor.obj.bsl`. Для остальных модулей — `None`.
/// Чистая функция от пути: индекс не нужен. Нужна и источникам поверх чужой базы
/// (см. крейт `symbol-source`), поэтому объявлена публично.
pub fn owner_module_path(rel_path: &str) -> Option<String> {
    let segments: Vec<&str> = rel_path.split('/').collect();
    if segments.len() == 5
        && segments[0].eq_ignore_ascii_case("external")
        && segments[2].eq_ignore_ascii_case("Form")
        && segments[4].eq_ignore_ascii_case("Form.obj.bsl")
    {
        Some(format!("{}/{}/ExternalDataProcessor.obj.bsl", segments[0], segments[1]))
    } else {
        None
    }
}

fn parse_path(rel_path: &str) -> ParsedPath {
    let segments: Vec<&str> = rel_path.split('/').collect();

    let scope = match segments.first().map(|s| s.to_lowercase()) {
        Some(s) if s == "base" => "base",
        Some(s) if s == "extensions" => "extensions",
        Some(s) if s == "external" => "external",
        _ => "other",
    }
    .to_string();

    let collection_idx = segments
        .iter()
        .position(|seg| KNOWN_COLLECTIONS.iter().any(|k| k.eq_ignore_ascii_case(seg)));
    let collection = collection_idx.map(|i| {
        KNOWN_COLLECTIONS
            .iter()
            .find(|k| k.eq_ignore_ascii_case(segments[i]))
            .copied()
            .unwrap_or_default()
            .to_string()
    });
    let object_name = collection_idx
        .and_then(|i| segments.get(i + 1))
        .map(|s| s.to_string());

    let file_name = segments.last().copied().unwrap_or("");

    // Путь под /Forms/ или /Form/ — это модуль формы, ДАЖЕ ЕСЛИ сам файл
    // называется Module.bsl (реальный экспорт формы: `.../Forms/Имя/Ext/Form/Module.bsl`,
    // проверено на выгрузке УТ) — поэтому проверка пути идёт ПЕРЕД именем файла.
    let module_type = if rel_path.contains("/Forms/") || rel_path.contains("/Form/") {
        "Form".to_string()
    } else if let Some((_, kind)) = MODULE_TYPE_BY_FILE_NAME
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(file_name))
    {
        kind.to_string()
    } else {
        "Other".to_string()
    };

    // Модуль формы внешней обработки: external/<Обработка>/Form/<Имя>/Form.obj.bsl
    // -> external/<Обработка>/ExternalDataProcessor.obj.bsl.
    let owner_path = owner_module_path(rel_path);

    ParsedPath {
        scope,
        collection,
        object_name,
        module_type,
        owner_path,
    }
}

/// Открытый на чтение облегчённый индекс.
pub struct LiteIndex {
    conn: Connection,
}

impl LiteIndex {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("не удалось открыть индекс {}", db_path.display()))?;
        Ok(Self { conn })
    }

    /// Есть ли ГДЕ-НИБУДЬ в конфигурации метод с таким именем (регистронезависимо).
    pub fn method_exists(&self, name: &str) -> Result<bool> {
        let name_lower = name.to_lowercase();
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM methods WHERE name_lower = ?1 LIMIT 1",
                params![name_lower],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Экспортный метод глобального общего модуля — такой зовут без префикса откуда угодно.
    pub fn is_global_export(&self, name: &str) -> Result<bool> {
        let name_lower = name.to_lowercase();
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM methods m JOIN modules md ON md.id = m.module_id \
                 WHERE m.name_lower = ?1 AND m.is_export = 1 AND md.is_global = 1 LIMIT 1",
                params![name_lower],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Имена методов конкретного модуля (в нижнем регистре).
    pub fn module_methods(&self, module_path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.name_lower FROM methods m JOIN modules md ON md.id = m.module_id \
             WHERE md.path = ?1",
        )?;
        let rows = stmt.query_map(params![module_path], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Экспортные методы модуля объекта-владельца (для модуля формы внешней обработки).
    ///
    /// Владельца берём из индекса, а если самого модуля там нет — выводим из пути:
    /// путь формы уже говорит, кто владелец. Иначе новая форма, ещё не попавшая в
    /// индекс, давала бы ложную находку на законном вызове метода своего объекта.
    pub fn owner_exports(&self, module_path: &str) -> Result<Vec<String>> {
        let owner_path: Option<String> = self
            .conn
            .query_row(
                "SELECT owner_path FROM modules WHERE path = ?1",
                params![module_path],
                |row| row.get(0),
            )
            .optional()?
            .flatten()
            .or_else(|| owner_module_path(module_path));

        let Some(owner_path) = owner_path else {
            return Ok(Vec::new());
        };

        let mut stmt = self.conn.prepare(
            "SELECT m.name_lower FROM methods m JOIN modules md ON md.id = m.module_id \
             WHERE md.path = ?1 AND m.is_export = 1",
        )?;
        let rows = stmt.query_map(params![owner_path], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Все объявленные имена методов (в нижнем регистре), одним запросом.
    ///
    /// Для источников, которые кэшируют индекс в памяти вместо построчного
    /// запроса `method_exists` на каждую проверку (см. крейт `symbol-source`).
    pub fn all_method_names(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT name_lower FROM methods")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Экспортные методы ГЛОБАЛЬНЫХ общих модулей (в нижнем регистре), одним запросом.
    /// Как выше — для кэширующих источников вместо построчного `is_global_export`.
    pub fn all_global_export_names(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT m.name_lower FROM methods m JOIN modules md ON md.id = m.module_id \
             WHERE m.is_export = 1 AND md.is_global = 1",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Имена объектов по коллекциям. `None` — таблицы `objects` в базе нет
    /// (индекс собран версией до неё): это «не знаю», а не «объектов нет».
    pub fn all_objects(&self) -> Result<Option<HashMap<String, HashSet<String>>>> {
        if !self.has_table("objects")? {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare("SELECT collection, name FROM objects")?;
        let rows =
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut out: HashMap<String, HashSet<String>> = HashMap::new();
        for row in rows {
            let (collection, name) = row?;
            out.entry(collection).or_default().insert(name);
        }
        Ok(Some(out))
    }

    /// Состав одного объекта: вид регистра и поля с признаком индексирования.
    ///
    /// Состав СЛИВАЕТСЯ по всем копиям объекта. Один объект встречается в
    /// выгрузке многократно — в базовой конфигурации и в каждом расширении,
    /// которое его дополняет (замер на УТ: у `Documents.ЗаказКлиента` 19 копий,
    /// 95 полей в базовой и ещё 2…56 в расширениях; всего таких имён 734).
    /// Взять первую попавшуюся строку значило бы вернуть пару реквизитов из
    /// случайного расширения вместо всего состава.
    ///
    /// Поле, объявленное в нескольких копиях, считается индексированным, если
    /// индекс есть хотя бы в одной: пропустить существующий индекс безопаснее,
    /// чем выдумать отсутствующий и дать ложную находку.
    ///
    /// `None` — таблицы `object_fields` в базе нет (индекс собран схемой 2 или
    /// раньше) либо такого объекта нет: и то и другое означает «не знаю»,
    /// и правило на этом обязано молчать. Пересобрать индекс — `rebuild_symbol_index`.
    pub fn object_schema(&self, collection: &str, name_lower: &str) -> Result<Option<ObjectFields>> {
        if !self.has_table("object_fields")? {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare(
            "SELECT o.register_type, f.name, f.kind, f.indexing \
             FROM objects o LEFT JOIN object_fields f ON f.object_id = o.id \
             WHERE o.collection = ?1 AND o.name_lower = ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![collection, name_lower], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;

        let mut register_type: Option<String> = None;
        // Порядок полей сохраняем: он повторяет порядок в выгрузке.
        let mut order: Vec<(String, String)> = Vec::new();
        let mut merged: HashMap<(String, String), (String, Option<String>)> = HashMap::new();
        let mut found = false;

        for row in rows {
            let (reg, name, kind, indexing) = row?;
            found = true;
            if register_type.is_none() {
                register_type = reg;
            }
            let (Some(name), Some(kind)) = (name, kind) else {
                continue; // копия без состава
            };
            let key = (kind.clone(), name.to_lowercase());
            match merged.get_mut(&key) {
                Some((_, existing)) => {
                    if existing.is_none() {
                        *existing = indexing;
                    }
                }
                None => {
                    order.push(key.clone());
                    merged.insert(key, (name, indexing));
                }
            }
        }

        if !found {
            return Ok(None);
        }

        let fields = order
            .into_iter()
            .filter_map(|key| {
                let (name, indexing) = merged.remove(&key)?;
                Some((name, key.0, indexing))
            })
            .collect();

        Ok(Some(ObjectFields {
            register_type,
            fields,
        }))
    }

    /// Имена экспортных переменных модулей приложения (нижний регистр).
    /// `None` — таблицы `global_vars` в базе нет (индекс собран версией до неё):
    /// это «не знаю», а не «таких переменных нет».
    pub fn all_global_var_names(&self) -> Result<Option<HashSet<String>>> {
        if !self.has_table("global_vars")? {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare("SELECT name_lower FROM global_vars")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(Some(rows.collect::<rusqlite::Result<HashSet<String>>>()?))
    }

    /// Есть ли таблица в базе? Индекс, собранный прежней версией, не содержит
    /// таблиц, добавленных позже, — отличаем «нет таблицы» от «таблица пуста».
    fn has_table(&self, name: &str) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                params![name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_module_path_resolves_external_form_module() {
        assert_eq!(
            owner_module_path("external/Моя обработка/Form/ФормаНастроек/Form.obj.bsl"),
            Some("external/Моя обработка/ExternalDataProcessor.obj.bsl".to_string())
        );
    }

    #[test]
    fn owner_module_path_none_for_base_module() {
        assert_eq!(owner_module_path("base/Documents/Заказ/Ext/ObjectModule.bsl"), None);
    }

    #[test]
    fn owner_module_path_none_for_owner_itself() {
        assert_eq!(
            owner_module_path("external/Обработка/ExternalDataProcessor.obj.bsl"),
            None
        );
    }

    #[test]
    fn owner_module_path_case_insensitive() {
        assert_eq!(
            owner_module_path("EXTERNAL/Обработка/FORM/Имя/FORM.OBJ.BSL"),
            Some("EXTERNAL/Обработка/ExternalDataProcessor.obj.bsl".to_string())
        );
    }
}
