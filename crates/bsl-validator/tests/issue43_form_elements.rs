//! Вторая половина issue #43: имена элементов формы (`Элементы.<Имя>`).
//!
//! До правки в модуле управляемой формы `Элементы.НетТакогоЭлемента123.Видимость
//! = Ложь` проходило молча: состав элементов лежит в `Form.xml` выгрузки рядом с
//! модулем, но его никто не читал. Теперь вызывающий (сервер) передаёт набор имён,
//! а валидатор сверяет с ним голову `Элементы`.
//!
//! Набор синтетический: проверяем ПРАВИЛО, а не разбор XML (тот закрыт тестами
//! `crates/server/src/form_dump.rs`).

use std::collections::HashSet;

use bsl_validator::{
    validate_module_with_symbols_and_form_kind, Confidence, ExprError, ExprErrorKind, Profile,
};
use platform_index::PlatformIndex;

const FORM_MODULE: &str = "Catalogs/Номенклатура/Forms/ФормаЭлемента/Ext/Form/Module.bsl";

fn elements(names: &[&str]) -> HashSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn findings(
    src: &str,
    module_path: Option<&str>,
    elements: Option<&HashSet<String>>,
) -> Vec<ExprError> {
    validate_module_with_symbols_and_form_kind(
        &PlatformIndex::new(),
        src,
        3,
        Profile::Full,
        module_path,
        None,
        None,
        None,
        elements,
    )
    .errors
    .into_iter()
    .filter(|e| e.kind == ExprErrorKind::UnknownFormElement)
    .collect()
}

fn probe(body: &str) -> String {
    format!("&НаКлиенте\nПроцедура Зонд()\n{body}КонецПроцедуры\n")
}

#[test]
fn invented_element_is_reported() {
    let set = elements(&["Товары", "ГруппаШапки"]);
    let found = findings(
        &probe("\tЭлементы.НетТакогоЭлемента123.Видимость = Ложь;\n"),
        Some(FORM_MODULE),
        Some(&set),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].confidence, Confidence::Low);
    assert!(
        found[0].message.contains("нет в форме"),
        "{}",
        found[0].message
    );
}

#[test]
fn existing_elements_are_silent() {
    let set = elements(&["Товары", "ГруппаШапки"]);
    let found = findings(
        &probe("\tЭлементы.Товары.Видимость = Ложь;\n\tЭлементы.ГруппаШапки.Видимость = Истина;\n"),
        Some(FORM_MODULE),
        Some(&set),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn element_typo_gets_suggestion() {
    let set = elements(&["Товары", "ГруппаШапки"]);
    let found = findings(
        &probe("\tЭлементы.Товари.Видимость = Ложь;\n"),
        Some(FORM_MODULE),
        Some(&set),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].suggestion.as_deref(), Some("Товары"));
}

#[test]
fn without_element_set_rule_is_silent() {
    let found = findings(
        &probe("\tЭлементы.НетТакогоЭлемента123.Видимость = Ложь;\n"),
        Some(FORM_MODULE),
        None,
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn dynamic_elements_disable_the_rule() {
    // Элемент, добавленный в рантайме, в `Form.xml` не описан: после
    // `Элементы.Добавить(...)` состав из файла неполон, и находок быть не должно.
    let set = elements(&["Товары"]);
    let found = findings(
        &probe("\tЭлементы.Добавить(НовыйПоле, Тип(\"ПолеВвода\"));\n\tЭлементы.НовоеПоле.Видимость = Ложь;\n"),
        Some(FORM_MODULE),
        Some(&set),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn locally_bound_head_is_silent() {
    let set = elements(&["Товары"]);
    let src = "&НаКлиенте\nПроцедура Зонд(Элементы)\n\tЭлементы.ЧтоТо.Видимость = Ложь;\nКонецПроцедуры\n";
    let found = findings(src, Some(FORM_MODULE), Some(&set));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn element_methods_are_not_names() {
    // `Элементы.Добавить(...)`, `Элементы.Найти("Товары")` — методы коллекции.
    let set = elements(&["Товары"]);
    let found = findings(
        &probe("\tЭ = Элементы.Найти(\"Товары\");\n\tЭлементы.Переместить(Э, 0);\n"),
        Some(FORM_MODULE),
        Some(&set),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn ordinary_form_head_is_not_checked() {
    // Состав обычной формы лежит в двоичном `Form.bin`: данных нет — находок нет.
    let set = elements(&["Товары"]);
    let found = findings(
        &probe("\tЭлементыФормы.НетТакогоЭлемента123.Видимость = Ложь;\n"),
        Some(FORM_MODULE),
        Some(&set),
    );
    assert!(found.is_empty(), "{found:?}");
}
