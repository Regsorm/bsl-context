//! Слияние состава объекта по копиям из базовой конфигурации и расширений.
//!
//! Один объект лежит в выгрузке многократно: базовая конфигурация плюс каждое
//! расширение, которое его дополняет. Замер на УТ: у `Documents.ЗаказКлиента`
//! 19 копий, полный состав только в базовой. Первая редакция `object_schema`
//! брала первую попавшуюся строку и возвращала два реквизита из случайного
//! расширения — тест держит именно этот случай.
//!
//! ```pwsh
//! cargo test -p lite-index --test object_schema_merge --release -- --ignored --nocapture
//! ```

use std::path::Path;

const DB: &str = r"C:\Temp\ut_lite_v3.db";

#[test]
#[ignore = "требует собранный lite-индекс УТ схемы 3 в C:\\Temp\\ut_lite_v3.db"]
fn merges_object_copies_from_extensions() {
    let path = Path::new(DB);
    assert!(
        path.is_file(),
        "нет базы {DB} — соберите: bsl-lite-index build --root C:\\RepoUT-test --db {DB}"
    );
    let index = lite_index::LiteIndex::open(path).expect("база не открылась");

    let schema = index
        .object_schema("Documents", "заказклиента")
        .expect("ошибка запроса")
        .expect("объект не найден");

    // В базовой копии 95 полей; расширения добавляют свои. Полный состав
    // обязан быть заметно больше того, что лежит в любой одной копии.
    assert!(
        schema.fields.len() >= 95,
        "состав слился не полностью: {} полей",
        schema.fields.len()
    );

    let names: Vec<&str> = schema.fields.iter().map(|(n, ..)| n.as_str()).collect();
    assert!(names.contains(&"Сделка"), "нет реквизита из базовой копии");

    // Дублей быть не должно: одно имя одного вида — одна запись.
    let mut seen = std::collections::HashSet::new();
    for (name, kind, _) in &schema.fields {
        assert!(
            seen.insert((kind.clone(), name.to_lowercase())),
            "поле {name} ({kind}) задвоилось при слиянии"
        );
    }

    // Признак индексирования переживает слияние.
    let indexed: Vec<&str> = schema
        .fields
        .iter()
        .filter(|(_, _, ix)| ix.is_some())
        .map(|(n, ..)| n.as_str())
        .collect();
    assert!(
        indexed.contains(&"Сделка"),
        "индексирование потеряно: {indexed:?}"
    );
}

#[test]
#[ignore = "требует собранный lite-индекс УТ схемы 3"]
fn register_schema_has_dimensions_and_type() {
    let path = Path::new(DB);
    assert!(path.is_file(), "нет базы {DB}");
    let index = lite_index::LiteIndex::open(path).expect("база не открылась");

    let schema = index
        .object_schema("AccumulationRegisters", "товарынаскладах")
        .expect("ошибка запроса")
        .expect("регистр не найден");

    assert_eq!(schema.register_type.as_deref(), Some("Balance"));
    let dimensions: Vec<&str> = schema
        .fields
        .iter()
        .filter(|(_, kind, _)| kind == "dimension")
        .map(|(n, ..)| n.as_str())
        .collect();
    assert!(dimensions.contains(&"Номенклатура"), "{dimensions:?}");
    assert!(dimensions.contains(&"Склад"), "{dimensions:?}");
}
