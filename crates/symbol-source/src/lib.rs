//! Источники имён методов конфигурации для `bsl_validator::SymbolSource`.
//!
//! Три реализации, каждая закрывает случай "метод объявлен в ДРУГОМ модуле
//! конфигурации, поэтому валидатор его не видит и путает вызов с опиской":
//!
//! - [`LiteSource`] — свой облегчённый индекс (`lite-index`): знает и
//!   `is_global_export`, и `owner_exports`, все проверки в памяти (O(1)).
//! - [`CodeIndexDbSource`] — прямое чтение SQLite-базы `code-index`
//!   (`<repo>/.code-index/index.db`). Отдельного флага "глобальный общий
//!   модуль" в базе нет, но признак извлекается из XML общих модулей
//!   (`<Global>true</Global>`), которые база хранит как есть.
//! - [`CodeIndexMcpSource`] — HTTP к живому MCP-серверу `code-index` (когда
//!   прямого доступа к файлу базы нет, например при удалённом деплое).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;

use bsl_validator::SymbolSource;

// ── LiteSource ──────────────────────────────────────────────────────────────

/// Источник поверх собственного облегчённого индекса конфигурации (`lite-index`).
///
/// При открытии поднимает в память набор ВСЕХ имён методов и набор экспортных
/// имён глобальных общих модулей: на базе в 82 МБ это доли секунды, зато
/// `is_global_export`/`method_exists` дальше — O(1), без похода в SQLite на
/// каждый вызов (`validate_module` может проверять сотни имён за один запрос).
pub struct LiteSource {
    // `rusqlite::Connection` (внутри `LiteIndex`) не `Sync` — под мьютексом,
    // чтобы `LiteSource` можно было держать в `Arc<dyn SymbolSource>` из
    // нескольких асинхронных обработчиков сервера. `owner_exports` — единственный
    // метод, которому реально нужен поход в SQLite (per-модуль, не кэшируется).
    index: Mutex<lite_index::LiteIndex>,
    all_names: HashSet<String>,
    global_exports: HashSet<String>,
    /// `None` — база собрана до появления таблицы `objects`: отвечать «не знаю».
    objects: Option<HashMap<String, HashSet<String>>>, // collection -> имена в исходном регистре
    objects_lower: Option<HashMap<String, HashSet<String>>>, // collection -> имена в нижнем регистре
    /// Экспортные переменные модулей приложения (нижний регистр).
    /// `None` — база собрана до появления таблицы `global_vars`.
    global_vars: Option<HashSet<String>>,
    db_path: PathBuf,
}

impl LiteSource {
    pub fn open(db_path: &Path) -> Result<Self> {
        let index = lite_index::LiteIndex::open(db_path)
            .with_context(|| format!("не удалось открыть lite-индекс {}", db_path.display()))?;
        let all_names = index
            .all_method_names()
            .with_context(|| "не удалось прочитать имена методов lite-индекса")?;
        let global_exports = index
            .all_global_export_names()
            .with_context(|| "не удалось прочитать экспорты глобальных модулей lite-индекса")?;
        let objects = index
            .all_objects()
            .with_context(|| "не удалось прочитать объекты lite-индекса")?;
        let objects_lower = objects.as_ref().map(|by_collection| {
            by_collection
                .iter()
                .map(|(collection, names)| {
                    (collection.clone(), names.iter().map(|n| n.to_lowercase()).collect())
                })
                .collect()
        });
        let global_vars = index
            .all_global_var_names()
            .with_context(|| "не удалось прочитать глобальные переменные lite-индекса")?;
        Ok(Self {
            index: Mutex::new(index),
            all_names,
            global_exports,
            objects,
            objects_lower,
            global_vars,
            db_path: db_path.to_path_buf(),
        })
    }
}

impl SymbolSource for LiteSource {
    fn is_global_export(&self, name_lower: &str) -> bool {
        self.global_exports.contains(name_lower)
    }

    fn method_exists(&self, name_lower: &str) -> bool {
        self.all_names.contains(name_lower)
    }

    fn owner_exports(&self, module_path: &str) -> Option<HashSet<String>> {
        match self.index.lock().unwrap().owner_exports(module_path) {
            Ok(names) => Some(names.into_iter().collect()),
            Err(e) => {
                tracing::warn!(error = %e, module_path, "lite-index: ошибка owner_exports");
                None
            }
        }
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        let by_collection = self.objects_lower.as_ref()?;
        match by_collection.get(collection) {
            Some(names) => Some(names.contains(name_lower)),
            // Коллекция не встретилась в выгрузке (например, в УТ нет Sequences) —
            // объектов в ней достоверно нет.
            None => Some(false),
        }
    }

    fn collection_names(&self, collection: &str) -> Option<HashSet<String>> {
        self.objects.as_ref().and_then(|by_collection| by_collection.get(collection).cloned())
    }

    fn global_variables(&self) -> Option<HashSet<String>> {
        self.global_vars.clone()
    }

    fn describe(&self) -> String {
        format!("lite-index: {}", self.db_path.display())
    }
}

// ── CodeIndexDbSource ───────────────────────────────────────────────────────

/// Источник поверх базы MCP-индексатора `code-index` (`<repo>/.code-index/index.db`),
/// только на чтение. Отдельного флага "глобальный общий модуль" база не хранит,
/// но исходный XML общего модуля (`CommonModules/<Имя>.xml`, `<Global>true</Global>`)
/// у неё есть — в `file_contents` (zstd). `global_exports` собирается из него один
/// раз при открытии. `owner_exports` работает отдельно: путь модуля-владельца
/// выводится из пути формы (индекс не нужен), а его экспортные методы — из сигнатуры.
pub struct CodeIndexDbSource {
    /// Имена функций (в нижнем регистре) — SQLite `lower()` не сворачивает
    /// кириллицу, поэтому регистр приводится в Rust один раз при открытии.
    names: HashSet<String>,
    db_path: PathBuf,
    /// Соединение с базой (read-only). Нужно только `owner_exports` — точечный
    /// запрос на модуль; `method_exists` отвечает из памяти.
    conn: Mutex<Connection>,
    /// Экспортные имена методов глобальных общих модулей (нижний регистр).
    /// Собираются при открытии ИЗ САМОЙ БАЗЫ: XML общих модулей лежат в
    /// `file_contents` (zstd), признак — `<Global>true</Global>`. Отдельного
    /// флага у `code-index` нет, но исходный XML он хранит.
    global_exports: HashSet<String>,
    /// Объекты конфигурации по `meta_type` (таблица `metadata_objects`), в
    /// исходном регистре. `None` — таблицы нет: это не BSL-индекс, либо старая
    /// версия без неё. НИКАКОГО вывода имён из путей модулей — у объекта может
    /// не быть ни одного модуля.
    objects: Option<HashMap<String, HashSet<String>>>,
    objects_lower: Option<HashMap<String, HashSet<String>>>,
    /// Экспортные переменные модулей приложения (нижний регистр). Собираются
    /// один раз при открытии из `file_contents` (zstd) — как и `global_exports`.
    global_vars: HashSet<String>,
}

