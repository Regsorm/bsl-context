//! Integration-тесты Phase 4: вызовы tool-методов на реальном `shcntx_ru.hbk`.
//!
//! Поднимаем `BslContextServer` напрямую (без HTTP-транспорта) и проверяем,
//! что Markdown-ответы содержат ожидаемый контент. Это smoke по контракту
//! tool'ов; полный MCP-роутинг проверится в Phase 7 после деплоя.
//!
//! Запуск:
//! ```pwsh
//! $env:BSL_CONTEXT_PLATFORM_PATH = 'C:\Program Files\1cv8\8.3.27.1786'
//! cargo test -p bsl-context-server --test real_mcp -- --nocapture
//! ```

use std::path::PathBuf;

use bsl_context_server::mcp_server::{
    BslContextServer, GetMemberParams, InfoParams, RebuildSymbolIndexParams,
    ReconnectSymbolSourceParams, SearchParams, TypeNameParams, ValidateEnumParams,
    ValidateMethodCallParams, ValidateModuleParams,
};
use platform_index::load_from_hbk;
use rmcp::handler::server::wrapper::Parameters;

fn hbk_path() -> Option<PathBuf> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH").ok().map(PathBuf::from)?;
    let candidates = [root.join("shcntx_ru.hbk"), root.join("bin").join("shcntx_ru.hbk")];
    candidates.into_iter().find(|p| p.exists())
}

async fn make_server() -> Option<BslContextServer> {
    let path = hbk_path()?;
    let index = load_from_hbk(&path).ok()?;
    Some(BslContextServer::new(index))
}

#[tokio::test]
async fn search_finds_real_method() {
    let Some(srv) = make_server().await else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let md = srv
        .search(Parameters(SearchParams {
            query: "СтрНайти".into(),
            limit: Some(5),
        }))
        .await;
    println!("--- search('СтрНайти') ---\n{md}");
    assert!(md.contains("СтрНайти"), "результат должен содержать имя метода");
}

#[tokio::test]
async fn info_returns_type_card() {
    let Some(srv) = make_server().await else { return };
    let md = srv
        .info(Parameters(InfoParams {
            name: "ТаблицаЗначений".into(),
            kind: None,
        }))
        .await;
    println!("--- info('ТаблицаЗначений') ---\n{md}");
    assert!(md.contains("# ТаблицаЗначений"));
    assert!(md.contains("## Методы"));
}

#[tokio::test]
async fn get_member_returns_method() {
    let Some(srv) = make_server().await else { return };
    let md = srv
        .get_member(Parameters(GetMemberParams {
            type_name: "ТаблицаЗначений".into(),
            member_name: "Добавить".into(),
        }))
        .await;
    println!("--- get_member(ТаблицаЗначений.Добавить) ---\n{md}");
    assert!(md.contains("Добавить"));
}

#[tokio::test]
async fn get_members_value_table() {
    let Some(srv) = make_server().await else { return };
    let md = srv
        .get_members(Parameters(TypeNameParams {
            type_name: "ТаблицаЗначений".into(),
        }))
        .await;
    println!("--- get_members(ТаблицаЗначений) ---\n{md}");
    assert!(md.contains("# ТаблицаЗначений"));
    assert!(md.contains("## Методы"));
    assert!(md.contains("## Свойства"));
}

#[tokio::test]
async fn get_constructors_returns_real_signatures() {
    let Some(srv) = make_server().await else { return };
    let md = srv
        .get_constructors(Parameters(TypeNameParams {
            type_name: "ТаблицаЗначений".into(),
        }))
        .await;
    println!("--- get_constructors(ТаблицаЗначений) ---\n{md}");
    assert!(
        md.contains("Конструктор"),
        "результат должен содержать заголовок 'Конструктор'"
    );
    assert!(md.contains("Новый ТаблицаЗначений"));
}

