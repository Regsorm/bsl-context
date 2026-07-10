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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
        Ok(Self {
            index: Mutex::new(index),
            all_names,
            global_exports,
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

        Ok(Self {
            names,
            db_path: db_path.to_path_buf(),
            conn: Mutex::new(conn),
            global_exports,
        })
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
        };
        source.initialize()?;
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
        let found = self.call_search(name_lower).unwrap_or_else(|e| {
            tracing::warn!(error = %e, name = name_lower, "code-index mcp: ошибка search_function");
            Vec::new()
        });
        self.search_cache
            .lock()
            .unwrap()
            .insert(name_lower.to_string(), found.clone());
        found
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
        let is_global = self.call_module_is_global(xml_path).unwrap_or_else(|e| {
            tracing::warn!(error = %e, xml_path, "code-index mcp: ошибка read_file общего модуля");
            false
        });
        self.global_module_cache
            .lock()
            .unwrap()
            .insert(xml_path.to_string(), is_global);
        is_global
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
        let names = self.call_owner_exports(&owner).unwrap_or_else(|e| {
            tracing::warn!(error = %e, owner = %owner, "code-index mcp: ошибка get_file_summary");
            HashSet::new()
        });
        self.owner_cache.lock().unwrap().insert(owner, names.clone());
        Some(names)
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

/// Полезная нагрузка ответа `code-index`. С версии 0.9 инструменты заворачивают
/// её в `{"result": {...}, "hint": ..., "truncated": ...}`; более ранние отдавали
/// объект напрямую. Разворачиваем оба варианта — иначе `functions` не находятся
/// и источник молча отвечает «имени нет» на любое имя.
fn payload(value: &Value) -> &Value {
    value.get("result").unwrap_or(value)
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