impl CodeIndexDbSource {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("не удалось открыть code-index базу {}", db_path.display()))?;

        let mut stmt = conn
            .prepare("SELECT name FROM functions")
            .with_context(|| "code-index база: не найдена таблица functions")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut names = HashSet::new();
        for row in rows {
            names.insert(row?.to_lowercase());
        }
        drop(stmt);

        let global_exports = Self::collect_global_exports(&conn);
        let objects = Self::collect_objects(&conn);
        let objects_lower = objects.as_ref().map(|by_type| {
            by_type
                .iter()
                .map(|(meta_type, names)| {
                    (meta_type.clone(), names.iter().map(|n| n.to_lowercase()).collect())
                })
                .collect()
        });

        let global_vars = Self::collect_global_vars(&conn);

        Ok(Self {
            names,
            db_path: db_path.to_path_buf(),
            conn: Mutex::new(conn),
            global_exports,
            objects,
            objects_lower,
            global_vars,
        })
    }

    /// Экспортные переменные модулей приложения. Текст модуля лежит в самой
    /// базе (`file_contents`, zstd) — тем же путём, что и XML общих модулей для
    /// `collect_global_exports`. Разбор строк — общий с `lite-index`, чтобы
    /// правило чтения `Перем Имя Экспорт;` жило в одном месте.
    /// Ошибка не фатальна: источник продолжит работать, просто без этих имён.
    fn collect_global_vars(conn: &Connection) -> HashSet<String> {
        Self::try_collect_global_vars(conn).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "code-index база: не удалось собрать переменные модуля приложения");
            HashSet::new()
        })
    }

    fn try_collect_global_vars(conn: &Connection) -> Result<HashSet<String>> {
        let mut stmt = conn.prepare(
            "SELECT f.path, fc.content_blob FROM files f \
             JOIN file_contents fc ON fc.file_id = f.id \
             WHERE f.path LIKE '%ApplicationModule.bsl' OR f.path LIKE '%SessionModule.bsl'",
        )?;
        let rows =
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?;

        let mut out = HashSet::new();
        for row in rows {
            let (path, blob) = row?;
            let file_name = path.rsplit('/').next().unwrap_or(&path);
            if !lite_index::is_application_module_file(file_name) {
                continue;
            }
            let Ok(bytes) = zstd::stream::decode_all(&blob[..]) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes);
            for name in lite_index::global_export_vars_from_text(&content) {
                out.insert(name.to_lowercase());
            }
        }
        Ok(out)
    }

    /// Объекты конфигурации по `meta_type` — из таблицы `metadata_objects`
    /// (BSL-расширение `code-index`). `None` — таблицы нет (не BSL-индекс).
    /// Единственный верный источник имён объектов: у объекта может не быть ни
    /// одного модуля, поэтому вывод имён из `functions`/`files` здесь не годится.
    fn collect_objects(conn: &Connection) -> Option<HashMap<String, HashSet<String>>> {
        let mut stmt = conn.prepare("SELECT meta_type, name FROM metadata_objects").ok()?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .ok()?;
        let mut out: HashMap<String, HashSet<String>> = HashMap::new();
        for (meta_type, name) in rows.flatten() {
            out.entry(meta_type).or_default().insert(name);
        }
        Some(out)
    }

    /// Экспортные имена методов глобальных общих модулей: XML общих модулей
    /// хранится в самой базе (`file_contents`, zstd) — отдельного признака
    /// "глобальный" у `code-index` нет, но исходник модуля есть. Ошибка любого
    /// шага не фатальна: источник продолжит работать, просто без подавления
    /// таких находок.
    fn collect_global_exports(conn: &Connection) -> HashSet<String> {
        Self::try_collect_global_exports(conn).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "code-index база: не удалось собрать экспорты глобальных модулей");
            HashSet::new()
        })
    }

    fn try_collect_global_exports(conn: &Connection) -> Result<HashSet<String>> {
        let mut stmt = conn.prepare(
            "SELECT f.path, fc.content_blob FROM files f \
             JOIN file_contents fc ON fc.file_id = f.id \
             WHERE f.path LIKE '%/CommonModules/%.xml' AND fc.oversize = 0",
        )?;
        let rows =
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?;

        let mut out = HashSet::new();
        for row in rows {
            let (xml_path, blob) = row?;
            let Ok(bytes) = zstd::stream::decode_all(&blob[..]) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes);
            if !content.contains("<Global>true</Global>") {
                continue;
            }
            let Some(module_path) = module_path_from_xml(&xml_path) else {
                continue;
            };
            let mut fn_stmt = conn.prepare(
                "SELECT fn.name FROM functions fn JOIN files fl ON fl.id = fn.file_id \
                 WHERE fl.path = ?1 AND fn.args LIKE '%) Экспорт%'",
            )?;
            let fn_rows = fn_stmt.query_map(params![module_path], |row| row.get::<_, String>(0))?;
            for fn_row in fn_rows {
                out.insert(fn_row?.to_lowercase());
            }
        }
        Ok(out)
    }
}

impl SymbolSource for CodeIndexDbSource {
    /// Экспортный метод глобального общего модуля: зовётся без префикса откуда угодно.
    fn is_global_export(&self, name_lower: &str) -> bool {
        self.global_exports.contains(name_lower)
    }

    fn method_exists(&self, name_lower: &str) -> bool {
        self.names.contains(name_lower)
    }

    /// Экспортные методы модуля объекта-владельца. Путь владельца выводится из
    /// пути формы (индекс не нужен), экспортность — по ключевому слову в
    /// сигнатуре: отдельного флага в базе нет.
    fn owner_exports(&self, module_path: &str) -> Option<HashSet<String>> {
        let owner = lite_index::owner_module_path(module_path)?;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT fn.name FROM functions fn JOIN files fl ON fl.id = fn.file_id \
                 WHERE fl.path = ?1 AND fn.args LIKE '%) Экспорт%'",
            )
            .ok()?;
        let rows = stmt.query_map(params![owner], |row| row.get::<_, String>(0)).ok()?;
        let mut out = HashSet::new();
        for row in rows {
            // Регистр сворачивается в Rust: SQLite lower() кириллицу не берёт.
            out.insert(row.ok()?.to_lowercase());
        }
        Some(out)
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        let meta_type = meta_type_for_collection(collection)?;
        let by_type = self.objects_lower.as_ref()?;
        match by_type.get(meta_type) {
            Some(names) => Some(names.contains(name_lower)),
            None => Some(false),
        }
    }

    fn collection_names(&self, collection: &str) -> Option<HashSet<String>> {
        let meta_type = meta_type_for_collection(collection)?;
        self.objects.as_ref().and_then(|by_type| by_type.get(meta_type).cloned())
    }

    fn global_variables(&self) -> Option<HashSet<String>> {
        Some(self.global_vars.clone())
    }

    fn describe(&self) -> String {
        format!(
            "code-index db: {} ({} имён, {} глобальных экспортов)",
            self.db_path.display(),
            self.names.len(),
            self.global_exports.len()
        )
    }
}

// ── CodeIndexMcpSource ───────────────────────────────────────────────────────