#[tokio::test]
async fn get_enum_values_canonical_638() {
    let Some(srv) = make_server().await else { return };
    let md = srv
        .get_enum_values(Parameters(TypeNameParams {
            type_name: "ТипРазмещенияТекстаТабличногоДокумента".into(),
        }))
        .await;
    println!("--- get_enum_values(ТипРазмещенияТекстаТабличногоДокумента) ---\n{md}");
    for name in ["Авто", "Забивать", "Обрезать", "Переносить"] {
        assert!(md.contains(name), "должен присутствовать '{name}'");
    }
}

#[tokio::test]
async fn get_enum_values_rejects_non_enum_type() {
    let Some(srv) = make_server().await else { return };
    let md = srv
        .get_enum_values(Parameters(TypeNameParams {
            type_name: "ТаблицаЗначений".into(),
        }))
        .await;
    println!("--- get_enum_values(ТаблицаЗначений) ---\n{md}");
    assert!(md.contains("не является системным перечислением"));
}

#[tokio::test]
async fn validate_enum_canonical_638() {
    // Канонический баг #638: 'Перенос' нет, должно быть 'Переносить'.
    let Some(srv) = make_server().await else { return };
    let json = srv
        .validate_enum(Parameters(ValidateEnumParams {
            type_name: "ТипРазмещенияТекстаТабличногоДокумента".into(),
            value_name: "Перенос".into(),
        }))
        .await;
    println!("--- validate_enum(...Перенос) ---\n{json}");
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(v["valid"], false);
    let similar: Vec<String> = v["similar"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        similar.iter().any(|s| s == "Переносить"),
        "должна быть подсказка 'Переносить', получено {similar:?}"
    );
}

