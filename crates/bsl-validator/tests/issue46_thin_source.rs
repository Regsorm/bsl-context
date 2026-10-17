//! Интеграционные тесты issue #46: «тонкий» источник имён не должен давать
//! находки с высокой уверенностью.
//!
//! Ответ «такого объекта нет» убедителен ровно настолько, насколько полон
//! источник. Пустой индекс отвечает так же уверенно, как полный, и отличить одно
//! от другого было нечем: `healthy` описывает транспорт, а не содержимое. Теперь
//! источник сам заявляет [`SymbolSource::is_thin`], и находки по его ответам
//! понижаются до `Low`.

use bsl_validator::{
    validate_module_with_symbols, Confidence, ExprError, ExprErrorKind, Profile, SourceStats,
    SymbolSource,
};
use platform_index::{PlatformIndex, Type};

/// Источник, который знает «только то, что видит»: объект можно объявить
/// отсутствующим, а `thin` — заявить состав.
struct Stub {
    thin: bool,
    /// Объекты, которые источник знает (в нижнем регистре) — по коллекциям.
    known: Vec<(&'static str, &'static str)>,
}

impl Stub {
    fn thin() -> Self {
        Self {
            thin: true,
            known: Vec::new(),
        }
    }

    fn full() -> Self {
        Self {
            thin: false,
            known: vec![("Catalogs", "номенклатура")],
        }
    }
}

impl SymbolSource for Stub {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        Some(
            self.known
                .iter()
                .any(|(c, n)| *c == collection && *n == name_lower),
        )
    }

    fn global_variables(&self) -> Option<std::collections::HashSet<String>> {
        Some(std::collections::HashSet::new())
    }

    fn stats(&self) -> Option<SourceStats> {
        Some(SourceStats {
            objects: self.known.len(),
            names: if self.thin { 3 } else { 139_853 },
        })
    }

    fn is_thin(&self) -> bool {
        self.thin
    }

    fn describe(&self) -> String {
        "stub-issue46".to_string()
    }
}

/// Индекс: глобальное свойство `Справочники` и его тип-менеджер, иначе ветка
/// `Справочники.<Имя>` промолчит (без состава менеджера имя объекта не отличить
/// от метода менеджера).
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.global_properties.push(platform_index::Property {
        name_ru: "Справочники".into(),
        name_en: "Catalogs".into(),
        description: String::new(),
        note: None,
        // В справке имя типа обёрнуто в обратные кавычки — так же и здесь.
        type_name: "`СправочникиМенеджер`".into(),
        readonly: true,
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
    });
    index
}

fn findings(src: &str, module_path: Option<&str>, symbols: &dyn SymbolSource) -> Vec<ExprError> {
    validate_module_with_symbols(
        &index(),
        src,
        3,
        Profile::Full,
        module_path,
        None,
        Some(symbols),
    )
    .errors
}

fn probe(head: &str) -> String {
    format!("Процедура Зонд()\n\t{head}.Метод();\nКонецПроцедуры\n")
}

#[test]
fn thin_source_lowers_metadata_object_finding() {
    let found = findings(
        &probe("Справочники.НетТакогоОбъекта123"),
        Some("CommonModules/Х/Ext/Module.bsl"),
        &Stub::thin(),
    );
    let object_found: Vec<&ExprError> = found
        .iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownMetadataObject)
        .collect();
    assert_eq!(object_found.len(), 1, "{found:?}");
    assert_eq!(object_found[0].confidence, Confidence::Low);
}

#[test]
fn full_source_keeps_high_confidence() {
    let found = findings(
        &probe("Справочники.НетТакогоОбъекта123"),
        Some("CommonModules/Х/Ext/Module.bsl"),
        &Stub::full(),
    );
    let object_found: Vec<&ExprError> = found
        .iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownMetadataObject)
        .collect();
    assert_eq!(object_found.len(), 1, "{found:?}");
    assert_eq!(object_found[0].confidence, Confidence::High);
}

#[test]
fn thin_source_lowers_common_module_finding() {
    // Путь не передан: правило про общий модуль работает, и голова, которой нет
    // в конфигурации, становится находкой — но у тонкого источника низкой.
    let found = findings(&probe("НетТакогоОбщегоМодуля123"), None, &Stub::thin());
    let module_found: Vec<&ExprError> = found
        .iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownCommonModule)
        .collect();
    assert_eq!(module_found.len(), 1, "{found:?}");
    assert_eq!(module_found[0].confidence, Confidence::Low);
}

#[test]
fn full_source_keeps_common_module_high() {
    let found = findings(&probe("НетТакогоОбщегоМодуля123"), None, &Stub::full());
    let module_found: Vec<&ExprError> = found
        .iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownCommonModule)
        .collect();
    assert_eq!(module_found.len(), 1, "{found:?}");
    assert_eq!(module_found[0].confidence, Confidence::High);
}
