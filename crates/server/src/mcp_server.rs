//! MCP-сервер: базовые tools для запроса контекста платформы 1С.
//!
//! Phase 4 — 6 tools: `search`, `info`, `getMember`, `getMembers`,
//! `getConstructors`, `getEnumValues`. Все возвращают Markdown-строку,
//! сформированную через [`platform_index::format`].
//!
//! Phase 5 (когда подключим валидаторы) — добавит `validateEnum`,
//! `validateMethodCall`. Phase 6 — `validateExpression`.

use std::sync::Arc;

use bsl_validator::{
    validate_enum, validate_expression_with_profile, validate_method_call,
    validate_module_with_profile, Profile,
};
use platform_index::{format, Definition, PlatformIndex, SearchEngine};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_router, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Состояние MCP-сервера: индекс + поисковый движок (готовы к чтению).
#[derive(Clone)]
pub struct BslContextServer {
    pub index: Arc<PlatformIndex>,
    pub engine: Arc<SearchEngine>,
    /// Дефолтный уровень валидации, если клиент не передал `level` в `validate_expression`.
    /// Берётся из `config.toml` (поле `default_validation_level`), кламп в `[1..=2]`.
    pub default_validation_level: u8,
    /// Дефолтный профиль потребителя, если клиент не передал `profile`
    /// в `validate_expression`. Берётся из `config.toml` (поле `default_profile`).
    pub default_profile: Profile,
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
            tool_router: Self::tool_router(),
        }
    }
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
    /// Текст ЦЕЛОГО BSL-модуля (общий модуль, модуль объекта, модуль формы и т.п.).
    /// В отличие от `validate_expression`, здесь валидатор сам извлекает
    /// объявленные в модуле процедуры/функции через tree-sitter и не путает их
    /// вызовы с опечатками платформенных методов.
    #[serde(alias = "bslModule", alias = "module", alias = "code")]
    pub source: String,
    /// Уровень валидации — семантика та же, что у `validate_expression`
    /// (1 — базовый, 2 — с local type inference, 3 — с return-type tracking).
    /// Клампится в `[1..=3]`.
    pub level: Option<u8>,
    /// Профиль потребителя — `"full"` (все находки) / `"strict"` (только High
    /// + форсированный level=1). Семантика та же, что у `validate_expression`.
    pub profile: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ValidateExpressionParams {
    /// Фрагмент BSL-кода (выражение, оператор или несколько строк).
    #[serde(alias = "bslSnippet", alias = "snippet", alias = "code")]
    pub source: String,
    /// Уровень валидации:
    /// `1` (default) — статический анализ ссылок с явным именем типа в исходнике;
    /// `2` — дополнительно локальный type inference (Phase 8 MVP) для переменных,
    /// присвоенных через `Новый`, `ТипX.ЗначениеY` или аннотацию `// @type ТипX`;
    /// `3` — дополнительно return-type tracking (Уровень 2.5): тип переменной из
    /// возвращаемого типа метода/свойства и цепочек `Запрос.Выполнить().Выбрать()`,
    /// а при заданном `base` — реквизиты справочников/документов из метаданных
    /// конфигурации. Чем выше уровень, тем больше находок и потенциальных
    /// false-positive — поэтому за флагом.
    pub level: Option<u8>,
    /// Профиль потребителя (карточка-decision #1230):
    /// `"full"` (default) — все находки, `level` как передан; для сильной модели,
    /// которая сама отбросит сомнительные.
    /// `"strict"` — только high-confidence находки (`unknown_enum_value`,
    /// `wrong_argument_count`) и форсированный `level=1`; для слабых моделей
    /// (LibreChat/DeepSeek), чтобы ложное срабатывание не приводило к зацикливанию.
    /// Неизвестное значение трактуется как `"full"`.
    pub profile: Option<String>,
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
        description = "Phase 6 (Уровень 1): валидация BSL-фрагмента против платформенного контекста. \
                       Ловит несуществующие значения системных перечислений, неизвестные платформенные типы в \
                       'Новый ТипX', неверное число аргументов глобальных функций. Не делает inter-procedural \
                       type inference (это Уровень 2/3, пост-MVP). У каждой находки есть поле confidence \
                       (high/low). Параметр profile: 'strict' (только high-confidence + level=1, для слабых \
                       моделей) или 'full' (все находки, default). Возвращает JSON \
                       {valid, errors:[{line,col,kind,confidence,message,suggestion?}]}."
    )]
    pub async fn validate_expression(
        &self,
        Parameters(p): Parameters<ValidateExpressionParams>,
    ) -> String {
        let level = p
            .level
            .unwrap_or(self.default_validation_level)
            .clamp(1, 3);
        let profile = match p.profile {
            Some(ref s) => Profile::parse_or_default(Some(s)),
            None => self.default_profile,
        };
        let result = validate_expression_with_profile(&self.index, &p.source, level, profile);
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
    }

    #[tool(
        description = "Валидация ЦЕЛОГО BSL-модуля против платформенного контекста. Отличие от \
                       validate_expression: сервер сам через tree-sitter извлекает объявления \
                       Процедура/Функция и не считает их вызовы опечатками платформенных методов. \
                       Ловит: несуществующие enum-значения; неизвестные типы в 'Новый'; неверное \
                       число аргументов глобальных функций; опечатки платформенных методов \
                       (fuzzy-сходство). Уровни/профили — как у validate_expression. Возвращает \
                       JSON {valid, errors:[{line,col,kind,confidence,message,suggestion?}]}."
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
        let result = validate_module_with_profile(&self.index, &p.source, level, profile);
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
        let mut result = rmcp::model::ListToolsResult::default();
        result.tools = self.tool_router.list_all();
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}
