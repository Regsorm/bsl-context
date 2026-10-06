//! MCP-сервер: tools для запроса контекста платформы 1С и валидации BSL.
//!
//! Справочные (`search`, `info`, `getMember`, `getMembers`, `getConstructors`,
//! `getEnumValues`) возвращают Markdown через [`platform_index::format`];
//! проверки (`validateEnum`, `validateMethodCall`, `validateExpression`,
//! `validateModule`) — JSON. Служебные: `reload_config`,
//! `symbol_sources_status`, `rebuild_symbol_index`, `reconnect_symbol_source`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use bsl_validator::{
    module_context::FormKind, validate_enum, validate_method_call, validate_module_degraded,
    validate_module_with_profile, validate_module_with_symbols_and_form_kind, ExpressionValidation,
    Profile, SymbolSource, FORM_TYPE,
};
use platform_index::{format, Definition, PlatformIndex, SearchEngine};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_router, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::module_source::{self, ModuleFile};

/// Вид формы по файлам выгрузки: рядом с модулем формы лежит `Ext/Form.xml`
/// (форма управляемая) или `Ext/Form.bin` (обычная).
///
/// Нужен фрагментам управляемых форм (issue #32): директив компиляции в тексте
/// фрагмента нет, и без этой подсказки он считался бы обычной формой, а её
/// контекст — объектом-владельцем. `None` — не определили (корня нет, файлов нет,
/// путь не модуль формы); тогда работают прежние признаки, директивы.
fn form_kind_from_dump(root: Option<&Path>, module_path: Option<&str>) -> Option<FormKind> {
    let root = root?;
    let path = module_path?.replace('\\', "/");
    // `Catalogs/X/Forms/Ф/Ext/Form/Module.bsl` → `<root>/Catalogs/X/Forms/Ф/Ext/`
    let dir = path.strip_suffix("Form/Module.bsl")?;
    let base = root.join(dir);
    if base.join("Form.xml").is_file() {
        Some(FormKind::Managed)
    } else if base.join("Form.bin").is_file() {
        Some(FormKind::Ordinary)
    } else {
        None
    }
}

/// Слот одного источника имён: конфиг, сам источник (пересборка подменяет его на
/// ходу) и флаг «идёт пересборка». Флаг на слот, а не на сервер: пересборка индекса
/// одной конфигурации не должна мешать работе с другой.
pub struct SourceSlot {
    pub config: crate::config::SymbolSourceConfig,
    /// Под `RwLock`, потому что `rebuild_symbol_index` подменяет источник на ходу.
    /// `tokio`-версия: блокировка переживает `await` вокруг сборки индекса.
    pub source: tokio::sync::RwLock<Option<Arc<dyn SymbolSource>>>,
    rebuilding: AtomicBool,
    /// Идёт попытка переподключения — второй запрос ждать её не должен.
    reconnecting: AtomicBool,
    /// Когда пробовали переподключиться в последний раз. Без отступа каждый
    /// `validate_module` при лежащем code-index ждал бы таймаут соединения.
    last_reconnect: std::sync::Mutex<Option<std::time::Instant>>,
    /// Текст последней неудачной попытки подключения. Нужен инструменту
    /// `symbol_sources_status`: без него причину отказа можно было узнать
    /// только чтением журнала сервера.
    last_error: std::sync::Mutex<Option<String>>,
}

/// Карта слотов источников имён: алиас → слот.
///
/// Подменяется целиком при перечитке config.toml (`reload_config`), поэтому слоты
/// живут за `Arc`: запрос, начатый до подмены, доработает со старым слотом.
pub type SourceMap = BTreeMap<String, Arc<SourceSlot>>;

/// Карта слотов за блокировкой чтения/записи. Вынесена наружу (`http.rs`), чтобы
/// /health читал актуальную карту, а не её снимок на момент старта.
pub type SourceMapHandle = Arc<std::sync::RwLock<Arc<SourceMap>>>;

/// Описание одного источника при первоначальной сборке карты.
pub type SourceSlotInit = (
    String,
    crate::config::SymbolSourceConfig,
    Result<Option<Arc<dyn SymbolSource>>, String>,
);

/// Отступ между попытками переподключения. Секунды, а не мгновенно: источник
/// поднимается людьми (перезапуск code-index) или планировщиком, и долбить его
/// на каждый вызов бессмысленно — зато после недолгого простоя валидация
/// возвращается сама, без перезапуска bsl-context.
const RECONNECT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(15);

/// Предел размера BSL-исходника в `validate_module`. Гигантский или враждебный
/// вход (в stdio кадр не ограничен) обязан отсекаться явным отказом, а не
/// блокировать воркер на минуты.
///
/// Ровно столько же, сколько у файлового входа (`path`,
/// [`crate::module_source::MAX_MODULE_BYTES`]): реальные модули 1С доходят до
/// ~9 МБ, и текстовый вход не должен отказывать там, где файловый работает —
/// разные потолки у одного и того же модуля давали ложный отказ «исходник
/// слишком большой» на модуле, который по `path` проверяется.
pub(crate) const MAX_SOURCE_BYTES: usize = crate::module_source::MAX_MODULE_BYTES as usize;

/// Предел запроса `search`: имена типов и методов — сотни байт.
const MAX_QUERY_BYTES: usize = 4 * 1024;

/// Сбрасывает атомарный флаг при выходе из области — включая отмену future и
/// панику: без guard'а прерванная посреди `.await` попытка оставляла
/// `reconnecting`/`rebuilding` залипшими навсегда.
struct FlagGuard<'a>(&'a AtomicBool);

impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl SourceSlot {
    /// `built` — результат первой попытки подключения (см.
    /// [`crate::sources::build_symbol_source`]): текст ошибки сохраняется в
    /// слоте и отдаётся инструментом `symbol_sources_status`.
    pub fn new(
        config: crate::config::SymbolSourceConfig,
        built: Result<Option<Arc<dyn SymbolSource>>, String>,
    ) -> Self {
        let (source, last_error) = match built {
            Ok(source) => (source, None),
            Err(msg) => (None, Some(msg)),
        };
        Self {
            config,
            source: tokio::sync::RwLock::new(source),
            rebuilding: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            last_reconnect: std::sync::Mutex::new(None),
            last_error: std::sync::Mutex::new(last_error),
        }
    }

    /// Пора ли пробовать снова (прошёл отступ и никто не пробует прямо сейчас).
    fn reconnect_due(&self) -> bool {
        let last = self
            .last_reconnect
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(at) => at.elapsed() >= RECONNECT_COOLDOWN,
            None => true,
        }
    }

    /// Текст последней неудачной попытки подключения, если она была.
    fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Состояние MCP-сервера: индекс + поисковый движок (готовы к чтению).
#[derive(Clone)]
pub struct BslContextServer {
    pub index: Arc<PlatformIndex>,
    pub engine: Arc<SearchEngine>,
    /// Дефолтный уровень валидации, если клиент не передал `level` в `validate_module`.
    /// Берётся из `config.toml` (поле `default_validation_level`), кламп в `[1..=3]`.
    pub default_validation_level: u8,
    /// Дефолтный профиль потребителя, если клиент не передал `profile`
    /// в `validate_module`. Берётся из `config.toml` (поле `default_profile`).
    pub default_profile: Profile,
    /// Именованные источники имён конфигураций. Ключ — алиас: это значение параметра
    /// `repo` у `validate_module`/`rebuild_symbol_index`. Пустая карта — конфигураций
    /// не настроено, валидация идёт только против справки платформы.
    ///
    /// Карта подменяется целиком при перечитке config.toml: слоты живут за `Arc`,
    /// поэтому начатый запрос доработает со старым слотом, а не увидит полупустую карту.
    sources: SourceMapHandle,
    /// Путь к config.toml, если сервер запущен с `--config`. Без него перечитывать
    /// нечего: инструмент `reload_config` отвечает отказом.
    config_path: Option<Arc<PathBuf>>,
    /// Снимок «холодных» полей конфига (платформа, порт, белый список, дефолты
    /// проверки), снятый при старте и заменяемый свежим при каждой удачной перечитке.
    /// Нужен, чтобы предупредить об их изменении один раз, а не на каждую перечитку.
    cold_baseline: Arc<std::sync::Mutex<Option<crate::config::Config>>>,
    /// Белый список инструментов из `[tools].enabled`. `None` — фильтр выключен.
    /// `Arc`, потому что сервер клонируется на каждый запрос.
    allowed_tools: Option<Arc<BTreeSet<String>>>,
    /// Причина, по которой платформенный индекс не загружен. `None` — индекс
    /// есть. Заполнена только у сервера из [`BslContextServer::unavailable`]:
    /// индекс-зависимые инструменты отвечают отказом с этой причиной, а
    /// инструменты обслуживания (перечитка конфига, источники имён) работают.
    unavailable_reason: Option<Arc<str>>,
    /// `platform_path` пришёл из опции командной строки и перекрывает значение
    /// файла: при перечитке config.toml это поле не сравнивается — файл всё
    /// равно не может его изменить.
    platform_path_from_cli: bool,
    tool_router: ToolRouter<Self>,
}

