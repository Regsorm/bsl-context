//! MCP-сервер: базовые tools для запроса контекста платформы 1С.
//!
//! Phase 4 — 6 tools: `search`, `info`, `getMember`, `getMembers`,
//! `getConstructors`, `getEnumValues`. Все возвращают Markdown-строку,
//! сформированную через [`platform_index::format`].
//!
//! Phase 5 (когда подключим валидаторы) — добавит `validateEnum`,
//! `validateMethodCall`. Phase 6 — `validateExpression`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use bsl_validator::{
    validate_enum, validate_method_call, validate_module_with_profile,
    validate_module_with_symbols, Profile, SymbolSource,
};
use platform_index::{format, Definition, PlatformIndex, SearchEngine};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_router, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Слот одного источника имён: конфиг, сам источник (пересборка подменяет его на
/// ходу) и флаг «идёт пересборка». Флаг на слот, а не на сервер: пересборка индекса
/// одной конфигурации не должна мешать работе с другой.
pub struct SourceSlot {
    pub config: crate::config::SymbolSourceConfig,
    /// Под `RwLock`, потому что `rebuild_symbol_index` подменяет источник на ходу.
    /// `tokio`-версия: блокировка переживает `await` вокруг сборки индекса.
    pub source: tokio::sync::RwLock<Option<Arc<dyn SymbolSource>>>,
    rebuilding: AtomicBool,
}

impl SourceSlot {
    pub fn new(
        config: crate::config::SymbolSourceConfig,
        source: Option<Arc<dyn SymbolSource>>,
    ) -> Self {
        Self {
            config,
            source: tokio::sync::RwLock::new(source),
            rebuilding: AtomicBool::new(false),
        }
    }
}

/// Состояние MCP-сервера: индекс + поисковый движок (готовы к чтению).
#[derive(Clone)]
pub struct BslContextServer {
    pub index: Arc<PlatformIndex>,
    pub engine: Arc<SearchEngine>,
    /// Дефолтный уровень валидации, если клиент не передал `level` в `validate_module`.
    /// Берётся из `config.toml` (поле `default_validation_level`), кламп в `[1..=2]`.
    pub default_validation_level: u8,
    /// Дефолтный профиль потребителя, если клиент не передал `profile`
    /// в `validate_module`. Берётся из `config.toml` (поле `default_profile`).
    pub default_profile: Profile,
    /// Именованные источники имён конфигураций. Ключ — алиас: это значение параметра
    /// `repo` у `validate_module`/`rebuild_symbol_index`. Пустая карта — конфигураций
    /// не настроено, валидация идёт только против справки платформы.
    pub sources: Arc<BTreeMap<String, SourceSlot>>,
    /// Белый список инструментов из `[tools].enabled`. `None` — фильтр выключен.
    /// `Arc`, потому что сервер клонируется на каждый запрос.
    allowed_tools: Option<Arc<BTreeSet<String>>>,
    tool_router: ToolRouter<Self>,
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
            sources: Arc::new(BTreeMap::new()),
            allowed_tools: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Подключить именованные источники имён (по одному на конфигурацию). Вызывается
    /// один раз на старте: карта дальше не меняется, меняется только содержимое слотов
    /// (пересборка индекса конкретной конфигурации).
    pub fn with_sources(
        mut self,
        slots: Vec<(String, crate::config::SymbolSourceConfig, Option<Arc<dyn SymbolSource>>)>,
    ) -> Self {
        let map = slots
            .into_iter()
            .map(|(name, config, source)| (name, SourceSlot::new(config, source)))
            .collect();
        self.sources = Arc::new(map);
        self
    }

    /// Применить белый список инструментов (`[tools].enabled` из config.toml).
    ///
    /// Пустой список — фильтр выключен, доступны все инструменты. Неизвестные
    /// имена старту не мешают: пишется предупреждение, сервер работает (иначе
    /// опечатка в конфиге роняла бы сервис).
    pub fn apply_tools_whitelist(mut self, enabled: &[String]) -> Self {
        if enabled.is_empty() {
            tracing::info!("[tools].enabled пуст — белый список выключен, доступны все инструменты");
            return self;
        }
        let known: BTreeSet<String> = self
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let allowed: BTreeSet<String> = enabled.iter().cloned().collect();
        let unknown: Vec<&str> = allowed
            .iter()
            .filter(|n| !known.contains(*n))
            .map(|s| s.as_str())
            .collect();
        if !unknown.is_empty() {
            tracing::warn!(
                ?unknown,
                "[tools].enabled содержит неизвестные имена инструментов (опечатка?) — они ни на что не повлияют"
            );
        }
        tracing::info!(
            known = allowed.len() - unknown.len(),
            listed = allowed.len(),
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
        self.sources.keys().cloned().collect::<Vec<_>>().join(", ")
    }

    /// Найти конфигурацию по алиасу. `repo` обязателен, когда настроена хотя бы одна:
    /// молча подставлять единственную нельзя — вызов должен быть однозначным.
    fn resolve_slot(&self, repo: Option<&str>) -> Result<&SourceSlot, String> {
        if self.sources.is_empty() {
            return Err(
                "на сервере не настроено ни одной конфигурации: ни выгрузки, ни источника имён \
                 (секция [[symbol_sources]] в config.toml)"
                    .to_string(),
            );
        }
        match repo {
            Some(name) => self.sources.get(name).ok_or_else(|| {
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

    /// Собрать индекс во временный файл, снять старый источник, подменить файл,
    /// открыть новый. Старый источник снимается ДО подмены: SQLite держит файл
    /// открытым, и на Windows переименовать поверх него нельзя.
    async fn rebuild_inner(&self, slot: &SourceSlot, root: &Path, db_path: &Path) -> anyhow::Result<String> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
        }
        let tmp = db_path.with_extension("db.tmp");

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
        let swapped = tokio::task::spawn_blocking(move || -> anyhow::Result<symbol_source::LiteSource> {
            if db_c.exists() {
                std::fs::remove_file(&db_c)
                    .with_context(|| format!("не удалось удалить старую базу {}", db_c.display()))?;
            }
            std::fs::rename(&tmp_c, &db_c)
                .with_context(|| format!("не удалось переместить {} → {}", tmp_c.display(), db_c.display()))?;
            symbol_source::LiteSource::open(&db_c).context("не удалось открыть свежий индекс")
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
            "db_path": db_path.display().to_string(),
        })
        .to_string())
    }
}

/// Отказ инструмента: не паника и не пустой ответ, а внятная причина.
fn err_json(message: &str) -> String {
    serde_json::json!({"ok": false, "message": message}).to_string()
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
    #[serde(
        alias = "bslModule",
        alias = "module",
        alias = "bslSnippet",
        alias = "snippet",
        alias = "code"
    )]
    pub source: String,
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
    /// методы модуля объекта-владельца внешней обработки.
    #[serde(alias = "modulePath")]
    pub module_path: Option<String>,
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

// ── Tools ──────────────────────────────────────────────────────────────────

#[tool_router]
impl BslContextServer {
    #[tool(
        description = "Нечёткий поиск по платформенному контексту: типы, глобальные методы, глобальные свойства. \
                       Префиксное совпадение, fallback word-order и подстрока. Возвращает Markdown."
    )]
    pub async fn search(&self, Parameters(p): Parameters<SearchParams>) -> String {
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
            Some("type") => self.engine.find_type(&p.name).cloned().map(Definition::Type),
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
                       Параметр level: 1 (default) — только явные имена типов; 2 — плюс локальный вывод \
                       типа переменных; 3 — плюс тип из возвращаемых значений. Параметр profile: 'strict' \
                       (только high-confidence + level=1, для слабых моделей) или 'full' (все находки, \
                       default). Параметр repo — алиас конфигурации из настроек сервера \
                       ([[symbol_sources]] в config.toml), чьи имена методов учитывать; обязателен, \
                       если на сервере настроена хотя бы одна конфигурация (список доступных алиасов \
                       возвращается отказом при промахе), иначе не нужен — проверка идёт только против \
                       справки платформы. Возвращает JSON \
                       {valid, errors:[{line,col,kind,confidence,message,suggestion?}]}."
    )]
    pub async fn validate_module(
        &self,
        Parameters(p): Parameters<ValidateModuleParams>,
    ) -> String {
        let level = p
            .level
            .unwrap_or(self.default_validation_level)
            .clamp(1, 3);
        let profile = match p.profile {
            Some(ref s) => Profile::parse_or_default(Some(s)),
            None => self.default_profile,
        };
        // Ни одной конфигурации не настроено И клиент не просил repo — обычная проверка
        // против справки платформы, как до появления параметра repo. Остальные случаи
        // (сервер пуст, но repo передан; сервер настроен) идут через resolve_slot — он
        // же формирует и единообразный текст ошибки для этого и для rebuild_symbol_index.
        let slot = if self.sources.is_empty() && p.repo.is_none() {
            None
        } else {
            match self.resolve_slot(p.repo.as_deref()) {
                Ok(slot) => Some(slot),
                Err(msg) => return err_json(&msg),
            }
        };
        let Some(slot) = slot else {
            let result = validate_module_with_profile(&self.index, &p.source, level, profile);
            return serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string());
        };
        // Слот найден по точному совпадению repo — значит параметр был Some.
        let repo = p.repo.as_deref().unwrap_or_default();
        let guard = slot.source.read().await;
        let source = match guard.as_ref() {
            Some(source) => source,
            // Слот настроен, но источник не поднят: для lite сборка ещё не запускалась,
            // для остальных — создание источника упало на старте (смотри лог сервера).
            // Тихая валидация без имён здесь недопустима — именно так на УТ получалась
            // 1420 ложных находок «метод не объявлен» на каждый вызов процедуры.
            None => {
                return if slot.config.kind == "lite" {
                    err_json(&format!(
                        "индекс имён конфигурации \"{repo}\" не собран — вызовите \
                         rebuild_symbol_index с repo=\"{repo}\""
                    ))
                } else {
                    err_json(&format!(
                        "источник имён конфигурации \"{repo}\" не подключён — смотрите ошибку \
                         в журнале сервера"
                    ))
                };
            }
        };
        if !source.is_healthy() {
            return err_json(&format!(
                "источник имён конфигурации \"{repo}\" недоступен: {}. Проверьте code-index.",
                source.describe()
            ));
        }
        let result = validate_module_with_symbols(
            &self.index,
            &p.source,
            level,
            profile,
            p.module_path.as_deref(),
            Some(source.as_ref()),
        );
        if !source.is_healthy() {
            // Отвалился во время самой валидации (code-index упал на полпути) — часть
            // имён могла быть заменена пустыми ответами. Лучше явный отказ, чем
            // заведомо неполный результат.
            return err_json(&format!(
                "источник имён конфигурации \"{repo}\" недоступен: {}. Проверьте code-index.",
                source.describe()
            ));
        }
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
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
            return err_json("для пересборки нужны symbol_source.root и symbol_source.db_path в config.toml");
        };
        if !root.is_dir() {
            return err_json(&format!("symbol_source.root = {} — каталога нет", root.display()));
        }
        // 2. Одна сборка за раз для ЭТОЙ конфигурации — другие слоты пересобираются независимо.
        if slot.rebuilding.swap(true, Ordering::SeqCst) {
            return err_json("пересборка уже идёт");
        }
        let result = self.rebuild_inner(slot, &root, &db_path).await;
        slot.rebuilding.store(false, Ordering::SeqCst);
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
        info.instructions = Some(
            "MCP-сервер контекста платформы 1С: типы, методы, свойства, конструкторы, значения системных перечислений.".into(),
        );
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
        let mut result = rmcp::model::ListToolsResult::default();
        result.tools = tools;
        Ok(result)
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
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}
