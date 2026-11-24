use std::collections::BTreeSet;
use std::sync::Arc;

use bsl_context_server::config::SymbolSourceConfig;
use bsl_context_server::mcp_server::{BslContextServer, ValidateModuleParams};
use platform_index::{PlatformIndex, Property, Type};
use rmcp::handler::server::wrapper::Parameters;
use rusqlite::Connection;
use serde_json::Value;

fn platform_index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.global_properties.push(Property {
        name_ru: "Справочники".into(),
        name_en: "Catalogs".into(),
        description: String::new(),
        note: None,
        type_name: "`СправочникиМенеджер`".into(),
        readonly: true,
        availability: None,
    });
    index.insert_type(Type {
        name_ru: "СправочникиМенеджер".into(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        methods: Vec::new(),
        properties: Vec::new(),
        constructors: Vec::new(),
        enum_values: Vec::new(),
        availability: None,
    });
    index
}

fn sqlite_source(dir: &tempfile::TempDir, objects: Option<i64>) -> BslContextServer {
    let path = dir.path().join("index.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE functions (name TEXT);
         CREATE TABLE files (id INTEGER, path TEXT);
         CREATE TABLE file_contents (file_id INTEGER, content_blob BLOB);
         WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 1200)
         INSERT INTO functions SELECT 'Method' || n FROM seq;",
    )
    .unwrap();
    if let Some(count) = objects {
        conn.execute_batch("CREATE TABLE metadata_objects (meta_type TEXT, name TEXT);")
            .unwrap();
        conn.execute(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?1)
             INSERT INTO metadata_objects
             SELECT 'Catalog', CASE n WHEN 1 THEN 'Контрагенты' ELSE 'Справочник' || n END
             FROM seq WHERE n <= ?1",
            [count],
        )
        .unwrap();
    }
    drop(conn);
    let source = symbol_source::CodeIndexDbSource::open(&path).unwrap();
    let config = SymbolSourceConfig {
        kind: "code_index_db".into(),
        db_path: Some(path),
        ..Default::default()
    };
    BslContextServer::new(platform_index()).with_sources(vec![(
        "sample".into(),
        config,
        Ok(Some(Arc::new(source))),
    )])
}

async fn validate(server: &BslContextServer) -> Value {
    let source = "Процедура Проба() Экспорт\n\
                  А = Справочники.Контрагенты.ПустаяСсылка();\n\
                  Н = Справочники.ЭтогоОбъектаТочноНет.ПустаяСсылка();\n\
                  НетТакогоОбщегоМодуля.Проверка();\n\
                  КонецПроцедуры";
    let json = server
        .validate_module(Parameters(ValidateModuleParams {
            source: source.into(),
            path: None,
            level: Some(3),
            profile: None,
            module_path: Some("CommonModules/Проба/Ext/Module.bsl".into()),
            form_attributes: None,
            repo: Some("sample".into()),
        }))
        .await;
    serde_json::from_str(&json).unwrap()
}

#[tokio::test]
async fn empty_object_index_degrades_an_otherwise_connected_source() {
    let dir = tempfile::tempdir().unwrap();
    let server = sqlite_source(&dir, Some(0));
    let status: Value = serde_json::from_str(&server.symbol_sources_status().await).unwrap();
    let entry = &status["sources"][0];
    assert_eq!(entry["connected"], true);
    assert_eq!(entry["thin"], true);
    assert_eq!(entry["stats"]["objects"], 0);

    let result = validate(&server).await;
    assert_eq!(result["tree_parsed"], true, "{result}");
    assert_eq!(result["symbols_available"], false, "{result}");
    assert!(result["degraded_reason"]
        .as_str()
        .unwrap()
        .contains("индекс объектов пуст"));
    assert!(result["errors"].as_array().unwrap().iter().all(|error| {
        error["kind"] != "unknown_common_module" && error["kind"] != "unknown_metadata_object"
    }));
}

#[tokio::test]
async fn composition_controls_confidence_while_keeping_the_source_available() {
    for (objects, confidence) in [(60, "low"), (240, "high")] {
        let dir = tempfile::tempdir().unwrap();
        let server = sqlite_source(&dir, Some(objects));
        let status: Value = serde_json::from_str(&server.symbol_sources_status().await).unwrap();
        let entry = &status["sources"][0];
        assert_eq!(entry["stats"]["objects"], objects);
        assert_eq!(entry["stats"]["names"], 1200);
        assert_eq!(entry["thin"], confidence == "low");

        let result = validate(&server).await;
        assert_eq!(result["symbols_available"], true, "{result}");
        assert!(result.get("degraded_reason").is_none(), "{result}");
        let findings: BTreeSet<_> = result["errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|error| {
                let kind = error["kind"].as_str().unwrap();
                matches!(kind, "unknown_metadata_object" | "unknown_common_module").then(|| {
                    (
                        error["line"].as_u64().unwrap(),
                        kind,
                        error["confidence"].as_str().unwrap(),
                    )
                })
            })
            .collect();
        assert_eq!(
            findings,
            BTreeSet::from([
                (3, "unknown_metadata_object", confidence),
                (4, "unknown_common_module", confidence)
            ]),
            "{result}"
        );
    }
}

#[tokio::test]
async fn legacy_index_without_object_table_can_still_supply_method_names() {
    let dir = tempfile::tempdir().unwrap();
    let result = validate(&sqlite_source(&dir, None)).await;
    assert_eq!(result["symbols_available"], true, "{result}");
    assert!(result.get("degraded_reason").is_none(), "{result}");
}