#[tokio::test]
async fn validate_enum_accepts_valid_value() {
    let Some(srv) = make_server().await else { return };
    let json = srv
        .validate_enum(Parameters(ValidateEnumParams {
            type_name: "ТипРазмещенияТекстаТабличногоДокумента".into(),
            value_name: "Переносить".into(),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["valid"], true);
}

#[tokio::test]
async fn validate_method_call_rejects_extra_argument() {
    let Some(srv) = make_server().await else { return };
    // У 'СтрНайти' максимум 5 аргументов (Строка, Подстрока, НаправлениеПоиска,
    // НачальнаяПозиция, НомерВхождения). 6 аргументов должно дать valid=false.
    let json = srv
        .validate_method_call(Parameters(ValidateMethodCallParams {
            method_name: "СтрНайти".into(),
            arg_count: 6,
        }))
        .await;
    println!("--- validate_method_call(СтрНайти, 6) ---\n{json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["valid"], false);
    assert!(
        v["signatures"].as_array().unwrap().len() >= 1,
        "должна быть минимум одна сигнатура"
    );
}

#[tokio::test]
async fn validate_method_call_accepts_normal_call() {
    let Some(srv) = make_server().await else { return };
    let json = srv
        .validate_method_call(Parameters(ValidateMethodCallParams {
            method_name: "СтрНайти".into(),
            arg_count: 2,
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["valid"], true);
}

#[tokio::test]
async fn tools_whitelist_hides_and_blocks_tools() {
    let Some(srv) = make_server().await else {
        eprintln!("skip: hbk не найден");
        return;
    };
    // Без белого списка доступно всё.
    assert!(srv.is_tool_allowed("search"));
    assert!(srv.is_tool_allowed("validate_module"));

    // Неизвестное имя в списке не роняет сервер и ничего не разрешает.
    // Клонируем, а не грузим hbk второй раз: загрузка индекса — десятки секунд.
    let srv2 = srv.clone().apply_tools_whitelist(&[
        "validate_module".to_string(),
        "нет_такого_инструмента".to_string(),
    ]);
    assert!(srv2.is_tool_allowed("validate_module"));
    assert!(!srv2.is_tool_allowed("search"));

    // С белым списком — только перечисленное.
    let srv = srv.apply_tools_whitelist(&["validate_module".to_string()]);
    assert!(srv.is_tool_allowed("validate_module"));
    assert!(!srv.is_tool_allowed("search"));
}

#[tokio::test]
async fn rebuild_symbol_index_refuses_when_source_is_not_lite() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    // Слот сконфигурирован, но источник — не lite (например, прямое чтение базы
    // code-index): пересобирать через этот инструмент нечего, это чужой индекс.
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "code_index_db".to_string();
    cfg.db_path = Some(std::path::PathBuf::from(r"C:\Repo1C\.code-index\index.db"));
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Ok(None))]);

    let json = srv
        .rebuild_symbol_index(Parameters(RebuildSymbolIndexParams { repo: Some("ut".to_string()) }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["message"].as_str().unwrap().contains("пересобирать нечего"));
}

#[tokio::test]
async fn rebuild_symbol_index_builds_database_and_creates_directory() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let corpus = std::env::var("BSL_CONTEXT_CORPUS_PATH").unwrap_or_default();
    let root = std::path::Path::new(&corpus);
    if !root.exists() { eprintln!("skip: корпуса нет — задайте BSL_CONTEXT_CORPUS_PATH"); return; }
    // Каталога заведомо нет — инструмент обязан его создать.
    let dir = std::env::temp_dir().join("bslctx_rebuild_test").join("nested");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    let db = dir.join("wms_lite.db");

    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "lite".to_string();
    cfg.root = Some(root.to_path_buf());
    cfg.db_path = Some(db.clone());
    let srv = srv.with_sources(vec![("wms".to_string(), cfg, Ok(None))]);

    let json = srv
        .rebuild_symbol_index(Parameters(RebuildSymbolIndexParams { repo: Some("wms".to_string()) }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], true, "ответ: {json}");
    assert!(v["modules"].as_u64().unwrap() > 0);
    assert!(db.exists(), "база не создана");
    // Источник подменён в памяти.
    assert!(srv.sources["wms"].source.read().await.is_some());
    // Временный файл убран.
    assert!(!db.with_extension("db.tmp").exists());

    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

/// Источник в слоте пуст, но база на диске есть: валидация обязана поднять его
/// сама. До этой правки слот с `None` оставался пустым до перезапуска процесса —
/// сетевой источник ронял свой `healthy` навсегда, а пересоздать его было нечем.
#[tokio::test]
async fn validate_module_reconnects_source_on_demand() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let dir = std::env::temp_dir().join("bslctx_reconnect_test");
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("dump");
    let module = root.join("base/CommonModules/Гло/Ext/Module.bsl");
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    std::fs::write(&module, "Процедура ИмяИзГло() Экспорт\nКонецПроцедуры\n").unwrap();
    std::fs::write(
        root.join("base/CommonModules/Гло.xml"),
        "<?xml version=\"1.0\"?>\n<MetaDataObject><CommonModule><Properties><Name>Гло</Name><Global>true</Global></Properties></CommonModule></MetaDataObject>\n",
    )
    .unwrap();
    let db = dir.join("lite.db");
    lite_index::build(&root, &db, 0).expect("сборка индекса");

    // Слот настроен и база на месте, но источник в памяти НЕ поднят.
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "lite".to_string();
    cfg.root = Some(root.clone());
    cfg.db_path = Some(db.clone());
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Ok(None))]);
    assert!(srv.sources["ut"].source.read().await.is_none());

    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\n\tИмяИзГло();\nКонецПроцедуры".into(),
            level: None,
            profile: None,
            module_path: None,
            form_attributes: None,
            repo: Some("ut".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        v.get("valid").is_some(),
        "ожидалась валидация, а не отказ: {json}"
    );
    assert!(
        srv.sources["ut"].source.read().await.is_some(),
        "источник должен быть поднят на лету"
    );
    // Имя из глобального общего модуля источник знает — ложной находки нет.
    assert_eq!(v["valid"], true, "неожиданные находки: {json}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn validate_module_rejects_unknown_repo() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "lite".to_string();
    cfg.db_path = Some(std::path::PathBuf::from(r"C:\tools\bsl-context\ut_lite.db"));
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Ok(None))]);

    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\nКонецПроцедуры".into(),
            level: None,
            profile: None,
            module_path: None,
            form_attributes: None,
            repo: Some("нет-такого".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], false);
    assert!(
        v["message"].as_str().unwrap().contains("ut"),
        "в сообщении должны быть перечислены доступные алиасы: {json}"
    );
}

