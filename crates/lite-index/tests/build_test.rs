//! Интеграционные тесты сборки облегчённого индекса на временном каталоге
//! выгрузки: проверяем разбор путей (scope/collection/module_type/owner_path),
//! флаг глобального модуля и публичный API `LiteIndex`.

use std::fs;
use std::path::Path;

use lite_index::{build, LiteIndex};

/// Создать файл вместе с родительскими каталогами.
fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn setup(root: &Path) {
    // Глобальный общий модуль.
    write_file(
        root,
        "base/CommonModules/Гло/Ext/Module.bsl",
        "Процедура ИмяИзГло() Экспорт\nКонецПроцедуры\n",
    );
    write_file(
        root,
        "base/CommonModules/Гло.xml",
        "<?xml version=\"1.0\"?>\n<MetaDataObject><CommonModule><Properties><Name>Гло</Name><Global>true</Global></Properties></CommonModule></MetaDataObject>\n",
    );

    // Обычный (не глобальный) общий модуль.
    write_file(
        root,
        "base/CommonModules/Обычный/Ext/Module.bsl",
        "Процедура МетодОбычного() Экспорт\nКонецПроцедуры\n",
    );
    write_file(
        root,
        "base/CommonModules/Обычный.xml",
        "<?xml version=\"1.0\"?>\n<MetaDataObject><CommonModule><Properties><Name>Обычный</Name><Global>false</Global></Properties></CommonModule></MetaDataObject>\n",
    );

    // Внешняя обработка: модуль объекта и модуль формы.
    write_file(
        root,
        "external/Обр/ExternalDataProcessor.obj.bsl",
        "Процедура ЭкспортныйМетодОбр() Экспорт\nКонецПроцедуры\n",
    );
    write_file(
        root,
        "external/Обр/Form/Ф/Form.obj.bsl",
        "&НаКлиенте\nПроцедура ПриОткрытии(Отказ)\nКонецПроцедуры\n",
    );

    // Объект без единого модуля — как большинство перечислений в УТ (909 из 1069).
    // Список объектов обязан строиться по XML, а не по таблице modules.
    write_file(
        root,
        "base/Enums/ТестБезМодуля.xml",
        "<?xml version=\"1.0\"?>\n<MetaDataObject><Enum><Properties><Name>ТестБезМодуля</Name></Properties></Enum></MetaDataObject>\n",
    );
}

#[test]
fn build_indexes_all_modules_and_flags() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());

    let db_path = tmp.path().join("lite.db");
    let stats = build(tmp.path(), &db_path, 0).unwrap();

    assert_eq!(
        stats.modules, 4,
        "ожидались 4 модуля, получили {}",
        stats.modules
    );
    assert_eq!(stats.global_modules, 1);
    assert!(stats.methods >= 4);

    // Проверка module-level полей напрямую через SQLite — LiteIndex их не отдаёт,
    // это внутренние поля схемы, а не часть публичного API.
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    let glo_is_global: i64 = conn
        .query_row(
            "SELECT is_global FROM modules WHERE path = ?1",
            ["base/CommonModules/Гло/Ext/Module.bsl"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(glo_is_global, 1, "Гло должен быть is_global=1");

    let obychny_global: i64 = conn
        .query_row(
            "SELECT is_global FROM modules WHERE path = ?1",
            ["base/CommonModules/Обычный/Ext/Module.bsl"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(obychny_global, 0);

    let owner_path: Option<String> = conn
        .query_row(
            "SELECT owner_path FROM modules WHERE path = ?1",
            ["external/Обр/Form/Ф/Form.obj.bsl"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        owner_path.as_deref(),
        Some("external/Обр/ExternalDataProcessor.obj.bsl")
    );

    // Публичный API LiteIndex.
    let index = LiteIndex::open(&db_path).unwrap();

    assert!(index.method_exists("имяИзГло").unwrap());
    assert!(!index.method_exists("НесуществующийМетод").unwrap());

    assert!(index.is_global_export("ИмяИзГло").unwrap());
    assert!(!index.is_global_export("МетодОбычного").unwrap());

    let exports = index
        .owner_exports("external/Обр/Form/Ф/Form.obj.bsl")
        .unwrap()
        .expect("владелец есть в индексе — Set, а не None");
    assert_eq!(exports, vec!["экспортныйметодобр".to_string()]);

    // Не форма → владельца нет: «не знаю» (None), а не пустой набор.
    assert!(
        index
            .owner_exports("base/CommonModules/Обычный/Ext/Module.bsl")
            .unwrap()
            .is_none(),
        "у не-формы владельца быть не может"
    );

    // Объект без модуля (перечисление) обязан попасть в objects — источник
    // тут XML, а не таблица modules.
    let objects = index
        .all_objects()
        .unwrap()
        .expect("свежесобранная база должна содержать таблицу objects");
    let enums = objects
        .get("Enums")
        .expect("коллекция Enums должна быть в наборе");
    assert!(
        enums.contains("ТестБезМодуля"),
        "объект без модуля не попал в objects, получено: {:?}",
        enums
    );
}

/// Индексы строятся ПОСЛЕ вставки данных (см. `INDEXES_SQL`), поэтому их
/// наличие проверяется отдельно: забытый `INDEXES_SQL` не сломает запросы —
/// они продолжат работать полным сканом, и деградация была бы тихой.
#[test]
fn build_creates_all_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());

    let db_path = tmp.path().join("lite.db");
    build(tmp.path(), &db_path, 0).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
        .unwrap();
    let indexes: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    for expected in [
        "idx_methods_name_lower",
        "idx_methods_module",
        "idx_modules_global",
        "idx_objects_lookup",
        "idx_object_fields_object",
        "idx_global_vars_name",
    ] {
        assert!(
            indexes.iter().any(|name| name == expected),
            "после сборки нет индекса {expected}: {indexes:?}"
        );
    }
}

/// Этапы сборки заполнены и укладываются в общее время: по ним скрипт замера
/// и `rebuild_symbol_index` показывают, где прошло время.
#[test]
fn build_reports_stage_timings() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());

    let db_path = tmp.path().join("lite.db");
    let stats = build(tmp.path(), &db_path, 0).unwrap();

    let stages = stats.walk_ms + stats.xml_ms + stats.parse_ms + stats.db_ms + stats.indexes_ms;
    assert!(
        stages <= stats.elapsed_ms,
        "сумма этапов ({stages} мс) больше общего времени ({} мс)",
        stats.elapsed_ms
    );

    // Этапы продублированы в meta — уже после сборки видно, где прошло время.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    for key in ["walk_ms", "xml_ms", "parse_ms", "db_ms", "indexes_ms"] {
        let value: String = conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .unwrap_or_else(|e| panic!("в meta нет ключа {key}: {e}"));
        assert!(value.parse::<u128>().is_ok(), "{key} = {value:?} не число");
    }
}

