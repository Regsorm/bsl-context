//! Интеграционные тесты issue #41: значения прикладных перечислений.
//!
//! До правки `Перечисления.<Имя>.<Значение>` не проверялось вовсе: имя
//! перечисления сверяла ветка менеджеров, а значение — никто, и
//! `Перечисления.ВариантыОкругления.НетТакогоЗначения123` давало `valid: true`.
//! Значения приходят из источника имён (`enum_values` в `attributes_json`).

use bsl_validator::{
    validate_module_with_symbols, Confidence, ExprError, ExprErrorKind, ObjectSchema, Profile,
    SymbolSource,
};

/// Источник имён: одно прикладное перечисление с тремя значениями.
struct Stub {
    /// `true` — состав значений не отдан (индекс старой версии).
    no_values: bool,
    /// `true` — источник «тонкий»: состав почти пуст (issue #46).
    thin: bool,
}

impl Stub {
    fn known() -> Self {
        Self {
            no_values: false,
            thin: false,
        }
    }

    fn without_values() -> Self {
        Self {
            no_values: true,
            thin: false,
        }
    }

    fn thin() -> Self {
        Self {
            no_values: false,
            thin: true,
        }
    }
}

impl SymbolSource for Stub {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_schema(&self, collection: &str, name_lower: &str) -> Option<ObjectSchema> {
        if collection != "Enums" || name_lower != "вариантыокругления" {
            return None;
        }
        Some(ObjectSchema {
            enum_values: if self.no_values {
                Vec::new()
            } else {
                vec![
                    "ПоАрифметическимПравилам".to_string(),
                    "ВсегдаВПользуКлиента".to_string(),
                    "ВсегдаВПользуПредприятия".to_string(),
                ]
            },
            ..Default::default()
        })
    }

    fn is_thin(&self) -> bool {
        self.thin
    }

    fn describe(&self) -> String {
        "stub-issue41".to_string()
    }
}

fn findings(src: &str, symbols: &dyn SymbolSource) -> Vec<ExprError> {
    validate_module_with_symbols(
        &platform_index::PlatformIndex::new(),
        src,
        3,
        Profile::Full,
        None,
        None,
        Some(symbols),
    )
    .errors
}

fn probe(chain: &str) -> String {
    format!("Процедура Зонд()\n\tА = {chain};\nКонецПроцедуры\n")
}

fn enum_findings(errors: &[ExprError]) -> Vec<&ExprError> {
    errors
        .iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownEnumValue)
        .collect()
}

// ── Выдуманное значение — находка ────────────────────────────────────────────

#[test]
fn invented_enum_value_is_reported() {
    let found = findings(
        &probe("Перечисления.ВариантыОкругления.НетТакогоЗначения123"),
        &Stub::known(),
    );
    let enum_found = enum_findings(&found);
    assert_eq!(enum_found.len(), 1, "{found:?}");
    assert_eq!(enum_found[0].confidence, Confidence::High);
    assert!(
        enum_found[0].message.contains("ВариантыОкругления"),
        "{}",
        enum_found[0].message
    );
}

#[test]
fn value_typo_gets_suggestion() {
    let found = findings(
        &probe("Перечисления.ВариантыОкругления.ПоАрифметическимПравилм"),
        &Stub::known(),
    );
    let enum_found = enum_findings(&found);
    assert_eq!(enum_found.len(), 1, "{found:?}");
    assert_eq!(
        enum_found[0].suggestion.as_deref(),
        Some("ПоАрифметическимПравилам")
    );
}

#[test]
fn existing_enum_value_is_silent() {
    for chain in [
        "Перечисления.ВариантыОкругления.ПоАрифметическимПравилам",
        "Перечисления.ВариантыОкругления.ВсегдаВПользуКлиента",
        "Перечисления.ВариантыОкругления",
    ] {
        let found = findings(&probe(chain), &Stub::known());
        assert!(enum_findings(&found).is_empty(), "{chain}: {found:?}");
    }
}

// ── Молчание там, где состав неизвестен ──────────────────────────────────────

#[test]
fn without_enum_values_rule_is_silent() {
    // Индекс старой версии: значения не отданы — выдуманное значение
    // неотличимо от законного, находки быть не должно.
    let found = findings(
        &probe("Перечисления.ВариантыОкругления.НетТакогоЗначения123"),
        &Stub::without_values(),
    );
    assert!(enum_findings(&found).is_empty(), "{found:?}");
}

#[test]
fn unknown_enum_name_is_not_reported_as_value() {
    // Имя перечисления проверяет ветка менеджеров — своё правило не дублирует.
    let found = findings(
        &probe("Перечисления.НетТакогоПеречисления123.Значение"),
        &Stub::known(),
    );
    assert!(enum_findings(&found).is_empty(), "{found:?}");
}

#[test]
fn without_source_rule_is_silent() {
    let found = findings(
        &probe("Перечисления.ВариантыОкругления.НетТакогоЗначения123"),
        &Stub::without_values(),
    );
    assert!(found.is_empty(), "{found:?}");
}

// ── Тонкий источник понижает уверенность (issue #46) ─────────────────────────

#[test]
fn thin_source_lowers_confidence() {
    let found = findings(
        &probe("Перечисления.ВариантыОкругления.НетТакогоЗначения123"),
        &Stub::thin(),
    );
    let enum_found = enum_findings(&found);
    assert_eq!(enum_found.len(), 1, "{found:?}");
    assert_eq!(enum_found[0].confidence, Confidence::Low);
}