/// Инструменты, которым для ответа нужен платформенный индекс. Остальные
/// (`reload_config`, `symbol_sources_status`, `reconnect_symbol_source`,
/// `rebuild_symbol_index`) обязаны работать и без него: оператор, запустивший
/// сервер без платформы, должен иметь возможность посмотреть состояние
/// источников и перечитать настройку. Полнота разбиения закреплена тестом
/// `tool_partition_matches_router`.
const PLATFORM_INDEX_TOOLS: [&str; 10] = [
    "search",
    "info",
    "get_member",
    "get_members",
    "get_constructors",
    "get_enum_values",
    "validate_enum",
    "validate_method_call",
    "validate_module",
    "reserved_names",
];

fn cold_changes(
    prev: &crate::config::Config,
    fresh: &crate::config::Config,
    platform_path_from_cli: bool,
) -> Vec<&'static str> {
    let changed = [
        (
            "platform_path",
            !platform_path_from_cli && prev.platform_path != fresh.platform_path,
        ),
        ("host", prev.host != fresh.host),
        ("port", prev.port != fresh.port),
        (
            "platform_cache_path",
            prev.platform_cache_path != fresh.platform_cache_path,
        ),
        ("allowed_hosts", prev.allowed_hosts != fresh.allowed_hosts),
        ("tools.enabled", prev.tools.enabled != fresh.tools.enabled),
        (
            "default_validation_level",
            prev.default_validation_level != fresh.default_validation_level,
        ),
        (
            "default_profile",
            prev.default_profile != fresh.default_profile,
        ),
    ];
    changed
        .into_iter()
        .filter_map(|(name, is_changed)| is_changed.then_some(name))
        .collect()
}

impl BslContextServer {
    pub fn new(index: PlatformIndex) -> Self {
        Self::with_defaults(index, 1, Profile::Full)
    }

    /// Совместимость со старым вызовом (профиль — дефолтный `Full`).
    pub fn with_default_level(index: PlatformIndex, default_validation_level: u8) -> Self {
        Self::with_defaults(index, default_validation_level, Profile::Full)
    }

    pub fn with_defaults(
        index: PlatformIndex,
        default_validation_level: u8,
        default_profile: Profile,
    ) -> Self {
        let engine = SearchEngine::from_index(&index);
        Self {
            index: Arc::new(index),
            engine: Arc::new(engine),
            default_validation_level: default_validation_level.clamp(1, 3),
            default_profile,
            sources: Arc::new(std::sync::RwLock::new(Arc::new(BTreeMap::new()))),
            config_path: None,
            cold_baseline: Arc::new(std::sync::Mutex::new(None)),
            allowed_tools: None,
            unavailable_reason: None,
            platform_path_from_cli: false,
            tool_router: Self::tool_router(),
        }
    }

    /// Сервер без платформенного индекса: `platform_path` не задан или hbk не
    /// найден. Рукопожатие и `tools/list` работают — клиент видит инструменты
    /// и причину в `instructions`, — а вызов справочного инструмента получает
    /// понятный отказ вместо ложного «ничего не найдено» на пустом индексе.
    pub fn unavailable(
        reason: String,
        default_validation_level: u8,
        default_profile: Profile,
    ) -> Self {
        let mut server = Self::with_defaults(
            PlatformIndex::new(),
            default_validation_level,
            default_profile,
        );
        server.unavailable_reason = Some(Arc::from(reason));
        server
    }

    /// Загружен ли платформенный индекс.
    pub fn index_loaded(&self) -> bool {
        self.unavailable_reason.is_none()
    }

    /// Причина недоступности платформенного индекса, если он не загружен.
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    /// Подключить именованные источники имён (по одному на конфигурацию). Вызывается
    /// на старте; дальше карту целиком подменяет перечитка config.toml
    /// (`reload_sources_from_config`), а содержимое слотов меняется пересборкой
    /// индекса конкретной конфигурации.
    pub fn with_sources(mut self, slots: Vec<SourceSlotInit>) -> Self {
        let map: SourceMap = slots
            .into_iter()
            .map(|(name, config, built)| (name, Arc::new(SourceSlot::new(config, built))))
            .collect();
        self.sources = Arc::new(std::sync::RwLock::new(Arc::new(map)));
        self
    }