/// Источник через HTTP к живому MCP-серверу `code-index`: рукопожатие
/// (`initialize` + `notifications/initialized`, запоминается `Mcp-Session-Id`)
/// один раз при создании, дальше `method_exists` зовёт `tools/call` инструмента
/// `search_function` и кэширует ответ в памяти — без кэша каждая проверка стоила
/// бы ~13 мс (цена одного MCP-вызова), а `validate_module` проверяет сотни имён
/// за один запрос. `owner_exports` идёт тем же путём через `get_file_summary` и
/// тоже кэшируется — по пути модуля-владельца, а не по имени метода.
///
/// `is_global_export` отвечает: признак читается из XML общего модуля через
/// `read_file`, кэшируется по модулю; лишних сетевых вызовов нет — `search_function`
/// общий с `method_exists` (оба берут находки из `search`).
///
/// Почему не `find_symbol`, хотя он и создан для точного имени: он сравнивает
/// имя посимвольно, а валидатор присылает его в нижнем регистре — и ни одно
/// русское имя не находится (SQLite не сворачивает регистр кириллицы). Поэтому
/// спрашиваем нечёткий `search_function`, а точность обеспечиваем сами.
pub struct CodeIndexMcpSource {
    url: String,
    repo: String,
    agent: ureq::Agent,
    session_id: Mutex<Option<String>>,
    /// Кэш ответов `search_function`: имя → находки. Один сетевой вызов
    /// обслуживает и `method_exists`, и `is_global_export`.
    search_cache: Mutex<std::collections::HashMap<String, Vec<FoundFn>>>,
    /// Кэш «глобальный ли модуль», ключ — путь XML общего модуля.
    global_module_cache: Mutex<std::collections::HashMap<String, bool>>,
    /// Кэш экспортов владельца по пути модуля формы: один вызов `get_file_summary`
    /// на модуль, а не на каждое имя.
    owner_cache: Mutex<std::collections::HashMap<String, HashSet<String>>>,
    /// Кэш объектов конфигурации по коллекциям (нижний регистр). НАБОР ЦЕЛИКОМ
    /// на коллекцию, не по одному имени — иначе первый же вопрос про общие
    /// модули (3091 штука в УТ) ушёл бы отдельным сетевым вызовом на каждое имя.
    objects_cache: Mutex<HashMap<String, HashSet<String>>>,
    /// То же самое в исходном регистре — для `collection_names`.
    objects_cache_orig: Mutex<HashMap<String, HashSet<String>>>,
    /// Экспортные переменные модулей приложения (нижний регистр). `None` —
    /// ещё не запрашивались; запрос один на весь срок жизни источника (замер:
    /// 18 мс), дальше берётся отсюда.
    global_vars_cache: Mutex<Option<HashSet<String>>>,
    /// Источник в рабочем состоянии. Сбрасывается в `false` любой ошибкой транспорта
    /// (см. `is_healthy`) — обратно уже не поднимается: источник пересоздаётся заново.
    healthy: AtomicBool,
}

/// Находка `search_function`: то, что нужно обоим вопросам источника.
#[derive(Clone)]
struct FoundFn {
    name_lower: String,
    args: String,
    file_path: String,
}