#[tokio::test]
async fn validate_module_requires_repo_when_sources_configured() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "lite".to_string();
    cfg.db_path = Some(std::path::PathBuf::from(r"C:\tools\bsl-context\ut_lite.db"));
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Ok(None))]);

    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\nКонецПроцедуры".into(),
            level: None,
            profile: None,
            module_path: None,
            form_attributes: None,
            repo: None,
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["message"].as_str().unwrap().contains("repo обязателен"));
}

#[tokio::test]
async fn validate_module_without_sources_checks_platform_only() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    // Ни одной конфигурации не настроено — repo не нужен, проверка идёт только
    // против справки платформы (как до появления параметра repo).
    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\n\tА = ТипРазмещенияТекстаТабличногоДокумента.Перенос;\nКонецПроцедуры".into(),
            level: None,
            profile: None,
            module_path: None,
            form_attributes: None,
            repo: None,
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("valid").is_some(), "ожидался обычный результат валидации: {json}");
}

#[tokio::test]
async fn validate_module_degrades_when_lite_index_not_built() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    // Слот настроен (kind = "lite"), но источник — None: rebuild_symbol_index ни разу
    // не запускали. Отказывать нельзя: платформенный индекс исправен, и проверки
    // против него от имён конфигурации не зависят. Ответ — находки плюс признак
    // неполноты, а не {ok:false}.
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "lite".to_string();
    cfg.db_path = Some(std::path::PathBuf::from(r"C:\tools\bsl-context\ut_lite.db"));
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Ok(None))]);

    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\nКонецПроцедуры".into(),
            level: None,
            profile: None,
            module_path: None,
            form_attributes: None,
            repo: Some("ut".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("valid").is_some(), "ожидалась валидация, а не отказ: {json}");
    assert_eq!(v["symbols_available"], false, "признак неполноты обязателен: {json}");
    assert!(
        v["degraded_reason"]
            .as_str()
            .unwrap()
            .contains("rebuild_symbol_index"),
        "причина должна указывать на rebuild_symbol_index: {json}"
    );
}

/// §4 ТЗ: платформенные находки не теряются при неподнятом источнике имён.
///
/// Проверочный модуль из ТЗ: `СЕГОДНЯ()` — такой функции в BSL нет. Раньше
/// вызов отвечал `{ok:false}`, и выдуманный метод проходил в готовый код.
#[tokio::test]
async fn validate_module_degraded_still_finds_invented_call() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "code_index_mcp".to_string();
    cfg.url = Some("http://127.0.0.1:1/mcp".to_string());
    cfg.repo = Some("ut".to_string());
    let srv = srv.with_sources(vec![(
        "ut".to_string(),
        cfg,
        Err("code-index mcp: initialize не прошёл".to_string()),
    )]);

    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Функция ПолучитьДанные() Экспорт\n\tВозврат СЕГОДНЯ();\nКонецФункции".into(),
            level: None,
            profile: Some("full".to_string()),
            module_path: None,
            form_attributes: None,
            repo: Some("ut".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["symbols_available"], false, "признак неполноты обязателен: {json}");
    let errors = v["errors"].as_array().expect("errors обязателен");
    let finding = errors
        .iter()
        .find(|e| e["message"].as_str().unwrap_or_default().contains("СЕГОДНЯ"))
        .unwrap_or_else(|| panic!("находка по СЕГОДНЯ должна остаться: {json}"));
    // §4 п.2: находка остаётся, но с пониженной уверенностью — без имён
    // конфигурации утверждать «метода нет нигде» валидатор не вправе.
    assert_eq!(finding["confidence"], "low", "уверенность должна быть понижена: {json}");
    // Текст последней ошибки подключения виден вызывающему, а не только в журнале.
    // Ошибка здесь — от повторной попытки (её сервер делает перед отказом), а не
    // та, что передана в слот на старте: смысл проверки в том, что причина названа.
    assert!(
        v["degraded_reason"].as_str().unwrap().contains("initialize"),
        "причина отказа источника должна быть в ответе: {json}"
    );
}

