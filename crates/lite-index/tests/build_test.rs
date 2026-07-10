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
}

#[test]
fn build_indexes_all_modules_and_flags() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());

    let db_path = tmp.path().join("lite.db");
    let stats = build(tmp.path(), &db_path, 0).unwrap();

    assert_eq!(stats.modules, 4, "ожидались 4 модуля, получили {}", stats.modules);
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
        .unwrap();
    assert_eq!(exports, vec!["экспортныйметодобр".to_string()]);
}