    /// Снимок карты источников. Блокировка берётся на время клонирования `Arc` и
    /// отпускается до любого `await` — это единственное место, где берётся `read()`.
    pub fn sources_snapshot(&self) -> Arc<SourceMap> {
        self.sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Слот по алиасу — через снимок карты.
    pub fn slot(&self, repo: &str) -> Option<Arc<SourceSlot>> {
        self.sources_snapshot().get(repo).cloned()
    }

    /// Подменить карту целиком. Блокировка на запись живёт без `await`: у уже
    /// начатых запросов на руках остаётся снимок старой карты.
    fn replace_sources(&self, map: SourceMap) {
        *self.sources.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(map);
    }

    /// Запомнить путь к config.toml и снять базовый снимок «холодных» полей.
    ///
    /// Ошибка чтения старт не ломает: сервер уже поднят с тем конфигом, что прочитал
    /// `main`, — ронять его из-за недоступного файла незачем.
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        match crate::config::Config::load_or_default(Some(&path)) {
            Ok(cfg) => *self.cold_baseline.lock().unwrap_or_else(|e| e.into_inner()) = Some(cfg),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "config.toml не прочитан — перечитка сообщит об ошибке"
                );
            }
        }
        self.config_path = Some(Arc::new(path));
        self
    }

    /// Отметить, что `platform_path` пришёл из опции командной строки и
    /// перекрывает значение файла: при перечитке config.toml это поле не
    /// сравнивается — файл всё равно не может его изменить.
    pub fn with_cli_platform_path(mut self, from_cli: bool) -> Self {
        self.platform_path_from_cli = from_cli;
        self
    }

    /// Предупредить об изменении полей, которые перечитка не применяет: платформа,
    /// адрес, порт и белый список инструментов прочитаны на старте, дефолты проверки
    /// зашиты в сервер. Снимок после сравнения заменяется свежим, иначе одно и то же
    /// предупреждение повторялось бы на каждой перечитке.
    fn warn_on_cold_changes(&self, fresh: &crate::config::Config) {
        let prev = {
            let mut guard = self.cold_baseline.lock().unwrap_or_else(|e| e.into_inner());
            guard.replace(fresh.clone())
        };
        let Some(prev) = prev else { return };
        for name in cold_changes(&prev, fresh, self.platform_path_from_cli) {
            tracing::warn!("поле {name} изменилось, применится после перезапуска");
        }
    }

    /// Перечитать config.toml и подменить карту источников имён целиком.
    ///
    /// Применяется только список `[[symbol_sources]]`: `platform_path`, `host`,
    /// `port`, `allowed_hosts`, `platform_cache_path`, `[tools].enabled` и дефолты
    /// проверки требуют перезапуска — об их изменении пишется предупреждение в
    /// журнал.
    ///
    /// Слот, настройки которого не изменились, переносится в новую карту тем же
    /// `Arc`: пересоздание погасило бы кэш и разорвало сессию к code-index у уже
    /// подключённого источника. Идущая в этот момент пересборка чужого слота ничего
    /// не ломает: она держит свой `Arc` и просто не попадёт в новую карту, если
    /// алиас изменился.
    pub async fn reload_sources_from_config(&self) -> Result<serde_json::Value, String> {
        let Some(path) = self.config_path.as_deref() else {
            return Err("сервер запущен без --config: перечитывать нечего".to_string());
        };
        // Ошибку разбора отдаём вызывающему: карту в этом случае не трогаем вовсе,
        // чтобы опечатка в файле не оставила сервер без рабочих источников.
        let cfg =
            crate::config::Config::load_or_default(Some(path)).map_err(|e| format!("{e:#}"))?;
        let resolved = cfg
            .resolved_symbol_sources()
            .map_err(|e| format!("{e:#}"))?;
        // Предупреждаем о «холодных» полях только после успешной валидации:
        // иначе отклонённый файл «съедал» baseline и предупреждение терялось.
        self.warn_on_cold_changes(&cfg);

        let old = self.sources_snapshot();
        let mut new_map: SourceMap = BTreeMap::new();
        let mut added = Vec::new();
        let mut recreated = Vec::new();
        let mut unchanged = Vec::new();
        for (name, new_cfg) in resolved {
            match old.get(&name) {
                // Настройки те же — берём тот же слот, ничего не переподключаем.
                Some(slot) if slot.config == new_cfg => {
                    new_map.insert(name.clone(), Arc::clone(slot));
                    unchanged.push(name);
                }
                Some(_) => {
                    new_map.insert(name.clone(), Arc::new(build_slot(new_cfg).await));
                    recreated.push(name);
                }
                None => {
                    new_map.insert(name.clone(), Arc::new(build_slot(new_cfg).await));
                    added.push(name);
                }
            }
        }
        let removed: Vec<String> = old
            .keys()
            .filter(|name| !new_map.contains_key(*name))
            .cloned()
            .collect();
        self.replace_sources(new_map);

        // Состояние отдаём по свежей карте: подключение новых слотов только что
        // прошло, и потребителю нужен результат, а не то, что было до перечитки.
        let sources = {
            let map = self.sources_snapshot();
            let mut out = Vec::with_capacity(map.len());
            for (name, slot) in map.iter() {
                out.push(slot_state_json(name, slot).await);
            }
            out
        };
        tracing::info!(
            added = ?added,
            removed = ?removed,
            recreated = ?recreated,
            unchanged = ?unchanged,
            "config.toml перечитан — карта источников имён подменена"
        );
        Ok(serde_json::json!({
            "ok": true,
            "config_path": path.display().to_string(),
            "added": added,
            "removed": removed,
            "recreated": recreated,
            "unchanged": unchanged,
            "sources": sources,
        }))
    }

    /// Применить белый список инструментов (`[tools].enabled` из config.toml).
    ///
    /// Пустой список — фильтр выключен, доступны все инструменты. Неизвестные
    /// имена старту не мешают: пишется предупреждение, сервер работает (иначе
    /// опечатка в конфиге роняла бы сервис).
    pub fn apply_tools_whitelist(mut self, enabled: &[String]) -> Self {
        if enabled.is_empty() {
            tracing::info!(
                "[tools].enabled пуст — белый список выключен, доступны все инструменты"
            );
            return self;
        }
        let known: BTreeSet<String> = self
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let allowed: BTreeSet<String> = enabled.iter().cloned().collect();
        let unknown: Vec<String> = allowed
            .iter()
            .filter(|n| !known.contains(*n))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            tracing::warn!(
                ?unknown,
                "[tools].enabled содержит неизвестные имена инструментов (опечатка?) — они не разрешают ничего"
            );
        }
        // В список кладём ТОЛЬКО существующие имена: опечатка не должна
        // занимать место в белом списке, ничего при этом не разрешая.
        let allowed: BTreeSet<String> = allowed
            .into_iter()
            .filter(|name| known.contains(name))
            .collect();
        if allowed.is_empty() {
            // Список из одних опечаток запретил бы ВСЕ инструменты: сервис
            // стартует, /health отвечает «ok», tools/list пуст — работающим он
            // только выглядит. Это заведомо не то, чего хотел автор конфига,
            // поэтому фильтр не включаем, а ошибку делаем видимой.
            tracing::error!(
                "[tools].enabled не содержит ни одного существующего имени — белый список \
                 НЕ включён, доступны все инструменты. Проверьте имена в config.toml."
            );
            return self;
        }
        tracing::info!(
            known = allowed.len(),
            listed = allowed.len() + unknown.len(),
            "[tools].enabled — белый список активен"
        );
        self.allowed_tools = Some(Arc::new(allowed));
        self
    }

    /// Разрешён ли инструмент белым списком. Без списка — разрешено всё.
    pub fn is_tool_allowed(&self, name: &str) -> bool {
        match &self.allowed_tools {
            Some(allowed) => allowed.contains(name),
            None => true,
        }
    }

    /// Настроенные алиасы через запятую — для текста ошибок.
    fn source_names(&self) -> String {
        self.sources_snapshot()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Найти конфигурацию по алиасу. `repo` обязателен, когда настроена хотя бы одна:
    /// молча подставлять единственную нельзя — вызов должен быть однозначным.
    fn resolve_slot(&self, repo: Option<&str>) -> Result<Arc<SourceSlot>, String> {
        let sources = self.sources_snapshot();
        if sources.is_empty() {
            return Err(
                "на сервере не настроено ни одной конфигурации: ни выгрузки, ни источника имён \
                 (секция [[symbol_sources]] в config.toml)"
                    .to_string(),
            );
        }
        match repo {
            Some(name) => sources.get(name).cloned().ok_or_else(|| {
                format!(
                    "конфигурация \"{name}\" не настроена; доступны: {}",
                    self.source_names()
                )
            }),
            None => Err(format!(
                "параметр repo обязателен; доступные конфигурации: {}",
                self.source_names()
            )),
        }
    }

    /// Пересоздать источник имён для слота. Нужно потому, что сетевой источник
    /// (`code_index_mcp`) роняет свой `healthy` навсегда — по замыслу его
    /// автора, «источник пересоздаётся заново». Пересоздавать было нечем:
    /// `rebuild_symbol_index` работает только с `lite`, а `build_symbol_source`
    /// вызывался единственный раз на старте. Итог: секундный простой
    /// code-index (штатный перезапуск) выключал валидацию по конфигурации до
    /// перезапуска самого bsl-context, а сообщение отправляло искать
    /// несуществующую поломку в уже исправном code-index.
    ///
    /// Возвращает `true`, если после попытки в слоте лежит здоровый источник.
    ///
    /// `force` — не смотреть на отступ (инструмент `reconnect_symbol_source`:
    /// человек или конвейер попросил явно, значит ждать нечего).
    async fn try_reconnect(&self, repo: &str, slot: &SourceSlot, force: bool) -> bool {
        if (!force && !slot.reconnect_due()) || slot.reconnecting.swap(true, Ordering::SeqCst) {
            return false;
        }
        // Флаг сбросится и при отмене future, и при панике.
        let _reconnecting_guard = FlagGuard(&slot.reconnecting);
        *slot
            .last_reconnect
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());

        let cfg = slot.config.clone();
        // Создание синхронное (рукопожатие MCP через `ureq`) — worker-поток
        // tokio им занимать нельзя.
        let built = tokio::task::spawn_blocking(move || crate::sources::build_symbol_source(&cfg))
            .await
            .unwrap_or_else(|e| Err(format!("поток подключения источника упал: {e}")));

        let ok = match built {
            Ok(Some(source)) => {
                tracing::info!(repo, "источник имён конфигурации переподключён");
                *slot.source.write().await = Some(source);
                *slot.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                true
            }
            Ok(None) => {
                // Штатное «источника нет»: kind = "none" либо lite-индекс ещё
                // не собран. Ошибкой это не является, но и источника нет.
                *slot.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                false
            }
            Err(msg) => {
                tracing::warn!(repo, error = %msg, "переподключить источник имён не удалось");
                *slot.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
                false
            }
        };
        ok
    }

    /// Собрать индекс во временный файл, снять старый источник, подменить файл,
    /// открыть новый. Старый источник снимается ДО подмены: SQLite держит файл
    /// открытым, и на Windows переименовать поверх него нельзя.
    async fn rebuild_inner(
        &self,
        slot: &SourceSlot,
        root: &Path,
        db_path: &Path,
    ) -> anyhow::Result<String> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
        }
        let tmp = unique_build_tmp(db_path);

        let (root_c, tmp_c) = (root.to_path_buf(), tmp.clone());
        let build = tokio::task::spawn_blocking(move || lite_index::build(&root_c, &tmp_c, 0))
            .await
            .context("задача сборки индекса упала")?;
        let stats = match build {
            Ok(stats) => stats,
            Err(e) => {
                // Недособранный временный файл — мусор, рабочая база не тронута.
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };

        // Блокировка на запись: текущие валидации дождутся, новые подождут нас.
        let mut guard = slot.source.write().await;
        *guard = None; // закрывает старую базу — иначе Windows не даст её заменить

        let (tmp_c, db_c) = (tmp.clone(), db_path.to_path_buf());
        let swapped =
            tokio::task::spawn_blocking(move || -> anyhow::Result<symbol_source::LiteSource> {
                // Старую базу не удаляем, а откладываем в `.bak`: если подмена
                // не удастся, вернём её на место (на Windows rename поверх файла
                // запрещён, поэтому просто «переименовать поверх» нельзя).
                // Имя отката уникально вместе с tmp: две пересборки одного
                // db_path (reload_config × rebuild) не должны затирать откат
                // друг друга (аудит PR, M7).
                let backup = {
                    let mut name = tmp_c.as_os_str().to_owned();
                    name.push(".bak");
                    PathBuf::from(name)
                };
                if db_c.exists() {
                    if let Err(e) = std::fs::rename(&db_c, &backup) {
                        // Windows не переименовывает файл, открытый другим
                        // процессом (ERROR_SHARING_VIOLATION = 32): базу держит
                        // другой сеанс stdio или служба.
                        if e.raw_os_error() == Some(32) {
                            anyhow::bail!(
                                "база {} занята другим процессом (другой сеанс или служба \
                                 bsl-context) — пересоберите, когда он закроется",
                                db_c.display()
                            );
                        }
                        return Err(anyhow::Error::new(e).context(format!(
                            "не удалось отложить старую базу {}",
                            db_c.display()
                        )));
                    }
                }
                if let Err(e) = std::fs::rename(&tmp_c, &db_c) {
                    // Возвращаем прежнюю базу: иначе слот останется без источника.
                    if backup.exists() {
                        let _ = std::fs::rename(&backup, &db_c);
                    }
                    return Err(anyhow::Error::new(e).context(format!(
                        "не удалось переместить {} → {}",
                        tmp_c.display(),
                        db_c.display()
                    )));
                }
                match symbol_source::LiteSource::open(&db_c) {
                    Ok(source) => {
                        // Свежая база открылась — прежняя больше не нужна.
                        let _ = std::fs::remove_file(&backup);
                        Ok(source)
                    }
                    Err(e) => {
                        // Свежую не открыть — возвращаем прежнюю.
                        let _ = std::fs::remove_file(&db_c);
                        if backup.exists() {
                            let _ = std::fs::rename(&backup, &db_c);
                        }
                        Err(e.context("не удалось открыть свежий индекс"))
                    }
                }
            })
            .await
            .context("задача подмены индекса упала")?;

        let source = match swapped {
            Ok(source) => source,
            Err(e) => {
                // Подмена не удалась. Источник уже снят — пробуем вернуть прежнюю
                // базу, если файл на месте: иначе сервер останется без источника
                // до перезапуска, хотя валидация могла бы продолжать работать.
                if db_path.exists() {
                    match symbol_source::LiteSource::open(db_path) {
                        Ok(old) => {
                            *guard = Some(Arc::new(old) as Arc<dyn SymbolSource>);
                            tracing::warn!(error = %e, "пересборка не удалась — вернулись к прежнему индексу");
                        }
                        Err(reopen) => {
                            tracing::error!(error = %reopen, "прежний индекс не открывается — источник отключён");
                        }
                    }
                }
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };

        *guard = Some(Arc::new(source) as Arc<dyn SymbolSource>);
        drop(guard);

        Ok(serde_json::json!({
            "ok": true,
            "modules": stats.modules,
            "methods": stats.methods,
            "global_modules": stats.global_modules,
            "elapsed_ms": stats.elapsed_ms,
            // Этапы: видно, где прошло время (обход, XML, разбор, запись, индексы).
            "walk_ms": stats.walk_ms,
            "xml_ms": stats.xml_ms,
            "parse_ms": stats.parse_ms,
            "db_ms": stats.db_ms,
            "indexes_ms": stats.indexes_ms,
            "db_path": db_path.display().to_string(),
        })
        .to_string())
    }
}