impl CodeIndexMcpSource {
    pub fn new(url: String, repo: String, timeout_ms: u64) -> Result<Self> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .build();
        let source = Self {
            url,
            repo,
            agent,
            session_id: Mutex::new(None),
            search_cache: Mutex::new(std::collections::HashMap::new()),
            global_module_cache: Mutex::new(std::collections::HashMap::new()),
            owner_cache: Mutex::new(std::collections::HashMap::new()),
            objects_cache: Mutex::new(HashMap::new()),
            objects_cache_orig: Mutex::new(HashMap::new()),
            global_vars_cache: Mutex::new(None),
            healthy: AtomicBool::new(true),
        };
        source.initialize()?;
        source.ensure_repo_known()?;
        Ok(source)
    }

    /// Рукопожатие MCP: `initialize`, ответ — SSE, интересен только заголовок
    /// `Mcp-Session-Id` (шлётся в последующих запросах).
    fn initialize(&self) -> Result<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "symbol-source", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        let resp = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .send_json(body)
            .with_context(|| format!("code-index mcp: initialize к {} не прошёл", self.url))?;

        if let Some(session_id) = resp.header("Mcp-Session-Id") {
            *self.session_id.lock().unwrap() = Some(session_id.to_string());
        }
        // Тело ответа (SSE) не разбирается — нужен был только заголовок сессии.
        let _ = resp.into_string();

        // Обязательный шаг протокола: без `notifications/initialized` сессия
        // остаётся неинициализированной и сервер отвергает `tools/call`.
        self.notify_initialized()
    }

    /// Уведомление о завершении рукопожатия. Без `id` — ответа по протоколу нет.
    fn notify_initialized(&self) -> Result<()> {
        let body = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let resp = self
            .post(body)
            .with_context(|| "code-index mcp: notifications/initialized не прошло")?;
        let _ = resp.into_string();
        Ok(())
    }

    /// Проверить, что code-index вообще знает этот репозиторий. Без проверки источник
    /// молча отвечал бы «метод не найден» на любое имя, а валидатор — выдавал находку
    /// «метод не объявлен» на каждый вызов процедуры. Лучше внятный отказ на старте.
    ///
    /// Спрашиваем статистику ИМЕННО по своему репозиторию (`get_stats(repo)`) — это 14 мс.
    /// Полный `get_stats()` без аргументов считает статистику по всем репозиториям сразу:
    /// на холодную это больше 5 секунд, и проверка сама себя роняла по таймауту (замер
    /// 2026-07-13). На незнакомое имя code-index отвечает «Неизвестный repo …» и тут же
    /// перечисляет доступные — отдельный запрос за списком не нужен.
    fn ensure_repo_known(&self) -> Result<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "get_stats",
                "arguments": {"repo": self.repo}
            }
        });
        let resp = self
            .post(body)
            .with_context(|| format!("code-index mcp: get_stats к {} не прошёл", self.url))?;
        let text = resp.into_string().context("code-index mcp: тело ответа")?;
        let value = parse_sse_json(&text)
            .ok_or_else(|| anyhow::anyhow!("code-index mcp: пустой/неразбираемый SSE-ответ"))?;
        match repo_check_from_get_stats(&value, &self.repo) {
            RepoCheck::Known => Ok(()),
            RepoCheck::Unknown(message) => {
                anyhow::bail!("code-index по адресу {}: {}", self.url, message)
            }
            // Форма ответа может смениться в новой версии code-index — это чужой продукт.
            // Не распознали — не блокируем старт, просто не проверяем.
            RepoCheck::Unrecognized => {
                tracing::warn!(
                    url = %self.url,
                    repo = %self.repo,
                    "code-index mcp: ответ get_stats не распознан, проверка репозитория пропущена"
                );
                Ok(())
            }
        }
    }

    /// POST к `/mcp` с заголовками и сессией. Общая часть всех вызовов.
    fn post(&self, body: Value) -> Result<ureq::Response, ureq::Error> {
        let mut req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        if let Some(session_id) = self.session_id.lock().unwrap().clone() {
            req = req.set("Mcp-Session-Id", &session_id);
        }
        req.send_json(body)
    }

    /// Находки `search_function` по имени (кэшируются). Поиск нечёткий —
    /// точность обеспечивают вызывающие, сверяя `name_lower`.
    ///
    /// `find_symbol` не годится: он сравнивает имя точно, а SQLite не сворачивает
    /// регистр кириллицы — валидатор же присылает имя в нижнем регистре, и любое
    /// русское имя «не находилось». Берём `search_function` (нечёткий поиск,
    /// регистронезависимый), а точность возвращаем сами — сверяем имена находок.
    fn search(&self, name_lower: &str) -> Vec<FoundFn> {
        if let Some(cached) = self.search_cache.lock().unwrap().get(name_lower) {
            return cached.clone();
        }
        match self.call_search(name_lower) {
            Ok(found) => {
                self.search_cache
                    .lock()
                    .unwrap()
                    .insert(name_lower.to_string(), found.clone());
                found
            }
            Err(e) => {
                // Пустой результат НЕ кэшируется: иначе одна сетевая ошибка отравляет
                // кэш до перезапуска сервера, и валидатор до конца сессии считает,
                // что метода нигде не существует.
                tracing::warn!(error = %e, name = name_lower, "code-index mcp: ошибка search_function");
                self.healthy.store(false, Ordering::Relaxed);
                Vec::new()
            }
        }
    }

    fn call_search(&self, name_lower: &str) -> Result<Vec<FoundFn>> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "search_function",
                "arguments": {"repo": self.repo, "query": name_lower, "limit": 20}
            }
        });
        let resp = self
            .post(body)
            .with_context(|| format!("code-index mcp: search_function({name_lower}) не прошёл"))?;
        let text = resp.into_string().context("code-index mcp: тело ответа")?;
        let value = parse_sse_json(&text)
            .ok_or_else(|| anyhow::anyhow!("code-index mcp: пустой/неразбираемый SSE-ответ"))?;
        Ok(found_fns_from_search(&value))
    }

    /// Глобальный ли общий модуль. XML читается у самого code-index
    /// (`read_file`) и кэшируется: файл маленький, но вызов сетевой.
    fn module_is_global(&self, xml_path: &str) -> bool {
        if let Some(cached) = self.global_module_cache.lock().unwrap().get(xml_path) {
            return *cached;
        }
        match self.call_module_is_global(xml_path) {
            Ok(is_global) => {
                self.global_module_cache
                    .lock()
                    .unwrap()
                    .insert(xml_path.to_string(), is_global);
                is_global
            }
            Err(e) => {
                tracing::warn!(error = %e, xml_path, "code-index mcp: ошибка read_file общего модуля");
                self.healthy.store(false, Ordering::Relaxed);
                false
            }
        }
    }

    fn call_module_is_global(&self, xml_path: &str) -> Result<bool> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "read_file",
                "arguments": {"repo": self.repo, "path": xml_path}
            }
        });
        let resp = self
            .post(body)
            .with_context(|| format!("code-index mcp: read_file({xml_path}) не прошёл"))?;
        let text = resp.into_string().context("code-index mcp: тело ответа")?;
        let value = parse_sse_json(&text)
            .ok_or_else(|| anyhow::anyhow!("code-index mcp: пустой/неразбираемый SSE-ответ"))?;
        Ok(xml_says_global(&value))
    }

    /// `get_file_summary(repo, path)` — карта файла без тел. Возвращает имена
    /// экспортных функций (нижний регистр): экспортность видна в поле `args`
    /// сигнатуры (`"() Экспорт"`).
    fn call_owner_exports(&self, owner_path: &str) -> Result<HashSet<String>> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_file_summary",
                "arguments": {"repo": self.repo, "path": owner_path}
            }
        });
        let resp = self
            .post(body)
            .with_context(|| format!("code-index mcp: get_file_summary({owner_path}) не прошёл"))?;
        let text = resp.into_string().context("code-index mcp: тело ответа")?;
        let value = parse_sse_json(&text)
            .ok_or_else(|| anyhow::anyhow!("code-index mcp: пустой/неразбираемый SSE-ответ"))?;
        let text = value
            .pointer("/result/content/0/text")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("code-index mcp: get_file_summary без result.content[0].text"))?;
        let summary: Value =
            serde_json::from_str(text).context("code-index mcp: get_file_summary: не JSON")?;
        Ok(export_names_from_summary(&summary))
    }

    /// Набор имён объектов конфигурации коллекции (нижний регистр), из кэша
    /// или сетевым запросом `bsl_sql`. Кэшируется НАБОРОМ ЦЕЛИКОМ по коллекции.
    fn objects_for_collection(&self, collection: &str, meta_type: &str) -> Option<HashSet<String>> {
        if let Some(cached) = self.objects_cache.lock().unwrap().get(collection) {
            return Some(cached.clone());
        }
        match self.call_objects(meta_type) {
            Ok(Some((lower, orig))) => {
                self.objects_cache.lock().unwrap().insert(collection.to_string(), lower.clone());
                self.objects_cache_orig.lock().unwrap().insert(collection.to_string(), orig);
                Some(lower)
            }
            // truncated=true: обрезанному набору доверять нельзя — каждый необрезанный
            // объект стал бы ложной находкой «объекта не существует».
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, collection, "code-index mcp: ошибка bsl_sql metadata_objects");
                self.healthy.store(false, Ordering::Relaxed);
                None
            }
        }
    }

    /// Имена объектов коллекции по `meta_type` — СТРАНИЦАМИ.
    ///
    /// Три неочевидных условия, каждое стоило ложных находок на живом коде.
    /// Важно: `code-index` про обе обрезки сообщает ЧЕСТНО и подробно — ошибки
    /// были в этом источнике, который его пометок не читал.
    ///
    /// 1. **Страницы, а не один запрос.** `bsl_sql` входит в `DEFAULT_CAP_TOOLS`
    ///    у `code-index`: его ответ проходит через `cap_response` с бюджетом
    ///    `[mcp].max_response_bytes` (дефолт 48 000 байт). Список общих модулей
    ///    УТ (3091 имя) весит ~170 КБ, и страж ОПОЛОВИНИВАЕТ массив, пока тот не
    ///    влезет: приходит 386 строк из 3091 — по алфавиту до «И». Об этом
    ///    сообщается четырьмя способами (`rows_total`, `rows_truncated`,
    ///    `response_truncated` + человекочитаемый `response_truncated_hint`), а
    ///    поле `truncated` — про СОБСТВЕННЫЙ лимит `bsl_sql` и правдиво отвечает
    ///    `false`: читать надо не его. Забрать набор одним запросом нельзя в
    ///    принципе. Страница по `PAGE` строк укладывается в бюджет; идём по
    ///    `OFFSET`, пока страница полная.
    /// 2. **`full_name`, а не `name`.** У `serve` есть сессионный дедуп
    ///    (`serve_dedup.rs`): он считает отпечаток каждой СТРОКИ и опускает уже
    ///    отданные в этой сессии, ЯВНО помечая это полем
    ///    `rows_elided_already_delivered`. Источник живёт одной сессией, а имена
    ///    пересекаются между коллекциями (в УТ `Закупки` — и общий модуль, и
    ///    регистр накопления; `ПодарочныеСертификаты` — и регистр, и справочник).
    ///    Со `SELECT name` строка `["Закупки"]` во втором запросе была бы
    ///    опущена, и реальный объект получил бы `Some(false)`. `full_name`
    ///    уникален глобально — опускать нечего.
    /// 3. **`ORDER BY`** — без него порядок строк между страницами не определён и
    ///    `OFFSET` пропустит или задвоит имена.
    fn call_objects(&self, meta_type: &str) -> Result<Option<(HashSet<String>, HashSet<String>)>> {
        /// Строк на страницу. 400 × ~55 байт ≈ 22 КБ — вдвое ниже дефолтного
        /// бюджета `cap_response` (48 000). Запас на случай, если бюджет на
        /// сервере окажется ниже дефолта: страница всё равно проверяется на
        /// обрезку, и при ней набор признаётся недостоверным.
        const PAGE: usize = 400;

        let mut lower = HashSet::new();
        let mut orig = HashSet::new();
        let mut offset = 0usize;
        loop {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "bsl_sql",
                    "arguments": {
                        "repo": self.repo,
                        "sql": "SELECT full_name FROM metadata_objects WHERE meta_type = ?1 \
                                ORDER BY full_name LIMIT ?2 OFFSET ?3",
                        "params": [meta_type, PAGE, offset],
                        "limit": PAGE + 1
                    }
                }
            });
            let resp = self.post(body).with_context(|| {
                format!("code-index mcp: bsl_sql(metadata_objects, {meta_type}, offset={offset}) не прошёл")
            })?;
            let text = resp.into_string().context("code-index mcp: тело ответа")?;
            let value = parse_sse_json(&text)
                .ok_or_else(|| anyhow::anyhow!("code-index mcp: пустой/неразбираемый SSE-ответ"))?;
            let Some(page) = objects_from_bsl_sql(&value) else {
                // Страница недостоверна (обрезана/дедуплицирована) — весь набор
                // под вопросом. Лучше молчание, чем ложные находки.
                return Ok(None);
            };
            let received = page.received;
            lower.extend(page.lower);
            orig.extend(page.orig);
            if received < PAGE {
                break;
            }
            offset += PAGE;
        }
        Ok(Some((lower, orig)))
    }

    /// Экспортные переменные модулей приложения — ОДИН запрос `grep_code` за
    /// строками объявлений. Модули целиком читать не нужно: интересны только
    /// строки `Перем ... Экспорт`, их единицы (замер на УТ: 18 мс, 959 байт).
    /// Раскладка `<...>/Ext/<Имя>ApplicationModule.bsl` проверена на УТ и БП.
    fn call_global_vars(&self) -> Result<HashSet<String>> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "grep_code",
                "arguments": {
                    "repo": self.repo,
                    // Ловим ВСЕ объявления `Перем`; экспортные отберёт общий разбор.
                    "regex": r"(?m)^\s*Перем\s",
                    "path_glob": "**/Ext/{ManagedApplicationModule,OrdinaryApplicationModule,SessionModule,ExternalConnectionModule}.bsl",
                    "limit": 500
                }
            }
        });
        let resp = self
            .post(body)
            .with_context(|| "code-index mcp: grep_code(модули приложения) не прошёл")?;
        let text = resp.into_string().context("code-index mcp: тело ответа")?;
        let value = parse_sse_json(&text)
            .ok_or_else(|| anyhow::anyhow!("code-index mcp: пустой/неразбираемый SSE-ответ"))?;
        Ok(global_vars_from_grep(&value))
    }
}

