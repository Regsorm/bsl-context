//! Имена из путей выгрузки должны совпадать с NFC-именами в BSL и XML.

use std::fs;
use std::path::Path;

use lite_index::{build, LiteIndex};

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

#[test]
fn normalizes_object_names_without_changing_file_paths() {
    for (document, module_xml, module_dir, enumeration) in [
        ("МойДокумент", "ОбщийМодуль", "ОбщийМодуль", "Ёлка"),
        (
            "Мои\u{306}Документ",
            "Общии\u{306}Модуль",
            "Общии\u{306}Модуль",
            "Е\u{308}лка",
        ),
        (
            "Мои\u{306}Документ",
            "Общии\u{306}Модуль",
            "ОбщийМодуль",
            "Е\u{308}лка",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dump");
        write(
            &root,
            &format!("base/Documents/{document}.xml"),
            "<MetaDataObject><Document><Properties><Name>МойДокумент</Name></Properties>\
             <ChildObjects><Attribute><Properties><Name>Контрагент</Name></Properties>\
             </Attribute></ChildObjects></Document></MetaDataObject>",
        );
        write(
            &root,
            &format!("base/Documents/{document}/Ext/ObjectModule.bsl"),
            "Процедура МетодДокумента() Экспорт\nКонецПроцедуры",
        );
        write(
            &root,
            &format!("base/CommonModules/{module_xml}.xml"),
            "<MetaDataObject><CommonModule><Properties><Name>ОбщийМодуль</Name>\
             <Global>true</Global></Properties></CommonModule></MetaDataObject>",
        );
        write(
            &root,
            &format!("base/CommonModules/{module_dir}/Ext/Module.bsl"),
            "Процедура ГлобальныйМетод() Экспорт\nКонецПроцедуры",
        );
        write(
            &root,
            &format!("base/Enums/{enumeration}.xml"),
            "<MetaDataObject><Enum><Properties><Name>Ёлка</Name></Properties></Enum></MetaDataObject>",
        );
        // Контроль: имя без разложимых букв и неглобальный модуль.
        write(
            &root,
            "base/CommonModules/Контроль.xml",
            "<MetaDataObject><CommonModule><Properties><Global>false</Global>\
             </Properties></CommonModule></MetaDataObject>",
        );
        write(
            &root,
            "base/CommonModules/Контроль/Ext/Module.bsl",
            "Процедура ЛокальныйМетод() Экспорт\nКонецПроцедуры",
        );

        let db = tmp.path().join("lite.db");
        let stats = build(&root, &db, 0).unwrap();
        assert_eq!(stats.modules, 3);
        assert_eq!(stats.global_modules, 1);
        let index = LiteIndex::open(&db).unwrap();
        let objects = index.all_objects().unwrap().unwrap();
        assert_eq!(objects["Documents"], ["МойДокумент".into()].into());
        assert_eq!(objects["Enums"], ["Ёлка".into()].into());
        assert_eq!(
            objects["CommonModules"],
            ["ОбщийМодуль".into(), "Контроль".into()].into()
        );
        assert!(index.is_global_export("ГлобальныйМетод").unwrap());
        assert!(!index.is_global_export("ЛокальныйМетод").unwrap());
        assert!(!index.is_global_export("НетТакогоМетода").unwrap());
        let schema = index
            .object_schema("Documents", "мойдокумент")
            .unwrap()
            .expect("NFC-имя должно находить состав объекта из NFD-пути");
        assert_eq!(schema.fields[0].0, "Контрагент");
        assert!(index
            .object_schema("Documents", "неттакогообъекта")
            .unwrap()
            .is_none());

        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT path, object_name FROM modules ORDER BY path")
            .unwrap();
        let modules: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut original_paths: Vec<String> = walkdir::WalkDir::new(&root)
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "bsl"))
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/")
            })
            .collect();
        original_paths.sort();
        assert_eq!(
            modules.iter().map(|(path, _)| path).collect::<Vec<_>>(),
            original_paths.iter().collect::<Vec<_>>()
        );
        for (path, name) in modules {
            let expected = if path.contains("/Documents/") {
                "МойДокумент"
            } else if path.contains("/Контроль/") {
                "Контроль"
            } else {
                "ОбщийМодуль"
            };
            assert_eq!(name, expected);
        }
    }
}