/// Создать слот источника: подключение синхронное (рукопожатие MCP через `ureq`),
/// поэтому идёт в `spawn_blocking` — worker-поток tokio им занимать нельзя.
async fn build_slot(config: crate::config::SymbolSourceConfig) -> SourceSlot {
    let cfg = config.clone();
    let built = tokio::task::spawn_blocking(move || crate::sources::build_symbol_source(&cfg))
        .await
        .unwrap_or_else(|e| Err(format!("поток подключения источника упал: {e}")));
    SourceSlot::new(config, built)
}

/// Уникальное имя временного файла сборки индекса: две пересборки (например,
/// гонка `reload_config` × `rebuild_symbol_index`) не должны писать в один tmp.
fn unique_build_tmp(db_path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = db_path.as_os_str().to_owned();
    name.push(format!(".{}.{}.tmp", std::process::id(), n));
    PathBuf::from(name)
}

/// Состояние слота в виде JSON.
///
/// Собрано в одном месте, потому что поля читают три потребителя:
/// `symbol_sources_status`, `reconnect_symbol_source` и отчёт перечитки
/// (`reload_sources_from_config`). Разъехавшись, они бы расходились молча.
async fn slot_state_json(repo: &str, slot: &SourceSlot) -> serde_json::Value {
    let guard = slot.source.read().await;
    let (connected, healthy, describe) = match guard.as_ref() {
        Some(source) => (true, source.is_healthy(), Some(source.describe())),
        None => (false, false, None),
    };
    let state = match (connected, healthy) {
        (true, true) => "ok",
        (true, false) => "unhealthy",
        _ => "not_connected",
    };
    serde_json::json!({
        "repo": repo,
        "kind": slot.config.kind,
        "connected": connected,
        "healthy": healthy,
        "state": state,
        // Для нездорового источника причина — в его собственном описании
        // (что именно ответил code-index), для неподнятого — в слоте.
        "last_error": describe
            .filter(|_| !healthy)
            .or_else(|| slot.last_error()),
    })
}

/// Отказ инструмента: не паника и не пустой ответ, а внятная причина.
fn err_json(message: &str) -> String {
    serde_json::json!({"ok": false, "message": message}).to_string()
}

/// Ответ `validate_module` для проверки по файлу: результат валидатора плюс
/// отпечаток прочитанного файла (issue #13). `flatten` оставляет поля результата
/// на верхнем уровне — ровно как в ответе по тексту, — а отпечаток добавляется
/// рядом: видно, какая версия файла проверена.
#[derive(Serialize)]
struct FileValidation<'a> {
    #[serde(flatten)]
    result: &'a ExpressionValidation,
    /// Полный путь прочитанного файла.
    source_path: String,
    /// Путь, ушедший в `module_path` (относительно корня выгрузки).
    source_module_path: String,
    /// Размер файла в байтах.
    source_bytes: u64,
    /// Время изменения файла, RFC3339 UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_modified: Option<String>,
}

/// Сериализация результата: с отпечатком файла, если модуль читался с диска.
/// Без файла ответ побайтово тот же, что и раньше (поля отпечатка не появляются),
/// — эталоны ответов инструментов на этом и держатся.
fn validation_json(result: &ExpressionValidation, file: Option<&ModuleFile>) -> String {
    let Some(file) = file else {
        return serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string());
    };
    let payload = FileValidation {
        result,
        source_path: file.path.display().to_string(),
        source_module_path: file.module_path.clone(),
        source_bytes: file.bytes,
        source_modified: file.modified.clone(),
    };
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

/// Проверка модуля, когда источник имён настроен, но недоступен.
///
/// Собрана в одном месте, потому что случаев четыре (источник не поднят,
/// lite-индекс не собран, источник нездоров, источник отвалился по ходу
/// проверки), а ответ у всех один: находки против платформенного контекста
/// плюс `symbols_available: false` и причина.
#[allow(clippy::too_many_arguments)]
fn degraded_json(
    index: &PlatformIndex,
    source: &str,
    level: u8,
    profile: Profile,
    module_path: Option<&str>,
    form_attributes: Option<&HashSet<String>>,
    file: Option<&ModuleFile>,
    reason: String,
) -> String {
    let result = validate_module_degraded(
        index,
        source,
        level,
        profile,
        module_path,
        form_attributes,
        reason,
    );
    validation_json(&result, file)
}

