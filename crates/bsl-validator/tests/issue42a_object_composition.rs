//! Issue #42, п. 1: в модуле объекта неквалифицированное имя сверяется с СОСТАВОМ
//! собственного объекта — несуществующий реквизит или табличная часть становится
//! находкой, а законные имена (состав, стандартные реквизиты, методы объекта)
//! молчат.
//!
//! До правки `НетТакойТЧ1.Итог("Сумма")` в модуле объекта проходило молча: правило
//! голов умело только «похоже на глобальное имя платформы». Индекс синтетический
//! (справка не нужна), источник имён — заглушка с составом объекта и регистра.

use bsl_validator::{
    validate_module_with_symbols, Confidence, ExprError, ExprErrorKind, ObjectField, ObjectSchema,
    Profile, SymbolSource,
};
use platform_index::{Method, PlatformIndex, Property, Type};

const DP_OBJECT: &str = "DataProcessors/ФормированиеЗаказовПоставщикам/Ext/ObjectModule.bsl";
const DOC_OBJECT: &str = "Documents/РеализацияТоваровУслуг/Ext/ObjectModule.bsl";
const RS_MODULE: &str = "AccumulationRegisters/ТоварыНаСкладах/Ext/RecordSetModule.bsl";

/// Источник имён: состав обработки, документа и регистра.
struct Stub {
    /// `true` — источник состава не отдаёт (объекта нет в индексе).
    silent: bool,
}

impl Stub {
    fn known() -> Self {
        Self { silent: false }
    }

    fn silent() -> Self {
        Self { silent: true }
    }

    fn schema(&self, kind: &str) -> Option<ObjectSchema> {
        if self.silent {
            return None;
        }
        let field = |name: &str| ObjectField {
            name: name.to_string(),
            indexing: None,
        };
        match kind {
            "dp" => Some(ObjectSchema {
                attributes: vec![field("Организация")],
                tabular_sections: vec![field("Заказы"), field("Запасы")],
                ..Default::default()
            }),
            "doc" => Some(ObjectSchema {
                attributes: vec![field("Организация"), field("Комментарий")],
                tabular_sections: vec![field("Товары")],
                ..Default::default()
            }),
            "rs" => Some(ObjectSchema {
                dimensions: vec![field("Номенклатура")],
                resources: vec![field("Количество")],
                register_type: Some("Balance".to_string()),
                ..Default::default()
            }),
            _ => None,
        }
    }
}

impl SymbolSource for Stub {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_schema(&self, collection: &str, name_lower: &str) -> Option<ObjectSchema> {
        match (collection, name_lower) {
            ("DataProcessors", "формированиезаказовпоставщикам") => {
                self.schema("dp")
            }
            ("Documents", "реализациятоваровуслуг") => self.schema("doc"),
            ("AccumulationRegisters", "товарынаскладах") => self.schema("rs"),
            _ => None,
        }
    }

    fn describe(&self) -> String {
        "stub-issue42a".to_string()
    }
}

/// Индекс: шаблонные типы контекста владельца (стандартные реквизиты и методы) и
/// одно глобальное свойство `Задачи` — для контроля прежнего поведения.
fn index() -> PlatformIndex {
    let prop = |name: &str| Property {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        type_name: String::new(),
        readonly: false,
    };
    let method = |name: &str| Method {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        return_type: String::new(),
        signatures: Vec::new(),
        note: None,
    };
    let ty = |name: &str, props: Vec<Property>, methods: Vec<Method>| Type {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        methods,
        properties: props,
        constructors: Vec::new(),
        enum_values: Vec::new(),
    };

    let mut index = PlatformIndex::new();
    index.insert_type(ty(
        "ОбработкаОбъект.ФормированиеЗаказовПоставщикам",
        vec![prop("Ссылка")],
        vec![method("Записать")],
    ));
    index.insert_type(ty(
        "ДокументОбъект.РеализацияТоваровУслуг",
        vec![
            prop("Ссылка"),
            prop("Номер"),
            prop("Дата"),
            prop("Проведен"),
        ],
        vec![method("Записать")],
    ));
    index.insert_type(ty(
        "РегистрНакопленияНаборЗаписей.ТоварыНаСкладах",
        vec![prop("Регистратор"), prop("Период"), prop("Активность")],
        vec![method("Записать")],
    ));
    index.global_properties.push(prop("Задачи"));
    index
}