/// Порядок объектов в базе повторяет порядок обхода выгрузки, а поля внутри
/// объекта — порядок в XML. От этих порядков зависят строки `object_fields` и
/// порядок состава в `object_schema` (слияние копий идёт в порядке строк),
/// поэтому распараллеленный разбор XML не должен их менять.
#[test]
fn objects_and_fields_keep_source_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Полторы сотни объектов: параллельный разбор должен пройти через пул, а не
    // уложиться в один поток. Порядок сверяется с тем же обходом дерева.
    for index in 0..150 {
        let name = format!("Справочник{index:03}");
        write_file(
            root,
            &format!("base/Catalogs/{name}.xml"),
            &format!(
                "<?xml version=\"1.0\"?>\n<MetaDataObject><Catalog><Properties><Name>{name}</Name></Properties></MetaDataObject>\n"
            ),
        );
    }
    write_file(
        root,
        "base/Documents/Заказ.xml",
        "<?xml version=\"1.0\"?>\n\
         <MetaDataObject><Document><Properties><Name>Заказ</Name></Properties><ChildObjects>\n\
         <Attribute><Properties><Name>Первый</Name></Properties></Attribute>\n\
         <Attribute><Properties><Name>Второй</Name><Indexing>Index</Indexing></Properties></Attribute>\n\
         <Attribute><Properties><Name>Третий</Name></Properties></Attribute>\n\
         </ChildObjects></Document></MetaDataObject>\n",
    );

    let db_path = root.join("lite.db");
    build(root, &db_path, 0).unwrap();

    // Поля документа — в порядке XML.
    let index = LiteIndex::open(&db_path).unwrap();
    let schema = index
        .object_schema("Documents", "заказ")
        .unwrap()
        .expect("Заказ должен быть в индексе");
    let names: Vec<&str> = schema.fields.iter().map(|(n, ..)| n.as_str()).collect();
    assert_eq!(
        names,
        ["Первый", "Второй", "Третий"],
        "порядок полей разошёлся"
    );

    // Объекты — в порядке обхода дерева; тест повторяет тот же обход.
    let mut expected: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
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
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
        if !matches!(parent, Some("Catalogs") | Some("Documents")) {
            continue;
        }
        expected.push(path.file_stem().unwrap().to_string_lossy().to_string());
    }

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let actual: Vec<String> = conn
        .prepare(
            "SELECT name FROM objects WHERE collection IN ('Catalogs', 'Documents') ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        actual, expected,
        "порядок объектов разошёлся с порядком обхода"
    );
}