// ── Параметры tools ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Поисковый запрос (русское или английское имя). Регистронезависимо.
    pub query: String,
    /// Максимум результатов (1..=50). По умолчанию 10.
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InfoParams {
    /// Имя элемента (тип, метод, свойство). Регистронезависимо.
    pub name: String,
    /// Опциональный фильтр по виду: `type`, `method`, `property`. Без фильтра —
    /// поиск по всем коллекциям с приоритетом тип > метод > свойство.
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TypeNameParams {
    /// Русское имя типа (например, `ТаблицаЗначений`).
    #[serde(alias = "typeName")]
    pub type_name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetMemberParams {
    #[serde(alias = "typeName")]
    pub type_name: String,
    #[serde(alias = "memberName")]
    pub member_name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ValidateEnumParams {
    /// Имя типа-перечисления (например, `ТипРазмещенияТекстаТабличногоДокумента`).
    #[serde(alias = "typeName")]
    pub type_name: String,
    /// Проверяемое значение (например, `Перенос`).
    #[serde(alias = "valueName")]
    pub value_name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ValidateMethodCallParams {
    /// Имя глобального метода (`СтрНайти`, `Найти`, `СформироватьЗапрос` и т.д.).
    #[serde(alias = "methodName")]
    pub method_name: String,
    /// Количество фактически передаваемых аргументов в вызове.
    #[serde(alias = "argCount")]
    pub arg_count: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ValidateModuleParams {
    /// Текст BSL: целый модуль (общий модуль, модуль объекта, модуль формы) либо
    /// произвольный фрагмент. У целого модуля валидатор сам извлекает через
    /// tree-sitter объявленные процедуры/функции и не путает их вызовы с
    /// опечатками платформенных методов; у фрагмента этот список просто пуст.
    ///
    /// Взаимоисключающе с `path`: передайте ровно одно из двух (пустой `source`
    /// считается непереданным). Строка, похожая на путь к файлу, здесь
    /// отвергается с понятным текстом — раньше она разбиралась как BSL и давала
    /// `valid: true` без единой проверки (issue #13).
    #[serde(
        default,
        alias = "bslModule",
        alias = "module",
        alias = "bslSnippet",
        alias = "snippet",
        alias = "code"
    )]
    pub source: String,
    /// Путь к файлу модуля вместо текста: абсолютный или относительно корня
    /// выгрузки конфигурации (поле `root` источника имён для `repo`). Сервер
    /// читает файл сам — модуль на 120 КБ не нужно прогонять через свой контекст.
    ///
    /// Файл обязан лежать ВНУТРИ корня выгрузки (символические ссылки и junction'ы
    /// разрешаются до проверки), проверяются только `.bsl`. `module_path` при этом
    /// выводится из пути сам — проверки модуля формы и объектного контекста
    /// включаются без отдельного параметра. В ответе — путь, размер и время
    /// изменения файла, чтобы было видно, какая версия проверена.
    #[serde(alias = "file", alias = "modulePathFile")]
    pub path: Option<String>,
    /// Уровень валидации:
    /// `1` (default) — статический анализ ссылок с явным именем типа в исходнике;
    /// `2` — дополнительно локальный type inference (Phase 8 MVP) для переменных,
    /// присвоенных через `Новый`, `ТипX.ЗначениеY` или аннотацию `// @type ТипX`;
    /// `3` — дополнительно return-type tracking (Уровень 2.5): тип переменной из
    /// возвращаемого типа метода/свойства и цепочек `Запрос.Выполнить().Выбрать()`.
    /// Чем выше уровень, тем больше находок и потенциальных false-positive —
    /// поэтому за флагом. Клампится в `[1..=3]`.
    pub level: Option<u8>,
    /// Профиль потребителя (карточка-decision #1230):
    /// `"full"` (default) — все находки, `level` как передан; для сильной модели,
    /// которая сама отбросит сомнительные.
    /// `"strict"` — только high-confidence находки (`unknown_enum_value`,
    /// `wrong_argument_count`) и форсированный `level=1`; для слабых моделей
    /// (LibreChat/DeepSeek), чтобы ложное срабатывание не приводило к зацикливанию.
    /// Неизвестное значение трактуется как `"full"`.
    pub profile: Option<String>,
    /// Относительный путь модуля в выгрузке; нужен, чтобы учесть экспортные
    /// методы модуля объекта-владельца внешней обработки, а также чтобы понять,
    /// что это модуль формы (`.../Forms/<Имя>/Ext/Form/Module.bsl` или
    /// `.../Form/<Имя>/Form.obj.bsl`), и включить проверку имён, занятых
    /// членами `ФормаКлиентскогоПриложения`.
    ///
    /// Передавайте его ВСЕГДА, когда путь известен, особенно для модуля
    /// объекта. Без пути валидатор не отличает модуль объекта от произвольного
    /// фрагмента и считает, что неявного контекста объекта нет: обращения к
    /// реквизитам и табличным частям (`Товары.Очистить()`) тогда дают находку
    /// `unknown_common_module`.
    ///
    /// При проверке по `path` выводится из пути автоматически; явно переданное
    /// значение приоритетнее выведенного.
    #[serde(alias = "modulePath")]
    pub module_path: Option<String>,
    /// Имена реквизитов формы (`Объект`, `Список`, свои реквизиты). Реквизит
    /// перекрывает имя контекста, поэтому такие имена из проверки исключаются —
    /// и тогда внутри модуля формы включается проверка имён глобального
    /// контекста (`Справочники = …` в форме без реквизита `Справочники` —
    /// ошибка). Без этого параметра она в формах не работает: валидатор не
    /// видит состава реквизитов и молчит, чтобы не выдать ложную находку.
    #[serde(alias = "formAttributes")]
    pub form_attributes: Option<Vec<String>>,
    /// Алиас конфигурации из настроек сервера (`repo` в `[[symbol_sources]]`), чьи
    /// имена методов учитывать. Обязателен, если на сервере настроена хотя бы одна
    /// конфигурация. Если не настроено ни одной — код проверяется только против
    /// справки платформы, и параметр не нужен.
    pub repo: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RebuildSymbolIndexParams {
    /// Алиас конфигурации, чей lite-индекс пересобрать (`repo` в `[[symbol_sources]]`).
    pub repo: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ReconnectSymbolSourceParams {
    /// Алиас конфигурации, чей источник имён переподключить (`repo` в `[[symbol_sources]]`).
    pub repo: Option<String>,
}

// ── Tools ──────────────────────────────────────────────────────────────────

#[tool_router]
impl BslContextServer {
    #[tool(
        description = "Нечёткий поиск по платформенному контексту: типы, глобальные методы, глобальные свойства. \
                       Префиксное совпадение, fallback word-order и подстрока. Возвращает Markdown."
    )]
    pub async fn search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        if p.query.len() > MAX_QUERY_BYTES {
            return format!(
                "❌ Запрос слишком длинный: {} байт (предел {}).",
                p.query.len(),
                MAX_QUERY_BYTES
            );
        }
        let limit = p.limit.unwrap_or(10);
        let results = self.engine.search(&p.query, limit);
        let mut out = format::format_query_header(&p.query);
        out.push_str(&format::format_search_results(&results));
        out
    }

    #[tool(
        description = "Подробная информация об элементе по точному имени. kind может быть 'type'/'method'/'property' \
                       для фильтрации; без него ищется тип, затем метод, затем свойство."
    )]
    pub async fn info(&self, Parameters(p): Parameters<InfoParams>) -> String {
        let kind = p.kind.as_deref().map(str::to_ascii_lowercase);
        let def = match kind.as_deref() {
            Some("type") => self
                .engine
                .find_type(&p.name)
                .cloned()
                .map(Definition::Type),
            Some("method") => self
                .engine
                .find_method(&p.name)
                .cloned()
                .map(Definition::Method),
            Some("property") => self
                .engine
                .find_property(&p.name)
                .cloned()
                .map(Definition::Property),
            _ => self
                .engine
                .find_type(&p.name)
                .cloned()
                .map(Definition::Type)
                .or_else(|| {
                    self.engine
                        .find_method(&p.name)
                        .cloned()
                        .map(Definition::Method)
                })
                .or_else(|| {
                    self.engine
                        .find_property(&p.name)
                        .cloned()
                        .map(Definition::Property)
                }),
        };
        match def {
            Some(d) => format::format_member(&d),
            None => format!(
                "❌ **Не найдено:** элемент '{}' не найден в платформенном контексте\n",
                p.name
            ),
        }
    }

    #[tool(
        description = "Получить член типа (метод или свойство) по точному имени. Возвращает Markdown с описанием \
                       найденного метода/свойства либо ошибку 'не найден'."
    )]
    pub async fn get_member(&self, Parameters(p): Parameters<GetMemberParams>) -> String {
        let Some(ty) = self.engine.find_type(&p.type_name) else {
            return format!("❌ **Не найдено:** тип '{}' не найден\n", p.type_name);
        };
        match self.engine.find_type_member(ty, &p.member_name) {
            Some(d) => format::format_member(&d),
            None => format!(
                "❌ **Не найдено:** у типа '{}' нет члена '{}'\n",
                p.type_name, p.member_name
            ),
        }
    }

    #[tool(
        description = "Все члены типа: методы, свойства и значения системного перечисления. Для обычного типа \
                       enum_values пуст; для типа-перечисления — заполнен, а методы/свойства обычно пусты."
    )]
    pub async fn get_members(&self, Parameters(p): Parameters<TypeNameParams>) -> String {
        let Some(ty) = self.engine.find_type(&p.type_name) else {
            return format!("❌ **Не найдено:** тип '{}' не найден\n", p.type_name);
        };
        format::format_type(ty)
    }

    #[tool(
        description = "Конструкторы типа с полными сигнатурами. Если у типа нет конструкторов — возвращает явное сообщение."
    )]
    pub async fn get_constructors(&self, Parameters(p): Parameters<TypeNameParams>) -> String {
        let Some(ty) = self.engine.find_type(&p.type_name) else {
            return format!("❌ **Не найдено:** тип '{}' не найден\n", p.type_name);
        };
        if !ty.has_constructors() {
            return format!("У типа '{}' нет конструкторов.\n", p.type_name);
        }
        format::format_constructors(&ty.constructors, &ty.name_ru)
    }

    #[tool(
        description = "Проверка значения системного перечисления: 'допустимо ли value_name у type_name'. \
                       Возвращает JSON {valid, type_name, value_name, all_valid_values, similar:[...], message}. \
                       Похожие значения сортируются по убыванию score (расстояние Левенштейна, нормированное)."
    )]
    pub async fn validate_enum(&self, Parameters(p): Parameters<ValidateEnumParams>) -> String {
        let result = validate_enum(&self.index, &p.type_name, &p.value_name);
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
    }

    #[tool(
        description = "Проверка вызова глобального метода: укладывается ли arg_count в одну из перегрузок method_name. \
                       Возвращает JSON {valid, method_name, arg_count, signatures:[...], message}. У метода без \
                       описанных сигнатур (редкий случай) валидация считается warning, valid=true."
    )]
    pub async fn validate_method_call(
        &self,
        Parameters(p): Parameters<ValidateMethodCallParams>,
    ) -> String {
        let result = validate_method_call(&self.index, &p.method_name, p.arg_count);
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
    }

    #[tool(
        description = "Валидация BSL-кода против платформенного контекста. Принимает и целый модуль, \
                       и отдельный фрагмент. Ловит: несуществующие значения системных перечислений; \
                       неизвестные платформенные типы в 'Новый ТипX'; неверное число аргументов \
                       глобальных функций; опечатки платформенных методов и директив (fuzzy-сходство). \
                       Объявленные в самом тексте Процедура/Функция извлекаются через tree-sitter, их \
                       вызовы не считаются опечатками. У каждой находки есть поле confidence (high/low). \
                       Источник модуля — ровно один: source (текст BSL) или path (файл .bsl внутри корня \
                       выгрузки конфигурации, поле root источника имён; читает сервер, поэтому большой \
                       модуль не нужно прогонять через контекст модели). При path параметр module_path \
                       выводится из пути сам, а в ответе появляются source_path, source_module_path, \
                       source_bytes и source_modified — видно, какая версия файла проверена. Строка, \
                       похожая на путь к файлу, в source отвергается с объяснением: раньше она \
                       разбиралась как BSL и давала valid: true без единой проверки. \
                       Параметр level: 1 (default) — только явные имена типов, включая тип переменной из \
                       конструктора (Х = Новый ТипX); 2 — плюс локальный вывод типа переменных; 3 — плюс \
                       тип из возвращаемых значений. Имя переменной, совпавшее с именем платформенного \
                       типа, за тип не принимается: без конструктора и без вывода проверка членов молчит. \
                       Параметр profile: 'strict' \
                       (только high-confidence + level=1, для слабых моделей) или 'full' (все находки, \
                       default). Параметр repo — алиас конфигурации из настроек сервера \
                       ([[symbol_sources]] в config.toml), чьи имена методов учитывать; обязателен, \
                       если на сервере настроена хотя бы одна конфигурация (список доступных алиасов \
                       возвращается отказом при промахе), иначе не нужен — проверка идёт только против \
                       справки платформы. Ловит также имена локальных переменных, занятые свойствами \
                       глобального контекста, и (при переданном module_path модуля формы) членами \
                       ФормаКлиентскогоПриложения — присваивание такому имени падает в рантайме. \
                       Параметр form_attributes — имена реквизитов формы: реквизит перекрывает имя \
                       контекста, и, передав их, вы включаете проверку имён глобального контекста \
                       внутри модуля формы (без него она там выключена). \
                       Отдельно разбирает тексты запросов на языке запросов 1С, записанные \
                       строковыми литералами, и сообщает о неоптимальных конструкциях: \
                       temp_table_without_index — временная таблица участвует в соединении, но \
                       не индексирована (ИНДЕКСИРОВАТЬ ПО); or_in_join_condition — ИЛИ разрывает \
                       условие соединения, из-за чего индекс не применяется; join_with_subquery — \
                       соединение с подзапросом вместо временной таблицы. Запрос, текст которого \
                       собирается конкатенацией с переменными, и запрос, который не удалось \
                       разобрать, не проверяются вовсе. \
                       Возвращает JSON \
                       {valid, errors:[{line,col,kind,confidence,message,suggestion?}]}."
    )]
    pub async fn validate_module(&self, Parameters(p): Parameters<ValidateModuleParams>) -> String {
        // Лимит размера ДО любой работы: stdio-кадр не ограничен, а разбор
        // многомегабайтного текста блокирует воркер на секунды. Файловый вход
        // (`path`) сюда не попадает — у него собственный потолок
        // `module_source::MAX_MODULE_BYTES` (16 МиБ).
        if p.source.len() > MAX_SOURCE_BYTES {
            return err_json(&format!(
                "исходник слишком большой: {} байт (предел {} байт)",
                p.source.len(),
                MAX_SOURCE_BYTES
            ));
        }
        let level = p.level.unwrap_or(self.default_validation_level).clamp(1, 3);
        let profile = match p.profile {
            Some(ref s) => Profile::parse_or_default(Some(s)),
            None => self.default_profile,
        };
        // Источник модуля — ровно один: текст (`source`) или файл (`path`).
        // Пустой `source` считается непереданным: клиент, знающий только путь к
        // модулю, не обязан присылать текст. Строка-путь в `source` отвергается
        // здесь же: раньше она разбиралась как BSL и давала `valid: true` без
        // единой проверки (issue #13).
        let source_given = !p.source.trim().is_empty();
        let path_given = p.path.is_some();
        if source_given && path_given {
            return err_json(
                "передайте ровно один источник модуля: source (текст модуля) или path (файл \
                 внутри корня выгрузки конфигурации).",
            );
        }
        if !source_given && !path_given {
            return err_json(
                "нужен либо source (текст модуля), либо path (файл внутри корня выгрузки \
                 конфигурации).",
            );
        }
        if source_given && module_source::looks_like_path(&p.source) {
            return err_json(&module_source::path_in_source_message(&p.source));
        }
        // Ни одной конфигурации не настроено И клиент не просил repo — обычная проверка
        // против справки платформы, как до появления параметра repo. Остальные случаи
        // (сервер пуст, но repo передан; сервер настроен) идут через resolve_slot — он
        // же формирует и единообразный текст ошибки для этого и для rebuild_symbol_index.
        let slot = if self.sources_snapshot().is_empty() && p.repo.is_none() {
            None
        } else {
            match self.resolve_slot(p.repo.as_deref()) {
                Ok(slot) => Some(slot),
                Err(msg) => return err_json(&msg),
            }
        };
        // Модуль можно прочитать с диска: файл обязан лежать ВНУТРИ корня выгрузки
        // конфигурации (поле `root` источника имён) — иначе инструмент стал бы
        // средством чтения любых файлов машины.
        let from_file: Option<ModuleFile> = match p.path.as_deref() {
            Some(raw) => {
                let root = slot.as_ref().and_then(|s| s.config.root.as_deref());
                let Some(root) = root else {
                    return err_json(
                        "параметр path требует источника имён с полем root — корнем выгрузки \
                         конфигурации: добавьте root в секцию [symbol_source] или \
                         [[symbol_sources]] для нужного repo.",
                    );
                };
                match module_source::read_module(root, raw) {
                    Ok(module) => Some(module),
                    Err(message) => return err_json(&message),
                }
            }
            None => None,
        };
        let source_text: &str = match from_file.as_ref() {
            Some(module) => module.text.as_str(),
            None => p.source.as_str(),
        };
        // Явный module_path клиента приоритетнее выведенного из пути файла.
        let module_path: Option<String> = p
            .module_path
            .clone()
            .or_else(|| from_file.as_ref().map(|m| m.module_path.clone()));
        // Реквизиты формы сверяются регистронезависимо, как и всё в BSL.
        let form_attributes: Option<HashSet<String>> = p
            .form_attributes
            .as_ref()
            .map(|names| names.iter().map(|n| n.to_lowercase()).collect());

        let Some(slot) = slot else {
            let result = validate_module_with_profile(
                &self.index,
                source_text,
                module_path.as_deref(),
                form_attributes.as_ref(),
                level,
                profile,
            );
            return validation_json(&result, from_file.as_ref());
        };
        // Слот найден по точному совпадению repo — значит параметр был Some.
        let repo = p.repo.as_deref().unwrap_or_default();
        // Источника нет или он уже свалился — пробуем поднять заново, не
        // дожидаясь перезапуска процесса. Отступ между попытками внутри.
        let stale = {
            let guard = slot.source.read().await;
            match guard.as_ref() {
                None => true,
                Some(source) => !source.is_healthy(),
            }
        };
        if stale {
            self.try_reconnect(repo, &slot, false).await;
        }
        let guard = slot.source.read().await;
        let source = match guard.as_ref() {
            Some(source) => source,
            // Слот настроен, но источник не поднят: для lite сборка ещё не запускалась,
            // для остальных — подключение не удалось (текст последней ошибки в слоте).
            // Отказывать нельзя: платформенный индекс исправен, и проверки против него
            // от имён конфигурации не зависят — именно так в готовый код проходили
            // выдуманные вызовы. Проверяем против одной платформы, пометив ответ
            // признаком неполноты; находки «метод не объявлен» при этом понижаются до
            // Low — без имён конфигурации туда попадает каждый вызов процедуры
            // глобального общего модуля (на УТ это давало 1420 находок).
            None => {
                let reason = if slot.config.kind == "lite" {
                    format!(
                        "индекс имён конфигурации \"{repo}\" не собран — вызовите \
                         rebuild_symbol_index с repo=\"{repo}\". Проверка выполнена только \
                         против платформенного контекста."
                    )
                } else {
                    let cause = slot
                        .last_error()
                        .map(|e| format!(" Последняя ошибка: {e}."))
                        .unwrap_or_default();
                    format!(
                        "источник имён конфигурации \"{repo}\" не подключён; переподключение \
                         пробовали, оно не удалось.{cause} Следующая попытка — при вызове не \
                         раньше чем через {} с (или сразу через reconnect_symbol_source). \
                         Проверка выполнена только против платформенного контекста.",
                        RECONNECT_COOLDOWN.as_secs()
                    )
                };
                return degraded_json(
                    &self.index,
                    source_text,
                    level,
                    profile,
                    module_path.as_deref(),
                    form_attributes.as_ref(),
                    from_file.as_ref(),
                    reason,
                );
            }
        };
        if !source.is_healthy() {
            return degraded_json(
                &self.index,
                source_text,
                level,
                profile,
                module_path.as_deref(),
                form_attributes.as_ref(),
                from_file.as_ref(),
                format!(
                    "источник имён конфигурации \"{repo}\" недоступен: {}. Переподключение \
                     пробовали, оно не удалось — проверьте code-index; следующая попытка \
                     будет при вызове не раньше чем через {} с (или сразу через \
                     reconnect_symbol_source). Проверка выполнена только против \
                     платформенного контекста.",
                    source.describe(),
                    RECONNECT_COOLDOWN.as_secs()
                ),
            );
        }
        // Вид формы по файлам выгрузки (issue #32): у фрагмента управляемой формы
        // директив компиляции в тексте нет, и без подсказки он считался бы обычной
        // формой, а её контекст — объектом-владельцем. Директивы остаются запасным
        // признаком, когда корня выгрузки нет.
        let form_kind = form_kind_from_dump(slot.config.root.as_deref(), module_path.as_deref());
        let result = validate_module_with_symbols_and_form_kind(
            &self.index,
            source_text,
            level,
            profile,
            module_path.as_deref(),
            form_attributes.as_ref(),
            Some(source.as_ref()),
            form_kind,
        );
        if !source.is_healthy() {
            // Отвалился во время самой валидации (code-index упал на полпути) — часть
            // имён могла быть заменена пустыми ответами, и в `result` могли попасть
            // находки, порождённые именно этим. Отдавать его нельзя; считаем заново
            // против одной платформы — там таких находок не будет по построению.
            return degraded_json(
                &self.index,
                source_text,
                level,
                profile,
                module_path.as_deref(),
                form_attributes.as_ref(),
                from_file.as_ref(),
                format!(
                    "источник имён конфигурации \"{repo}\" отвалился во время проверки: {}. \
                     Результат пересчитан только против платформенного контекста — проверьте \
                     code-index.",
                    source.describe()
                ),
            );
        }
        validation_json(&result, from_file.as_ref())
    }

    #[tool(
        description = "Значения системного перечисления (enum_values). Для типа без enum_values возвращает явный отказ \
                       'тип не является системным перечислением'."
    )]
    pub async fn get_enum_values(&self, Parameters(p): Parameters<TypeNameParams>) -> String {
        let Some(ty) = self.engine.find_type(&p.type_name) else {
            return format!("❌ **Не найдено:** тип '{}' не найден\n", p.type_name);
        };
        if !ty.is_enum() {
            return format!(
                "❌ **Тип не является системным перечислением:** '{}' не имеет enum_values\n",
                p.type_name
            );
        }
        format::format_enum_values(&ty.enum_values, &ty.name_ru)
    }

    #[tool(
        description = "Имена, которые нельзя брать под локальные переменные BSL: они заняты контекстом \
                       модуля. Четыре группы. global_readonly — свойства глобального контекста только \
                       для чтения (Справочники, Документы, Метаданные, …): присваивание падает в \
                       рантайме, в ЛЮБОМ модуле. form_readonly — свойства ФормаКлиентскогоПриложения \
                       только для чтения (Параметры, Элементы, Команды, ЭтотОбъект, …): падает в модуле \
                       управляемой формы. global_writable (РабочаяДата, ГлавныйСтиль) и form_writable \
                       (Заголовок, Модифицированность, …) — свойства, доступные для записи: не падает, но \
                       локальная переменная НЕ создаётся, молча меняется настройка сеанса либо сама форма. \
                       Список берётся из справки той версии платформы, что задана в конфиге сервера \
                       (platform_path), поэтому не устаревает. Имя, занятое параметром процедуры или \
                       реквизитом формы, свободно — оно перекрывает контекст. Возвращает JSON \
                       {global_readonly:[…], global_writable:[…], form_readonly:[…], form_writable:[…], counts:{…}}."
    )]
    pub async fn reserved_names(&self) -> String {
        let readonly_names = |props: &[platform_index::Property], readonly: bool| {
            let mut names: Vec<String> = props
                .iter()
                .filter(|p| p.readonly == readonly)
                .map(|p| p.name_ru.clone())
                .collect();
            names.sort();
            names
        };

        let global_readonly = readonly_names(&self.index.global_properties, true);
        let global_writable = readonly_names(&self.index.global_properties, false);
        let (form_readonly, form_writable) = match self.index.find_type(FORM_TYPE) {
            Some(ty) => (
                readonly_names(&ty.properties, true),
                readonly_names(&ty.properties, false),
            ),
            // Тип не найден — справка другой локали или битый индекс. Молчать нельзя:
            // пустой список выглядит как «занятых имён нет».
            None => return err_json(&format!("тип '{FORM_TYPE}' не найден в справке платформы")),
        };

        let result = serde_json::json!({
            "counts": {
                "global_readonly": global_readonly.len(),
                "global_writable": global_writable.len(),
                "form_readonly": form_readonly.len(),
                "form_writable": form_writable.len(),
            },
            "global_readonly": global_readonly,
            "global_writable": global_writable,
            "form_readonly": form_readonly,
            "form_writable": form_writable,
        });
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
    }

    #[tool(
        description = "Состояние источников имён конфигураций: по каждому настроенному repo — \
                       подключён ли он сейчас, здоров ли, и текст последней ошибки подключения. \
                       Нужен перед началом работы с конкретной конфигурацией: если источник не \
                       поднят, validate_module проверит модуль только против платформенного \
                       контекста (ответ с symbols_available: false), и имена прикладных объектов \
                       проверены не будут. Параметров нет. Возвращает JSON \
                       {ok, sources:[{repo, kind, connected, healthy, state, last_error}]}, где \
                       state — 'ok' | 'not_connected' | 'unhealthy'."
    )]
    pub async fn symbol_sources_status(&self) -> String {
        let sources = self.sources_snapshot();
        let mut out = Vec::with_capacity(sources.len());
        for (repo, slot) in sources.iter() {
            out.push(slot_state_json(repo, slot).await);
        }
        serde_json::json!({"ok": true, "sources": out}).to_string()
    }

    #[tool(
        description = "Повторить подключение источника имён конфигурации, не перезапуская сервер. \
                       Нужен, когда code-index подняли или перезапустили позже bsl-context: сам \
                       сервер пробует переподключиться при обращении, но не чаще чем раз в 15 \
                       секунд, а этот вызов делает попытку немедленно. Параметр repo — алиас \
                       конфигурации ([[symbol_sources]] в config.toml), обязателен. Возвращает \
                       JSON {ok, repo, connected, healthy, state, last_error} — состояние ПОСЛЕ \
                       попытки; ok: false только если repo не настроен."
    )]
    pub async fn reconnect_symbol_source(
        &self,
        Parameters(p): Parameters<ReconnectSymbolSourceParams>,
    ) -> String {
        let repo = match p.repo.as_deref() {
            Some(repo) => repo,
            None => {
                return err_json(&format!(
                    "параметр repo обязателен; доступные конфигурации: {}",
                    self.source_names()
                ))
            }
        };
        // Алиаса может не быть в карте просто потому, что секцию дописали в
        // config.toml уже после старта: тогда перечитываем файл и ищем ещё раз.
        let slot = match self.slot(repo) {
            Some(slot) => Some(slot),
            None if self.config_path.is_some() => {
                if let Err(msg) = self.reload_sources_from_config().await {
                    return err_json(&msg);
                }
                self.slot(repo)
            }
            None => None,
        };
        let slot = match slot {
            Some(slot) => slot,
            None => {
                // Текст отказа — прежний, из resolve_slot, плюс признак того, что
                // файл уже перечитан: без него «не настроена» читается как «в
                // config.toml её нет» без проверки, а проверить как раз и просили.
                let msg = self
                    .resolve_slot(Some(repo))
                    .err()
                    .unwrap_or_else(|| format!("конфигурация \"{repo}\" не настроена"));
                let msg = if self.config_path.is_some() {
                    format!("{msg}. config.toml перечитан; секции с repo \"{repo}\" в нём нет")
                } else {
                    msg
                };
                return err_json(&msg);
            }
        };
        // force: попросили явно — отступ между автоматическими попытками здесь
        // ни при чём, иначе вызов «подними сейчас» молча ничего бы не делал.
        //
        // Попытка уже идёт: не выдаём ok:true по состоянию ДО неё, сообщаем явно.
        if slot.reconnecting.load(Ordering::SeqCst) {
            let state = slot_state_json(repo, &slot).await;
            let mut out = serde_json::Map::new();
            out.insert("ok".to_string(), serde_json::Value::Bool(false));
            out.insert(
                "message".to_string(),
                serde_json::Value::String(
                    "переподключение уже идёт — дождитесь его завершения".to_string(),
                ),
            );
            if let serde_json::Value::Object(fields) = state {
                out.extend(fields);
            }
            return serde_json::Value::Object(out).to_string();
        }
        self.try_reconnect(repo, &slot, true).await;

        // Ответ инструмента — состояние слота плюс собственный флаг `ok`.
        let state = slot_state_json(repo, &slot).await;
        let mut out = serde_json::Map::new();
        out.insert("ok".to_string(), serde_json::Value::Bool(true));
        if let serde_json::Value::Object(fields) = state {
            out.extend(fields);
        }
        serde_json::Value::Object(out).to_string()
    }

    #[tool(
        description = "Перечитать config.toml без перезапуска сервера: применяется список источников \
                       имён конфигураций ([[symbol_sources]]). Нужен, когда секцию источника \
                       дописали или поправили в файле уже после старта и перезапускать сервис \
                       ради этого не хочется: у неизменившегося источника настройки те же, и он \
                       остаётся тем же объектом (кэш и открытая сессия к code-index не теряются), \
                       изменённый или новый подключается заново, а удалённый уходит из карты. \
                       Применяется ТОЛЬКО список источников: platform_path, host, port, \
                       allowed_hosts, [tools].enabled и дефолты проверки прочитаны при старте — \
                       об их изменении пишется предупреждение в журнал, но в силу они вступают \
                       после перезапуска. Параметров нет. Возвращает JSON \
                       {ok, config_path, added:[…], removed:[…], recreated:[…], unchanged:[…], \
                       sources:[{repo, kind, connected, healthy, state, last_error}]}, где state — \
                       'ok' | 'not_connected' | 'unhealthy'; ok: false — сервер запущен без \
                       --config либо файл не читается или не разбирается (карта при этом не \
                       меняется)."
    )]
    pub async fn reload_config(&self) -> String {
        match self.reload_sources_from_config().await {
            Ok(json) => json.to_string(),
            Err(msg) => err_json(&msg),
        }
    }

    #[tool(
        description = "Пересобрать облегчённый индекс имён конфигурации. Работает только при \
                       symbol_source.kind = \"lite\". Пути берутся ИЗ КОНФИГА сервера: `root` — \
                       каталог выгрузки, `db_path` — файл базы (каталог создаётся, если его нет). \
                       Сборка идёт во временный файл и подменяет старую базу целиком: если она \
                       упадёт, рабочая база останется прежней. Параметр repo — алиас конфигурации из \
                       настроек сервера ([[symbol_sources]] в config.toml), чей индекс пересобрать; \
                       обязателен всегда, даже если настроена только одна конфигурация — вызов должен \
                       быть однозначным. Возвращает JSON \
                       {ok, modules, methods, global_modules, elapsed_ms, db_path} либо {ok:false, message}."
    )]
    pub async fn rebuild_symbol_index(
        &self,
        Parameters(p): Parameters<RebuildSymbolIndexParams>,
    ) -> String {
        let slot = match self.resolve_slot(p.repo.as_deref()) {
            Ok(slot) => slot,
            Err(msg) => return err_json(&msg),
        };
        let cfg = slot.config.clone();
        // 1. Пересобирать имеет смысл только собственный индекс.
        if cfg.kind != "lite" {
            return err_json(&format!(
                "symbol_source.kind = \"{}\": источник читает чужой индекс, пересобирать нечего",
                cfg.kind
            ));
        }
        let (Some(root), Some(db_path)) = (cfg.root.clone(), cfg.db_path.clone()) else {
            return err_json(
                "для пересборки нужны symbol_source.root и symbol_source.db_path в config.toml",
            );
        };
        if !root.is_dir() {
            return err_json(&format!(
                "symbol_source.root = {} — каталога нет",
                root.display()
            ));
        }
        // 2. Одна сборка за раз для ЭТОЙ конфигурации — другие слоты пересобираются независимо.
        if slot.rebuilding.swap(true, Ordering::SeqCst) {
            return err_json("пересборка уже идёт");
        }
        // Флаг сбросится и при отмене future (обрыв клиента), и при панике.
        let _rebuilding_guard = FlagGuard(&slot.rebuilding);
        let result = self.rebuild_inner(&slot, &root, &db_path).await;
        match result {
            Ok(json) => json,
            Err(e) => err_json(&format!("{e:#}")),
        }
    }
}

