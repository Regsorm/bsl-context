//! Issue #42, п. 2: голова обращения, совпавшая с реквизитом или ТАБЛИЧНОЙ ЧАСТЬЮ
//! собственного объекта, не должна получать `unknown_global_property`.
//!
//! До правки `Заказы.Очистить()` в модуле объекта `ФормированиеЗаказовПоставщикам`
//! (там табличная часть `Заказы` — настоящая) давало находку «'Заказы' не найдено
//! в глобальном контексте платформы. Возможно, вы имели в виду 'Задачи'» и
//! `valid: false` на корректном коде: состав объекта правилу голов был неизвестен,
//! а в справке табличных частей нет вовсе.
//!
//! Индекс синтетический (справка не нужна), источник имён — заглушка с составом
//! одной обработки.

use std::collections::HashSet;

use bsl_validator::{
    validate_module_with_symbols, Confidence, ExprError, ExprErrorKind, ObjectField, ObjectSchema,
    Profile, SymbolSource,
};
use platform_index::{PlatformIndex, Property};

const DP_OBJECT: &str = "DataProcessors/ФормированиеЗаказовПоставщикам/Ext/ObjectModule.bsl";

/// Источник имён: состав одной обработки, у которой есть табличные части.
struct Stub {
    /// `None` — источник состава не знает (объекта нет в индексе, коллекция не та).
    schema: Option<ObjectSchema>,
}

impl Stub {
    /// Состав обработки: реквизит `Организация`, табличные части `Заказы`, `Запасы`.
    fn with_schema() -> Self {
        let field = |name: &str| ObjectField {
            name: name.to_string(),
            indexing: None,
        };
        Self {
            schema: Some(ObjectSchema {
                attributes: vec![field("Организация")],
                dimensions: Vec::new(),
                resources: Vec::new(),
                tabular_sections: vec![field("Заказы"), field("Запасы")],
                register_type: None,
                enum_values: Vec::new(),
            }),
        }
    }

    fn without_schema() -> Self {
        Self { schema: None }
    }
}

impl SymbolSource for Stub {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_schema(&self, collection: &str, name_lower: &str) -> Option<ObjectSchema> {
        if collection == "DataProcessors" && name_lower == "формированиезаказовпоставщикам"
        {
            return self.schema.clone();
        }
        // Регистр: измерения и ресурсы видны в модуле НАБОРА ЗАПИСЕЙ без префикса.
        if collection == "AccumulationRegisters" && name_lower == "товарынаскладах" {
            return self.schema.clone();
        }
        None
    }

    fn describe(&self) -> String {
        "stub-issue42b".to_string()
    }
}

/// Индекс с единственным глобальным свойством `Задачи`: к нему и тянется
/// ложная подсказка («Заказы» → «Задачи», расстояние 3).
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.global_properties.push(Property {
        name_ru: "Задачи".into(),
        name_en: "Tasks".into(),
        description: String::new(),
        type_name: "ЗадачиМенеджер".into(),
        readonly: true,
        note: None,
    });
    index
}

/// Находки `unknown_global_property` по целому модулю.
fn property_heads(
    src: &str,
    module_path: Option<&str>,
    symbols: &dyn SymbolSource,
) -> Vec<ExprError> {
    property_heads_with_form(src, module_path, None, symbols)
}

/// То же, но с составом реквизитов формы (для модулей форм).
fn property_heads_with_form(
    src: &str,
    module_path: Option<&str>,
    form_attributes: Option<&HashSet<String>>,
    symbols: &dyn SymbolSource,
) -> Vec<ExprError> {
    validate_module_with_symbols(
        &index(),
        src,
        3,
        Profile::Full,
        module_path,
        form_attributes,
        Some(symbols),
    )
    .errors
    .into_iter()
    .filter(|e| e.kind == ExprErrorKind::UnknownGlobalProperty)
    .collect()
}