impl SymbolSource for CodeIndexMcpSource {
    /// Экспортный метод глобального общего модуля: зовётся без префикса откуда угодно.
    fn is_global_export(&self, name_lower: &str) -> bool {
        self.search(name_lower).into_iter().any(|f| {
            f.name_lower == name_lower
                && f.args.contains(") Экспорт")
                && f.file_path.contains("/CommonModules/")
                && common_module_xml_path(&f.file_path)
                    .is_some_and(|xml_path| self.module_is_global(&xml_path))
        })
    }

    fn method_exists(&self, name_lower: &str) -> bool {
        self.search(name_lower)
            .iter()
            .any(|f| f.name_lower == name_lower)
    }

    fn owner_exports(&self, module_path: &str) -> Option<HashSet<String>> {
        let owner = lite_index::owner_module_path(module_path)?;
        if let Some(cached) = self.owner_cache.lock().unwrap().get(&owner) {
            return Some(cached.clone());
        }
        match self.call_owner_exports(&owner) {
            Ok(names) => {
                self.owner_cache.lock().unwrap().insert(owner, names.clone());
                Some(names)
            }
            Err(e) => {
                tracing::warn!(error = %e, owner = %owner, "code-index mcp: ошибка get_file_summary");
                self.healthy.store(false, Ordering::Relaxed);
                Some(HashSet::new())
            }
        }
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        if !self.is_healthy() {
            return None;
        }
        let meta_type = meta_type_for_collection(collection)?;
        let names = self.objects_for_collection(collection, meta_type)?;
        Some(names.contains(name_lower))
    }

    fn collection_names(&self, collection: &str) -> Option<HashSet<String>> {
        if !self.is_healthy() {
            return None;
        }
        let meta_type = meta_type_for_collection(collection)?;
        self.objects_for_collection(collection, meta_type)?;
        self.objects_cache_orig.lock().unwrap().get(collection).cloned()
    }

    fn global_variables(&self) -> Option<HashSet<String>> {
        if !self.is_healthy() {
            return None;
        }
        if let Some(cached) = self.global_vars_cache.lock().unwrap().as_ref() {
            return Some(cached.clone());
        }
        match self.call_global_vars() {
            Ok(names) => {
                *self.global_vars_cache.lock().unwrap() = Some(names.clone());
                Some(names)
            }
            // Ошибку сети НЕ кэшируем и роняем healthy: пустой набор здесь
            // означал бы «таких переменных нет» и вернул бы ложные находки.
            Err(e) => {
                tracing::warn!(error = %e, "code-index mcp: ошибка grep_code за переменными модуля приложения");
                self.healthy.store(false, Ordering::Relaxed);
                None
            }
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    fn describe(&self) -> String {
        format!("code-index mcp: {} repo={}", self.url, self.repo)
    }
}

/// Разобрать SSE-ответ MCP-сервера: строки `data: {...}`, первая бывает
/// пустой. Берётся первая непустая строка `data:` и парсится как JSON; если
/// `data:`-строк нет вовсе — тело пробуется как обычный JSON (на случай
/// не-SSE ответа).
pub fn parse_sse_json(body: &str) -> Option<Value> {
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str(rest) {
            return Some(value);
        }
    }
    serde_json::from_str(body.trim()).ok()
}

/// `<...>/CommonModules/<Имя>.xml` → `<...>/CommonModules/<Имя>/Ext/Module.bsl`.
fn module_path_from_xml(xml_path: &str) -> Option<String> {
    let base = xml_path.strip_suffix(".xml")?;
    Some(format!("{base}/Ext/Module.bsl"))
}

/// `<...>/CommonModules/<Имя>/Ext/Module.bsl` → `<...>/CommonModules/<Имя>.xml`
fn common_module_xml_path(module_path: &str) -> Option<String> {
    let base = module_path.strip_suffix("/Ext/Module.bsl")?;
    Some(format!("{base}.xml"))
}

/// Коллекция каталога выгрузки → `meta_type` в code-index (единственное
/// число, английское). Неизвестная коллекция → `None`.
fn meta_type_for_collection(collection: &str) -> Option<&'static str> {
    match collection {
        "CommonModules" => Some("CommonModule"),
        "Catalogs" => Some("Catalog"),
        "Documents" => Some("Document"),
        "InformationRegisters" => Some("InformationRegister"),
        "AccumulationRegisters" => Some("AccumulationRegister"),
        "AccountingRegisters" => Some("AccountingRegister"),
        "CalculationRegisters" => Some("CalculationRegister"),
        "Enums" => Some("Enum"),
        // Три плана — единственный случай, где `meta_type` остаётся во
        // множественном числе (проверено: `SELECT DISTINCT meta_type` даёт
        // `ChartOfCharacteristicTypes` на УТ, `ChartOfAccounts` на БП).
        // Приведение к единственному числу «для единообразия» превращает
        // каждое обращение `ПланыВидовХарактеристик.Х` в ложную находку.
        "ChartsOfCharacteristicTypes" => Some("ChartOfCharacteristicTypes"),
        "ChartsOfAccounts" => Some("ChartOfAccounts"),
        "ChartsOfCalculationTypes" => Some("ChartOfCalculationTypes"),
        "BusinessProcesses" => Some("BusinessProcess"),
        "Tasks" => Some("Task"),
        "ExchangePlans" => Some("ExchangePlan"),
        "Constants" => Some("Constant"),
        "DataProcessors" => Some("DataProcessor"),
        "Reports" => Some("Report"),
        "DocumentJournals" => Some("DocumentJournal"),
        "FilterCriteria" => Some("FilterCriterion"),
        "Sequences" => Some("Sequence"),
        _ => None,
    }
}

/// Одна страница ответа `bsl_sql` со списком объектов.
struct ObjectPage {
    /// Имена в нижнем регистре (для поиска).
    lower: HashSet<String>,
    /// Имена в исходном регистре (для подсказок).
    orig: HashSet<String>,
    /// Сколько строк реально пришло — по нему вызывающий понимает, была ли
    /// страница последней. Не длина `lower`: одинаковые имена схлопнутся.
    received: usize,
}