// ── Реализация ServerHandler ───────────────────────────────────────────────

impl ServerHandler for BslContextServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut info = rmcp::model::ServerInfo::default();
        info.instructions = Some(match &self.unavailable_reason {
            None => "MCP-сервер контекста платформы 1С: типы, методы, свойства, \
                 конструкторы, значения системных перечислений."
                .to_string(),
            // Причину видно клиенту в описании сервера, не дожидаясь отказа
            // первого вызова инструмента.
            Some(reason) => {
                let mut maintenance_tools: Vec<String> = self
                    .tool_router
                    .list_all()
                    .into_iter()
                    .filter(|tool| {
                        !PLATFORM_INDEX_TOOLS.contains(&tool.name.as_ref())
                            && self.is_tool_allowed(tool.name.as_ref())
                    })
                    .map(|tool| tool.name.to_string())
                    .collect();
                maintenance_tools.sort();
                let mut instructions = format!(
                    "MCP-сервер контекста платформы 1С: платформенный контекст НЕ загружен — \
                     {reason}. Справочные инструменты отвечают отказом"
                );
                if !maintenance_tools.is_empty() {
                    instructions.push_str(&format!("; доступны {}.", maintenance_tools.join(", ")));
                } else {
                    instructions.push('.');
                }
                instructions
            }
        });
        info.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build();
        let mut impl_info = rmcp::model::Implementation::default();
        impl_info.name = "bsl-context-rs".into();
        impl_info.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = impl_info;
        info
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let mut tools = self.tool_router.list_all();
        tools.retain(|t| self.is_tool_allowed(t.name.as_ref()));
        Ok(rmcp::model::ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        // Проверка белого списка ДО диспетча: модель может позвать инструмент,
        // которого не было в `tools/list` (из системного промпта, из памяти).
        // Намеренно дублирует фильтр в `list_tools`.
        if !self.is_tool_allowed(request.name.as_ref()) {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "инструмент '{}' отключён белым списком [tools].enabled в config.toml",
                    request.name
                ),
                None,
            ));
        }
        // Недоступный индекс — состояние сервера, а не ошибка аргументов:
        // отдаём результат с `is_error`, чтобы модель увидела причину. Инструменты
        // обслуживания (перечитка конфига, источники имён) не трогают индекс и
        // обязаны работать — их в этом списке нет.
        if let Some(reason) = &self.unavailable_reason {
            if PLATFORM_INDEX_TOOLS.contains(&request.name.as_ref()) {
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(format!(
                        "Платформенный контекст не загружен: {reason}"
                    )),
                ]));
            }
        }
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_info_lists_only_allowed_maintenance_tools() {
        let server = BslContextServer::unavailable("причина".into(), 1, Profile::Full)
            .apply_tools_whitelist(&["search".into(), "symbol_sources_status".into()]);
        let instructions = server.get_info().instructions.unwrap();
        assert!(instructions.contains("причина"));
        assert!(instructions.contains("НЕ загружен"));
        assert!(instructions.contains("symbol_sources_status"));
        assert!(!instructions.contains("reload_config"));
        assert!(!instructions.contains("reconnect_symbol_source"));
        assert!(!instructions.contains("rebuild_symbol_index"));
    }

    #[test]
    fn unavailable_info_omits_maintenance_tools_when_all_hidden() {
        let server = BslContextServer::unavailable("причина".into(), 1, Profile::Full)
            .apply_tools_whitelist(&["search".into()]);
        let instructions = server.get_info().instructions.unwrap();
        for name in TOOLS_WITHOUT_INDEX {
            assert!(!instructions.contains(name));
        }
        assert!(!instructions.contains("доступны"));
    }

    #[test]
    fn cold_changes_skip_platform_path_from_cli() {
        let prev = crate::config::Config::default();
        let mut fresh = crate::config::Config {
            platform_path: Some(PathBuf::from("other-platform")),
            ..crate::config::Config::default()
        };
        assert!(cold_changes(&prev, &fresh, true).is_empty());
        assert_eq!(cold_changes(&prev, &fresh, false), ["platform_path"]);
        fresh.port = prev.port + 1;
        assert_eq!(cold_changes(&prev, &fresh, true), ["port"]);
    }

    /// Инструменты, не зависящие от платформенного индекса: обязаны работать,
    /// когда справка не загружена (иначе сервер без `platform_path` нельзя
    /// починить, не перезапустив процесс).
    const TOOLS_WITHOUT_INDEX: [&str; 4] = [
        "reload_config",
        "symbol_sources_status",
        "reconnect_symbol_source",
        "rebuild_symbol_index",
    ];

    /// Разбиение «нужен индекс / не нужен» обязано совпадать с фактическим
    /// набором инструментов: забытый в `PLATFORM_INDEX_TOOLS` инструмент молча
    /// ушёл бы работать с пустой справкой, а лишний — сломал бы инструмент
    /// обслуживания.
    #[test]
    fn tool_partition_matches_router() {
        let server = BslContextServer::new(PlatformIndex::new());
        let mut actual: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        actual.sort();

        let mut expected: Vec<String> = PLATFORM_INDEX_TOOLS
            .iter()
            .chain(TOOLS_WITHOUT_INDEX.iter())
            .map(|name| name.to_string())
            .collect();
        expected.sort();

        assert_eq!(
            actual, expected,
            "набор инструментов разошёлся с разбиением PLATFORM_INDEX_TOOLS/TOOLS_WITHOUT_INDEX"
        );
    }
}