/// §8 п.3 ТЗ: без имён конфигурации ни одной находки `UndeclaredMethod`
/// с высокой уверенностью — иначе повторяется случай с 1420 ложными находками
/// на каждый вызов процедуры глобального общего модуля.
#[tokio::test]
async fn validate_module_degraded_has_no_high_undeclared() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "code_index_mcp".to_string();
    cfg.url = Some("http://127.0.0.1:1/mcp".to_string());
    cfg.repo = Some("ut".to_string());
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Err("нет связи".to_string()))]);

    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\n\tЗаписьЖурналаРегистрацииСлужебная();\n\t\
                     ПолучитьНастройкуПользователяСлужебная();\nКонецПроцедуры"
                .into(),
            level: None,
            profile: Some("full".to_string()),
            module_path: None,
            form_attributes: None,
            repo: Some("ut".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let high_undeclared = v["errors"]
        .as_array()
        .expect("errors обязателен")
        .iter()
        .filter(|e| e["kind"] == "undeclared_method" && e["confidence"] == "high")
        .count();
    assert_eq!(high_undeclared, 0, "лавина ложных High не должна вернуться: {json}");
}

/// §7.1 ТЗ: состояние источников видно отдельным вызовом, без разбора текста
/// сообщения об ошибке.
#[tokio::test]
async fn symbol_sources_status_reports_state_and_error() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "code_index_mcp".to_string();
    cfg.url = Some("http://127.0.0.1:1/mcp".to_string());
    cfg.repo = Some("ut".to_string());
    let srv = srv.with_sources(vec![(
        "ut".to_string(),
        cfg,
        Err("initialize не прошёл".to_string()),
    )]);

    let json = srv.symbol_sources_status().await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], true);
    let src = &v["sources"][0];
    assert_eq!(src["repo"], "ut");
    assert_eq!(src["kind"], "code_index_mcp");
    assert_eq!(src["connected"], false);
    assert_eq!(src["state"], "not_connected");
    assert!(
        src["last_error"].as_str().unwrap().contains("initialize"),
        "текст последней ошибки должен быть в ответе: {json}"
    );
}

/// §7.1 ТЗ: попытку подключения можно повторить снаружи. Источника по адресу
/// нет, поэтому проверяется сам факт попытки и внятность ответа, а не успех.
#[tokio::test]
async fn reconnect_symbol_source_reports_result() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "code_index_mcp".to_string();
    cfg.url = Some("http://127.0.0.1:1/mcp".to_string());
    cfg.repo = Some("ut".to_string());
    cfg.timeout_ms = 300;
    let srv = srv.with_sources(vec![("ut".to_string(), cfg, Err("нет связи".to_string()))]);

    let json = srv
        .reconnect_symbol_source(Parameters(ReconnectSymbolSourceParams {
            repo: Some("ut".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], true, "ok:false только когда repo не настроен: {json}");
    assert_eq!(v["repo"], "ut");
    assert_eq!(v["state"], "not_connected");
    assert!(v["last_error"].is_string(), "причина обязана быть названа: {json}");

    // repo не настроен — вот это уже отказ инструмента.
    let json = srv
        .reconnect_symbol_source(Parameters(ReconnectSymbolSourceParams {
            repo: Some("нет-такого".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], false, "неизвестный repo — отказ: {json}");
}

#[tokio::test]
async fn validate_module_rejects_repo_when_no_sources_configured() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    // Сервер вообще без слотов (make_server их не настраивает), но клиент явно
    // просит repo — отказ должен называть причину, а не тихо съесть параметр.
    let json = srv
        .validate_module(Parameters(ValidateModuleParams {
            source: "Процедура Тест()\nКонецПроцедуры".into(),
            level: None,
            profile: None,
            module_path: None,
            form_attributes: None,
            repo: Some("ut".to_string()),
        }))
        .await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], false);
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("не настроено ни одной конфигурации"),
        "сообщение: {json}"
    );
}