fn probe(head: &str) -> String {
    format!("Процедура Зонд()\n\t{head}.Очистить();\nКонецПроцедуры\n")
}

// ── Состав объекта известен: законные имена молчат ───────────────────────────

#[test]
fn tabular_section_of_owner_is_silent() {
    let found = property_heads(&probe("Заказы"), Some(DP_OBJECT), &Stub::with_schema());
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn second_tabular_section_of_owner_is_silent() {
    let found = property_heads(&probe("Запасы"), Some(DP_OBJECT), &Stub::with_schema());
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn attribute_of_owner_is_silent() {
    let found = property_heads(&probe("Организация"), Some(DP_OBJECT), &Stub::with_schema());
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn document_requisite_is_silent() {
    // Документ с реквизитом `Организация`: коллекция `Documents`.
    let found = property_heads(
        &probe("Организация"),
        Some("Documents/РеализацияТоваровУслуг/Ext/ObjectModule.bsl"),
        &Stub::with_schema(),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn record_set_module_sees_register_fields() {
    // Модуль набора записей регистра: измерения и ресурсы видны без префикса.
    let found = property_heads(
        &probe("Заказы"),
        Some("AccumulationRegisters/ТоварыНаСкладах/Ext/RecordSetModule.bsl"),
        &Stub::with_schema(),
    );
    assert!(found.is_empty(), "{found:?}");
}

// ── Контроли: правило продолжает работать ────────────────────────────────────

#[test]
fn typo_of_global_property_is_still_reported() {
    // `Задача` — не имя состава, а правдоподобная опечатка `Задачи` (расстояние 1).
    let found = property_heads(&probe("Задача"), Some(DP_OBJECT), &Stub::with_schema());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].suggestion.as_deref(), Some("Задачи"));
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn without_owner_schema_rule_works_as_before() {
    // Состава нет (объекта нет в источнике) — правило не меняется: находка
    // остаётся, и это честно, потому что состав действительно неизвестен.
    let found = property_heads(&probe("Заказы"), Some(DP_OBJECT), &Stub::without_schema());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].suggestion.as_deref(), Some("Задачи"));
}

#[test]
fn without_module_path_rule_works_as_before() {
    let found = property_heads(&probe("Заказы"), None, &Stub::with_schema());
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn manager_module_keeps_the_rule() {
    // В модуле МЕНЕДЖЕРА реквизиты и табличные части объекта без префикса не
    // видны: молчание по составу объекта там скрыло бы настоящую ошибку.
    let found = property_heads(
        &probe("Заказы"),
        Some("DataProcessors/ФормированиеЗаказовПоставщикам/Ext/ManagerModule.bsl"),
        &Stub::with_schema(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn form_module_without_attributes_is_silent() {
    // Прежнее поведение: состав реквизитов формы неизвестен — правило молчит
    // целиком (реквизит с именем `Справочник` законен, отличить его нечем).
    let found = property_heads(
        &probe("Заказы"),
        Some("DataProcessors/ФормированиеЗаказовПоставщикам/Forms/Форма/Ext/Form/Module.bsl"),
        &Stub::with_schema(),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn form_module_with_attributes_keeps_the_rule() {
    // Реквизиты формы известны: контекст — сама форма, состав ОБЪЕКТА не при чём,
    // и голова, похожая на глобальное свойство, остаётся находкой.
    let form_attrs: HashSet<String> = ["РеквизитФормы".to_string()].into_iter().collect();
    let found = property_heads_with_form(
        &probe("Заказы"),
        Some("DataProcessors/ФормированиеЗаказовПоставщикам/Forms/Форма/Ext/Form/Module.bsl"),
        Some(&form_attrs),
        &Stub::with_schema(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn name_outside_owner_schema_is_still_reported() {
    // Имя, похожее на глобальное свойство и НЕ входящее в состав, — находка.
    let found = property_heads(&probe("Задача"), Some(DP_OBJECT), &Stub::with_schema());
    assert_eq!(found.len(), 1, "{found:?}");
}