/// Разбор страницы `bsl_sql`: `{"columns":[...],"rows":[["Catalog.Имя"],...]}`.
/// Префикс типа снимается — наружу отдаются имена объектов.
///
/// Набору нельзя доверять в трёх случаях, каждый → `None` (валидатор промолчит):
/// - `rows_truncated` — страж размера ответа `code-index` (`cap_response`,
///   бюджет `[mcp].max_response_bytes`) ополовинил массив строк. ВНИМАНИЕ: поле
///   `truncated` про эту обрезку НЕ знает, оно про собственный лимит `bsl_sql`.
///   Проверять только `truncated` — значит принять 386 строк из 3091 за полный
///   список и объявить 2705 реальных модулей несуществующими;
/// - `truncated=true` — ответ обрезан собственным лимитом инструмента;
/// - `rows_elided_already_delivered` — сессионный дедуп опустил строки, уже
///   отданные в этой сессии (см. `call_objects`).
fn objects_from_bsl_sql(value: &Value) -> Option<ObjectPage> {
    let text = value.pointer("/result/content/0/text")?.as_str()?;
    let parsed: Value = serde_json::from_str(text).ok()?;
    // Маркер стража размера живёт на ВЕРХНЕМ уровне обёртки, рядом с `result`.
    if parsed.get("response_truncated").is_some() {
        return None;
    }
    let body = payload(&parsed);
    if body.get("rows_truncated").is_some() {
        return None;
    }
    if body.get("truncated").and_then(|t| t.as_bool()) == Some(true) {
        return None;
    }
    if body.get("rows_elided_already_delivered").is_some() {
        return None;
    }
    let rows = body.get("rows")?.as_array()?;
    let mut lower = HashSet::new();
    let mut orig = HashSet::new();
    for row in rows {
        let Some(full_name) = row.as_array().and_then(|r| r.first()).and_then(|n| n.as_str())
        else {
            continue;
        };
        // `Catalog.Номенклатура` → `Номенклатура`. Точка в имени объекта 1С
        // невозможна, поэтому первого разделителя достаточно.
        let name = full_name
            .split_once('.')
            .map(|(_, name)| name)
            .unwrap_or(full_name);
        lower.insert(name.to_lowercase());
        orig.insert(name.to_string());
    }
    Some(ObjectPage {
        lower,
        orig,
        received: rows.len(),
    })
}

/// Разбор ответа `grep_code`: `{"files": {"<path>": ["12: <строка>", ...]}}`.
/// Номер строки отрезаем — дальше работает разбор объявления, общий с
/// `lite-index`: правило чтения `Перем Имя Экспорт;` должно жить в одном месте.
fn global_vars_from_grep(value: &Value) -> HashSet<String> {
    let Some(text) = value
        .pointer("/result/content/0/text")
        .and_then(|t| t.as_str())
    else {
        return HashSet::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return HashSet::new();
    };
    let body = payload(&parsed);
    let Some(files) = body.get("files").and_then(|f| f.as_object()) else {
        return HashSet::new();
    };
    let mut lines = String::new();
    for entries in files.values() {
        for entry in entries.as_array().into_iter().flatten() {
            let Some(raw) = entry.as_str() else {
                continue;
            };
            // Формат строки — "<номер>: <содержимое>".
            let code = raw.split_once(": ").map(|(_, rest)| rest).unwrap_or(raw);
            lines.push_str(code);
            lines.push('\n');
        }
    }
    lite_index::global_export_vars_from_text(&lines)
        .into_iter()
        .map(|n| n.to_lowercase())
        .collect()
}

/// Полезная нагрузка ответа `code-index`. С версии 0.9 инструменты заворачивают
/// её в `{"result": {...}, "hint": ..., "truncated": ...}`; более ранние отдавали
/// объект напрямую. Разворачиваем оба варианта — иначе `functions` не находятся
/// и источник молча отвечает «имени нет» на любое имя.
fn payload(value: &Value) -> &Value {
    value.get("result").unwrap_or(value)
}

/// Итог проверки репозитория по ответу `get_stats(repo)`.
enum RepoCheck {
    /// code-index знает этот репозиторий.
    Known,
    /// code-index явно ответил «Неизвестный repo …» — в его сообщении уже перечислены
    /// доступные имена, поэтому текст берём целиком.
    Unknown(String),
    /// Форма ответа не распознана — блокировать старт по этому поводу нельзя.
    Unrecognized,
}

/// Разбор ответа `get_stats(repo=…)`. Формы подтверждены на живом сервере:
/// известный репозиторий — `{"repo":"ut-test","db":{...},"daemon":{...}}`;
/// неизвестный — `{"status":"not_started","message":"Неизвестный repo 'x'. Доступные: [...]"}`.
///
/// Репозиторий, который известен, но ещё не проиндексирован, проверку ПРОХОДИТ:
/// он объявлен в конфиге code-index, просто не готов — это не повод не подключаться.
fn repo_check_from_get_stats(value: &Value, repo: &str) -> RepoCheck {
    let Some(text) = value.pointer("/result/content/0/text").and_then(|t| t.as_str()) else {
        return RepoCheck::Unrecognized;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return RepoCheck::Unrecognized;
    };
    let body = payload(&parsed);
    if body.get("repo").and_then(|r| r.as_str()) == Some(repo) {
        return RepoCheck::Known;
    }
    match body.get("message").and_then(|m| m.as_str()) {
        Some(msg) if msg.contains("Неизвестный repo") => RepoCheck::Unknown(msg.to_string()),
        _ => RepoCheck::Unrecognized,
    }
}

/// Имена экспортных функций из ответа `get_file_summary` (нижний регистр).
///
/// Экспортность видна в сигнатуре: ключевое слово идёт сразу за закрывающей
/// скобкой (`"() Экспорт"`). Тот же критерий, что у SQL-варианта
/// (`args LIKE '%) Экспорт%'`) — проверка по `" Экспорт"` поймала бы и параметр
/// с таким именем.
fn export_names_from_summary(summary: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(functions) = payload(summary).get("functions").and_then(|f| f.as_array()) else {
        return out;
    };
    for func in functions {
        let Some(args) = func.get("args").and_then(|a| a.as_str()) else {
            continue;
        };
        if args.contains(") Экспорт") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                out.insert(name.to_lowercase());
            }
        }
    }
    out
}

