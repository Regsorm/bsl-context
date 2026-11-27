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

fn typed_findings(source: &str, level: u8) -> Vec<ExprError> {
    use bsl_validator::{
        form_elements::FormElements, module_context::FormKind, validate_module_with_form_elements,
    };
    use platform_index::{Property, Type};

    let mut index = PlatformIndex::new();
    for (name, member, member_en, member_type) in [
        ("КнопкаФормы", "Отображение", "Representation", ""),
        ("ПолеФормы", "Заголовок", "Title", ""),
        (
            "ТаблицаФормы",
            "ТекущиеДанные",
            "CurrentData",
            "ДанныеФормыСтруктура",
        ),
        ("ДанныеФормыСтруктура", "Количество", "Count", ""),
        (
            "Расширение поля формы для поля ввода",
            "СписокВыбора",
            "ChoiceList",
            "",
        ),
        ("ГруппаФормы", "Видимость", "Visible", ""),
        (
            "Расширение группы формы для страниц",
            "ТекущаяСтраница",
            "CurrentPage",
            "",
        ),
    ] {
        index.insert_type(Type {
            name_ru: name.to_owned(),
            name_en: String::new(),
            description: String::new(),
            methods: Vec::new(),
            properties: vec![Property {
                name_ru: member.to_owned(),
                name_en: member_en.to_owned(),
                description: String::new(),
                type_name: member_type.to_owned(),
                readonly: false,
                note: None,
                availability: None,
            }],
            constructors: Vec::new(),
            enum_values: Vec::new(),
            note: None,
            availability: None,
        });
    }
    let mut form = FormElements::default();
    for (name, ty) in [
        ("КнопкаВыбрать", "КнопкаФормы"),
        ("ПолеПоиска", "ПолеФормы"),
        ("СтраницыФормы", "ГруппаФормы"),
        ("Товары", "ТаблицаФормы"),
    ] {
        form.names.insert(name.to_owned());
        form.record_type(name, Some(ty));
    }
    validate_module_with_form_elements(
        &index,
        source,
        level,
        Profile::Full,
        Some(FORM_MODULE),
        None,
        None,
        Some(FormKind::Managed),
        Some(&form),
    )
    .errors
    .into_iter()
    .filter(|error| error.kind == ExprErrorKind::UnknownTypeMember)
    .collect()
}

#[test]
fn typed_form_members_are_checked_directly_and_through_assignment() {
    let source = probe(concat!(
        "Элементы.КнопкаВыбрать.Отображение = 1;\n",
        "Элементы.КнопкаВыбрать.ОтображениеКнопки = 1;\n",
        "Кнопка = ЭтаФорма.Элементы.кнопкавыбрать;\n",
        "Кнопка.Отображение = 1;\n",
        "Кнопка.Подсказка = \"текст\";\n",
    ));
    let found = typed_findings(&source, 3);
    assert_eq!(
        found.iter().map(|error| error.line).collect::<Vec<_>>(),
        [4, 7],
        "{found:?}"
    );
    assert!(found
        .iter()
        .all(|error| error.message.contains("КнопкаФормы")));
    assert!(typed_findings(&source, 2).is_empty());
}

#[test]
fn typed_form_members_support_calls_and_current_form_qualifier() {
    let source = probe(concat!(
        "Элементы.КнопкаВыбрать.НесуществующийМетод();\n",
        "ЭтаФорма.Элементы.КнопкаВыбрать.Подсказка = 1;\n",
        "ThisForm.Items.КнопкаВыбрать.Representation = 1;\n",
    ));
    let found = typed_findings(&source, 3);
    assert_eq!(
        found.iter().map(|error| error.line).collect::<Vec<_>>(),
        [3, 4],
        "{found:?}"
    );
}

#[test]
fn typed_form_members_require_the_current_form_and_context() {
    let source = concat!(
        "&НаКлиенте\nПроцедура Проба(Элементы, Форма)\n",
        "Элементы.КнопкаВыбрать.Подсказка = 1;\n",
        "Кнопка = Форма.Элементы.КнопкаВыбрать;\n",
        "Кнопка.Подсказка = 1;\nКонецПроцедуры\n",
        "&НаСервереБезКонтекста\nПроцедура ПробаНаСервере()\n",
        "Элементы.КнопкаВыбрать.Подсказка = 1;\n",
        "Кнопка = Элементы.КнопкаВыбрать;\n",
        "Кнопка.Подсказка = 1;\nКонецПроцедуры\n",
    );
    assert!(typed_findings(source, 3).is_empty());
}

#[test]
fn typed_form_members_respect_reassignment_and_dynamic_elements() {
    let reassigned = probe(concat!(
        "Кнопка = Элементы.КнопкаВыбрать;\n",
        "Кнопка = Неопределено;\n",
        "Кнопка.Подсказка = 1;\n",
    ));
    let dynamic = probe(concat!(
        "ЭтаФорма.Элементы.Удалить(Элементы.КнопкаВыбрать);\n",
        "ЭтаФорма.Элементы.Вставить(НовыйЭлемент, Тип(\"ПолеФормы\"));\n",
        "Элементы.КнопкаВыбрать.Подсказка = 1;\n",
    ));
    assert!(typed_findings(&reassigned, 3).is_empty());
    assert!(typed_findings(&dynamic, 3).is_empty());
}

#[test]
fn form_member_checks_include_documented_extensions() {
    let source = probe(concat!(
        "Элементы.ПолеПоиска.СписокВыбора.Очистить();\n",
        "Элементы.СтраницыФормы.ТекущаяСтраница = Элементы.ПолеПоиска;\n",
        "Поле = Элементы.ПолеПоиска;\n",
        "Список = Поле.ChoiceList;\n",
        "Поле.НесуществующийЧлен = 1;\n",
    ));
    let findings = typed_findings(&source, 3);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].line, 7, "{findings:?}");
    assert!(findings[0].message.contains("ПолеФормы"));
}

#[test]
fn fields_of_form_data_are_not_platform_members() {
    let source = probe(concat!(
        "Таблица = Элементы.Товары;\n",
        "Строка = Таблица.ТекущиеДанные;\n",
        "Ответ = Строка.РеквизитКонфигурации;\n",
    ));
    let findings = typed_findings(&source, 3);
    assert!(findings.is_empty(), "{findings:?}");
}