fn heads(src: &str, module_path: Option<&str>, symbols: &dyn SymbolSource) -> Vec<ExprError> {
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
    .into_iter()
    .filter(|e| e.kind == ExprErrorKind::UnknownGlobalProperty)
    .collect()
}

fn probe(head: &str, member: &str) -> String {
    format!("Процедура Зонд()\n\t{head}.{member}(\"Сумма\");\nКонецПроцедуры\n")
}

// ── Несуществующее имя в модуле объекта — находка ────────────────────────────

#[test]
fn invented_tabular_part_is_reported() {
    let found = heads(
        &probe("НетТакойТЧ1", "Итог"),
        Some(DP_OBJECT),
        &Stub::known(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].confidence, Confidence::Low);
    assert!(
        found[0].message.contains("составе объекта"),
        "{}",
        found[0].message
    );
}

#[test]
fn invented_requisite_is_reported() {
    let found = heads(
        &probe("НетТакогоРеквизита123", "Итог"),
        Some(DOC_OBJECT),
        &Stub::known(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn close_name_gets_suggestion_from_composition() {
    // `Организаиця` — опечатка реквизита `Организация` (расстояние 3, имя длинное).
    let found = heads(
        &probe("Организаиця", "Очистить"),
        Some(DP_OBJECT),
        &Stub::known(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].suggestion.as_deref(), Some("Организация"));
    assert!(
        found[0].message.contains("Возможно, вы имели в виду"),
        "{}",
        found[0].message
    );
}

// ── Законные имена молчат ────────────────────────────────────────────────────

#[test]
fn owner_requisite_and_tabular_part_are_silent() {
    for head in ["Организация", "Заказы", "Запасы"] {
        let found = heads(&probe(head, "Очистить"), Some(DP_OBJECT), &Stub::known());
        assert!(found.is_empty(), "{head}: {found:?}");
    }
}

#[test]
fn standard_fields_of_document_are_silent() {
    for head in ["Ссылка", "Номер", "Дата", "Проведен"] {
        let found = heads(&probe(head, "Очистить"), Some(DOC_OBJECT), &Stub::known());
        assert!(found.is_empty(), "{head}: {found:?}");
    }
}

#[test]
fn record_set_fields_and_standard_attributes_are_silent() {
    for head in [
        "Номенклатура",
        "Количество",
        "Регистратор",
        "Период",
        "Активность",
    ] {
        let found = heads(&probe(head, "Очистить"), Some(RS_MODULE), &Stub::known());
        assert!(found.is_empty(), "{head}: {found:?}");
    }
}

#[test]
fn without_owner_schema_nothing_new_is_reported() {
    // Состава нет — правило работает как раньше: `НетТакойТЧ1` не находится.
    let found = heads(
        &probe("НетТакойТЧ1", "Итог"),
        Some(DP_OBJECT),
        &Stub::silent(),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn without_module_path_nothing_new_is_reported() {
    let found = heads(&probe("НетТакойТЧ1", "Итог"), None, &Stub::known());
    assert!(found.is_empty(), "{found:?}");
}

// ── Прежнее поведение сохранено ──────────────────────────────────────────────

#[test]
fn global_property_typo_is_still_reported_as_before() {
    // `Задача` — опечатка глобального свойства `Задачи`: находка с высокой
    // уверенностью и прежним текстом, ветка состава до неё не доходит.
    let found = heads(
        &probe("Задача", "Очистить"),
        Some(DP_OBJECT),
        &Stub::known(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].confidence, Confidence::High);
    assert_eq!(found[0].suggestion.as_deref(), Some("Задачи"));
}