/// Разбор ответа `search_function`: `result` — массив локаций.
fn found_fns_from_search(value: &Value) -> Vec<FoundFn> {
    let Some(text) = value.pointer("/result/content/0/text").and_then(|t| t.as_str()) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(items) = payload(&parsed).as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| FoundFn {
            name_lower: item
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_lowercase(),
            args: item
                .get("args")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string(),
            file_path: item
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

/// В ответе `read_file` содержимое лежит в `content`. Ищем `<Global>true</Global>`.
fn xml_says_global(value: &Value) -> bool {
    let Some(text) = value.pointer("/result/content/0/text").and_then(|t| t.as_str()) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    payload(&parsed)
        .get("content")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.contains("<Global>true</Global>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── формат ответа code-index ─────────────────────────────────────────

    /// Ответ `tools/call`: полезная нагрузка — JSON-строка в `result.content[0].text`.
    fn tool_response(payload: &str) -> Value {
        serde_json::from_str(&format!(
            r#"{{"result":{{"content":[{{"type":"text","text":{}}}]}}}}"#,
            serde_json::to_string(payload).unwrap()
        ))
        .unwrap()
    }

    #[test]
    fn found_fns_parsed_from_search_response() {
        let v = tool_response(r#"{"result":[{"name":"Ф","args":"() Экспорт","file_path":"base/CommonModules/М/Ext/Module.bsl"}]}"#);
        let fns = found_fns_from_search(&v);
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name_lower, "ф");
        assert!(fns[0].args.contains("Экспорт"));
    }

    #[test]
    fn fuzzy_hit_is_not_an_exact_name_match() {
        // `search_function` ищет нечётко и вернёт похожее имя. Принять его за
        // искомое нельзя: опечатка сойдёт за объявленный метод и загасит находку.
        let v = tool_response(
            r#"{"result":[{"name":"СведенияОВнешнейОбработкеДопустимой","args":"() Экспорт","file_path":"base/CommonModules/М/Ext/Module.bsl"}]}"#,
        );
        let fns = found_fns_from_search(&v);
        assert!(!fns.iter().any(|f| f.name_lower == "сведенияовнешнейобработке"));
        assert!(fns.iter().any(|f| f.name_lower == "сведенияовнешнейобработкедопустимой"));
    }

    #[test]
    fn repo_check_known_repo() {
        let v = tool_response(
            r#"{"repo":"ut-test","path":"C:/RepoUT-test","db":{"total_functions":261548}}"#,
        );
        assert!(matches!(
            repo_check_from_get_stats(&v, "ut-test"),
            RepoCheck::Known
        ));
    }

    #[test]
    fn repo_check_unknown_repo_carries_available_list() {
        let v = tool_response(
            r#"{"status":"not_started","message":"Неизвестный repo 'нет-такого'. Доступные: [\"ut\", \"wms\"]"}"#,
        );
        match repo_check_from_get_stats(&v, "нет-такого") {
            RepoCheck::Unknown(msg) => {
                assert!(msg.contains("ut"), "в сообщении должен быть список доступных: {msg}");
            }
            _ => panic!("неизвестный репозиторий должен быть распознан как Unknown"),
        }
    }

    #[test]
    fn repo_check_unrecognized_shape_does_not_block() {
        let v = tool_response(r#"{"something_else": true}"#);
        assert!(matches!(
            repo_check_from_get_stats(&v, "ut"),
            RepoCheck::Unrecognized
        ));
    }

    #[test]
    fn xml_says_global_reads_flag() {
        assert!(xml_says_global(&tool_response(r#"{"result":{"content":"<Properties><Global>true</Global></Properties>"}}"#)));
        assert!(!xml_says_global(&tool_response(r#"{"result":{"content":"<Properties><Global>false</Global></Properties>"}}"#)));
    }

    #[test]
    fn module_path_from_xml_and_back() {
        assert_eq!(
            module_path_from_xml("base/CommonModules/М.xml").as_deref(),
            Some("base/CommonModules/М/Ext/Module.bsl")
        );
        assert_eq!(module_path_from_xml("base/CommonModules/М.txt"), None);
        assert_eq!(
            common_module_xml_path("base/CommonModules/М/Ext/Module.bsl").as_deref(),
            Some("base/CommonModules/М.xml")
        );
        assert_eq!(
            common_module_xml_path("base/Documents/Заказ/Ext/ObjectModule.bsl"),
            None
        );
    }

    #[test]
    fn export_names_taken_from_wrapped_summary_and_lowercased() {
        let v: Value = serde_json::from_str(
            r#"{"result":{"functions":[
                {"name":"СведенияОВнешнейОбработке","args":"() Экспорт"},
                {"name":"ВыполнитьОбмен","args":"(Отказ, Лог = \"\") Экспорт"},
                {"name":"Внутренняя","args":"(Парам)"},
                {"name":"ЛовушкаПараметра","args":"(Знач Экспорт)"}
            ]}}"#,
        )
        .unwrap();
        let names = export_names_from_summary(&v);
        assert!(names.contains("сведенияовнешнейобработке"));
        assert!(names.contains("выполнитьобмен"));
        assert!(!names.contains("внутренняя"));
        // Параметр с именем «Экспорт» не делает функцию экспортной.
        assert!(!names.contains("ловушкапараметра"));
        assert_eq!(names.len(), 2);
    }

    /// Разбор страницы `bsl_sql`: префикс типа снимается, имя остаётся.
    #[test]
    fn objects_parsed_from_full_name_rows() {
        let value = tool_response(
            r#"{"result":{"columns":["full_name"],"rows":[["Catalog.Номенклатура"],["Catalog.Валюты"]],"truncated":false}}"#,
        );
        let page = objects_from_bsl_sql(&value).expect("страница должна разобраться");
        assert!(page.lower.contains("номенклатура"));
        assert!(page.orig.contains("Номенклатура"));
        assert!(
            !page.orig.contains("Catalog.Номенклатура"),
            "префикс типа обязан быть снят"
        );
        assert_eq!(page.received, 2);
    }

    /// Страж размера ответа `code-index` (`cap_response`) ополовинил массив.
    /// Это ГЛАВНАЯ ловушка: рядом стоит `truncated:false`, и проверка только по
    /// нему принимает 386 строк из 3091 за полный список — все остальные
    /// реальные объекты становятся ложными находками. Замечено на живом сервере:
    /// `ОбщегоНазначения` объявлялся несуществующим.
    #[test]
    fn cap_truncated_rows_make_page_untrusted() {
        let value = tool_response(
            r#"{"result":{"columns":["full_name"],"rows":[["Catalog.Валюты"]],"rows_total":3091,"rows_truncated":true,"truncated":false}}"#,
        );
        assert!(objects_from_bsl_sql(&value).is_none());
    }

    /// Тот же страж, но маркер на верхнем уровне обёртки.
    #[test]
    fn response_truncated_marker_makes_page_untrusted() {
        let value = tool_response(
            r#"{"result":{"columns":["full_name"],"rows":[["Catalog.Валюты"]],"truncated":false},"response_truncated":true}"#,
        );
        assert!(objects_from_bsl_sql(&value).is_none());
    }

    /// Сессионный дедуп `code-index` опустил строки → набор неполон, доверять
    /// ему нельзя: объявить существующий объект несуществующим хуже, чем
    /// промолчать. Запрос по `full_name` до этого доводить не должен, но защита
    /// обязана остаться.
    #[test]
    fn elided_rows_make_object_set_untrusted() {
        let value = tool_response(
            r#"{"result":{"columns":["full_name"],"rows":[["Catalog.Валюты"]],"rows_elided_already_delivered":3}}"#,
        );
        assert!(objects_from_bsl_sql(&value).is_none());
    }

    /// Обрезка собственным лимитом `bsl_sql` — то же самое.
    #[test]
    fn truncated_rows_make_object_set_untrusted() {
        let value = tool_response(
            r#"{"result":{"columns":["full_name"],"rows":[["Catalog.Валюты"]],"truncated":true}}"#,
        );
        assert!(objects_from_bsl_sql(&value).is_none());
    }

    #[test]
    fn meta_type_for_collection_known_and_unknown() {
        assert_eq!(meta_type_for_collection("CommonModules"), Some("CommonModule"));
        assert_eq!(meta_type_for_collection("Catalogs"), Some("Catalog"));
        assert_eq!(meta_type_for_collection("Enums"), Some("Enum"));
        assert_eq!(meta_type_for_collection("НеизвестнаяКоллекция"), None);
    }

    /// Три плана — исключение из правила «meta_type в единственном числе».
    /// Значения сверены с живым индексом (`SELECT DISTINCT meta_type`): на УТ
    /// есть `ChartOfCharacteristicTypes`, на БП — `ChartOfAccounts`. Единственное
    /// число здесь дало бы `Some(false)` на КАЖДОМ обращении к плану — ложную
    /// находку на штатном коде.
    #[test]
    fn meta_type_for_charts_stays_plural() {
        assert_eq!(
            meta_type_for_collection("ChartsOfCharacteristicTypes"),
            Some("ChartOfCharacteristicTypes")
        );
        assert_eq!(
            meta_type_for_collection("ChartsOfAccounts"),
            Some("ChartOfAccounts")
        );
        assert_eq!(
            meta_type_for_collection("ChartsOfCalculationTypes"),
            Some("ChartOfCalculationTypes")
        );
    }

    // ── parse_sse_json ───────────────────────────────────────────────────

    #[test]
    fn parse_sse_json_skips_empty_first_line() {
        let body = "data:\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        let value = parse_sse_json(body).expect("должен распарситься второй data:");
        assert_eq!(value["jsonrpc"], "2.0");
    }

    #[test]
    fn parse_sse_json_falls_back_to_plain_json() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}";
        let value = parse_sse_json(body).expect("должен распарситься как обычный JSON");
        assert_eq!(value["id"], 1);
    }

    #[test]
    fn parse_sse_json_returns_none_on_garbage() {
        assert!(parse_sse_json("не json и не SSE").is_none());
    }

    // ── CodeIndexDbSource ─────────────────────────────────────────────────

    #[test]
    fn code_index_db_source_finds_cyrillic_name_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("index.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE TABLE functions (name TEXT NOT NULL)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO functions (name) VALUES (?1)",
            ["ДатаСообщенияEDI"],
        )
        .unwrap();
        drop(conn);

        let source = CodeIndexDbSource::open(&db_path).unwrap();
        assert!(source.method_exists("датасообщенияedi"));
        assert!(!source.method_exists("несуществующийметод"));
        assert!(!source.is_global_export("датасообщенияedi"));
        assert!(source.describe().contains("index.db"));
    }

    // ── LiteSource ────────────────────────────────────────────────────────

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn lite_source_reports_global_export_method_exists_and_owner_exports() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("upload");

        // Глобальный общий модуль.
        write_file(
            &root,
            "base/CommonModules/Гло/Ext/Module.bsl",
            "Процедура ИмяИзГло() Экспорт\nКонецПроцедуры\n",
        );
        write_file(
            &root,
            "base/CommonModules/Гло.xml",
            "<?xml version=\"1.0\"?>\n<MetaDataObject><CommonModule><Properties><Name>Гло</Name><Global>true</Global></Properties></CommonModule></MetaDataObject>\n",
        );

        // Внешняя обработка: модуль объекта и модуль формы.
        write_file(
            &root,
            "external/Обр/ExternalDataProcessor.obj.bsl",
            "Процедура ЭкспортныйМетодОбр() Экспорт\nКонецПроцедуры\n",
        );
        write_file(
            &root,
            "external/Обр/Form/Ф/Form.obj.bsl",
            "&НаКлиенте\nПроцедура ПриОткрытии(Отказ)\nКонецПроцедуры\n",
        );

        let db_path = tmp.path().join("lite.db");
        lite_index::build(&root, &db_path, 0).unwrap();

        let source = LiteSource::open(&db_path).unwrap();
        assert!(source.is_global_export("имяизгло"));
        assert!(!source.is_global_export("экспортныйметодобр"));
        assert!(source.method_exists("экспортныйметодобр"));
        assert!(!source.method_exists("несуществующийметод"));

        let owner = source
            .owner_exports("external/Обр/Form/Ф/Form.obj.bsl")
            .expect("owner_exports должен вернуть набор");
        assert!(owner.contains("экспортныйметодобр"));
        assert!(source.describe().contains("lite.db"));
    }

    #[test]
    fn lite_source_reports_object_exists_and_collection_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("upload");

        // Перечисление без единого модуля — как большинство перечислений в УТ
        // (909 из 1069). Единственный верный источник имени — XML, не modules.
        write_file(
            &root,
            "base/Enums/ТестБезМодуля.xml",
            "<?xml version=\"1.0\"?>\n<MetaDataObject><Enum><Properties><Name>ТестБезМодуля</Name></Properties></Enum></MetaDataObject>\n",
        );

        let db_path = tmp.path().join("lite.db");
        lite_index::build(&root, &db_path, 0).unwrap();

        let source = LiteSource::open(&db_path).unwrap();
        assert_eq!(source.object_exists("Enums", "тестбезмодуля"), Some(true));
        assert_eq!(source.object_exists("Enums", "нетакого"), Some(false));
        // Коллекция, которой в выгрузке вовсе не встретилось, — объектов в ней
        // достоверно нет (не «не знаю»).
        assert_eq!(source.object_exists("НеизвестнаяКоллекция", "х"), Some(false));

        let names = source.collection_names("Enums").expect("подсказки должны быть");
        assert!(names.contains("ТестБезМодуля"));
    }

    // ── Ручная проверка на реальной базе (приёмка, шаг 3 плана) ────────────

    /// `#[ignore]`: требует реальную базу `C:/Temp/ut_lite.db` (собрана вне
    /// тестового прогона). Имя взято запросом:
    /// `SELECT m.name_lower FROM methods m JOIN modules md ON md.id=m.module_id
    ///  WHERE md.is_global=1 AND m.is_export=1 LIMIT 3`.
    #[test]
    #[ignore]
    fn lite_source_real_ut_db_confirms_global_export() {
        let db_path = Path::new(r"C:\Temp\ut_lite.db");
        let source = LiteSource::open(db_path).expect("не удалось открыть C:/Temp/ut_lite.db");
        let real_export = "контрольотображенияпротоколавзаимодействия";
        assert!(
            source.is_global_export(real_export),
            "реальный экспорт глобального модуля должен подтверждаться: {real_export}"
        );
        assert!(source.method_exists(real_export));
    }

    #[test]
    fn lite_source_owner_exports_for_form_absent_from_index() {
        // Новая форма внешней обработки: индекс о ней ещё не знает, но путь
        // указывает на владельца — экспорты обязаны найтись.
        let db = std::path::Path::new(r"C:\Temp\ut_lite.db");
        if !db.exists() {
            eprintln!("skip: lite-индекса нет");
            return;
        }
        let src = LiteSource::open(db).unwrap();
        let names = src
            .owner_exports("external/Выгрузка накладных в Docsinbox/Form/НоваяФорма/Form.obj.bsl")
            .expect("владелец выводится из пути");
        assert!(
            names.contains("сведенияовнешнейобработке"),
            "экспорты владельца не найдены, получено {} имён",
            names.len()
        );
    }

    #[test]
    fn code_index_db_source_returns_owner_exports() {
        let db = std::path::Path::new(r"C:\RepoUT-test\.code-index\index.db");
        if !db.exists() {
            eprintln!("skip: базы code-index нет");
            return;
        }
        let src = CodeIndexDbSource::open(db).unwrap();
        let names = src
            .owner_exports("external/Выгрузка накладных в Docsinbox/Form/Форма/Form.obj.bsl")
            .expect("владелец должен определиться по пути");
        // Экспортные методы модуля объекта этой обработки.
        assert!(names.contains("сведенияовнешнейобработке"), "получено: {} имён", names.len());
        assert!(names.contains("выполнитьобменссервисом"));
        // Не-форма → владельца нет.
        assert!(src.owner_exports("base/Documents/Заказ/Ext/ObjectModule.bsl").is_none());
    }

    #[test]
    fn code_index_db_source_knows_global_exports() {
        let db = std::path::Path::new(r"C:\RepoUT-test\.code-index\index.db");
        if !db.exists() {
            eprintln!("skip: базы code-index нет");
            return;
        }
        let src = CodeIndexDbSource::open(db).unwrap();
        assert!(src.is_global_export("контрольотображенияпротоколавзаимодействия"));
        assert!(!src.is_global_export("дополнитьзапросдлярасчетапотенциаласделки"));
        eprintln!("глобальных экспортов найдено: {}", src.describe());
    }
}
