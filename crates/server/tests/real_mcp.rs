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
    BslContextServer, GetMemberParams, InfoParams, SearchParams, TypeNameParams,
    ValidateEnumParams, ValidateMethodCallParams,
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
    // Конфига источника нет → kind по умолчанию "none".
    let json = srv.rebuild_symbol_index().await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["message"].as_str().unwrap().contains("пересобирать нечего"));
}

#[tokio::test]
async fn rebuild_symbol_index_builds_database_and_creates_directory() {
    let Some(srv) = make_server().await else { eprintln!("skip: hbk не найден"); return; };
    let root = std::path::Path::new(r"C:\RepoWMS");
    if !root.exists() { eprintln!("skip: корпуса RepoWMS нет"); return; }
    // Каталога заведомо нет — инструмент обязан его создать.
    let dir = std::env::temp_dir().join("bslctx_rebuild_test").join("nested");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    let db = dir.join("wms_lite.db");

    let mut cfg = bsl_context_server::config::SymbolSourceConfig::default();
    cfg.kind = "lite".to_string();
    cfg.root = Some(root.to_path_buf());
    cfg.db_path = Some(db.clone());
    let srv = srv.with_symbol_source_config(cfg);

    let json = srv.rebuild_symbol_index().await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["ok"], true, "ответ: {json}");
    assert!(v["modules"].as_u64().unwrap() > 0);
    assert!(db.exists(), "база не создана");
    // Источник подменён в памяти.
    assert!(srv.symbol_source.read().await.is_some());
    // Временный файл убран.
    assert!(!db.with_extension("db.tmp").exists());

    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}
