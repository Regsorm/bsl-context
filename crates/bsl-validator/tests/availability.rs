//! Правило доступности методов платформы по контексту выполнения (этап 2 плана
//! `TZ-dostupnost-metodov-kontekst.md`, раздел 7.3).
//!
//! Фикстура — маленький `PlatformIndex`: клиентский метод, серверный метод,
//! методы с неполными метками и типы с разной доступностью. Контекст задаётся
//! путём модуля в выгрузке, видом формы и директивой процедуры — ровно так, как
//! это делает `validate_module` на реальном модуле.

use bsl_validator::module_context::FormKind;
use bsl_validator::{
    validate_module_with_symbols_and_form_kind, Confidence, ExprError, ExprErrorKind, Profile,
};
use platform_index::{Availability, AvailabilityMark, Method, PlatformIndex, Property, Type};

const CLIENT_ONLY: &[&str] = &[
    "Тонкий клиент",
    "веб-клиент",
    "мобильный клиент",
    "толстый клиент",
    "мобильное приложение (клиент)",
];
const SERVER_ONLY: &[&str] = &[
    "Сервер",
    "толстый клиент",
    "внешнее соединение",
    "мобильное приложение (сервер)",
    "мобильный автономный сервер",
];

const OBJECT: &str = "Catalogs/Тест/Ext/ObjectModule.bsl";
const MANAGER: &str = "Catalogs/Тест/Ext/ManagerModule.bsl";
const FORM: &str = "Documents/Заказ/Forms/ФормаДокумента/Ext/Form/Module.bsl";
const COMMON: &str = "CommonModules/Тест/Ext/Module.bsl";

fn availability(labels: &[&str]) -> Availability {
    Availability::new(
        labels
            .iter()
            .map(|label| AvailabilityMark::from_label(label))
            .collect(),
    )
}

fn method(name: &str, labels: Option<&[&str]>) -> Method {
    Method {
        name_ru: name.to_string(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        return_type: String::new(),
        signatures: Vec::new(),
        availability: labels.map(availability),
    }
}

fn property(name: &str, labels: Option<&[&str]>) -> Property {
    Property {
        name_ru: name.to_string(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        type_name: String::new(),
        readonly: true,
        availability: labels.map(availability),
    }
}

fn ty(
    name: &str,
    labels: Option<&[&str]>,
    methods: Vec<Method>,
    properties: Vec<Property>,
) -> Type {
    Type {
        name_ru: name.to_string(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        methods,
        properties,
        constructors: Vec::new(),
        enum_values: Vec::new(),
        availability: labels.map(availability),
    }
}

/// Платформенный индекс фикстуры: коды доступности — как у реальных страниц
/// справки по видам контекста.
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.global_methods = vec![
        // Только клиент: на сервере не компилируется.
        method("ПоказатьВопрос", Some(CLIENT_ONLY)),
        // Только сервер (и обычный клиент): в клиенте управляемой формы нельзя.
        method("ЗаписьЖурналаРегистрации", Some(SERVER_ONLY)),
        // Доступен обеим сторонам.
        method("ОбеСтороны", Some(&["Сервер", "Тонкий клиент"])),
        // Единственная клиентская метка — «толстый клиент» (раздел 6.5.1).
        method("ТолькоТолстый", Some(&["толстый клиент"])),
        // Неизвестная метка «интеграция»: список неполон, решать нельзя.
        method("ИнтеграцияМетод", Some(&["Сервер", "интеграция"])),
        // Страницы без главы «Доступность:» данных не дают.
        method("БезДоступности", None),
    ];
    index.insert_type(ty(
        "ТаблицаЗначений",
        Some(SERVER_ONLY),
        vec![method("Найти", Some(SERVER_ONLY))],
        vec![property("Количество", Some(SERVER_ONLY))],
    ));
    index.insert_type(ty("Запрос", Some(SERVER_ONLY), Vec::new(), Vec::new()));
    index.insert_type(ty("COMОбъект", Some(CLIENT_ONLY), Vec::new(), Vec::new()));
    // Тип без данных о доступности — для проверки «второй альтернативы».
    index.insert_type(ty("Массив", None, Vec::new(), Vec::new()));
    index
}

/// Находки именно нового правила: остальные проверки модуля в этом файле не
/// интересуют.
fn findings(
    index: &PlatformIndex,
    source: &str,
    path: &str,
    form: Option<FormKind>,
) -> Vec<ExprError> {
    validate_module_with_symbols_and_form_kind(
        index,
        source,
        1,
        Profile::Full,
        Some(path),
        None,
        None,
        form,
        None,
    )
    .errors
    .into_iter()
    .filter(|error| error.kind == ExprErrorKind::UnavailableInContext)
    .collect()
}

const QUESTION_CALL: &str = "Процедура П()\n    ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";

/// Кейс 1: Клиентский метод в модуле объекта: серверный код, находка.
#[test]
fn client_only_global_on_server_is_flagged() {
    let found = findings(&index(), QUESTION_CALL, OBJECT, None);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].confidence, Confidence::High);
    assert!(
        found[0].message.contains("ПоказатьВопрос")
            && found[0].message.contains("недоступен на сервере"),
        "{}",
        found[0].message
    );
    assert!(
        found[0].suggestion.is_none(),
        "перенос в другую процедуру — не автоправка"
    );
}

/// Кейс 2: Серверный метод в модуле объекта: контекст совпадает, молчание.
#[test]
fn server_only_global_on_server_is_silent() {
    let src = "Процедура П()\n    ЗаписьЖурналаРегистрации();\nКонецПроцедуры\n";
    assert!(findings(&index(), src, OBJECT, None).is_empty());
}

/// Кейс 3: Конструктор серверного типа в модуле менеджера: молчание.
#[test]
fn server_constructor_on_server_is_silent() {
    let src = "Процедура П()\n    Запрос = Новый Запрос;\nКонецПроцедуры\n";
    assert!(findings(&index(), src, MANAGER, None).is_empty());
}

/// Кейс 4: Конструктор клиентского типа в модуле менеджера: молчание.
///
/// Доступность ТИПА не описывает доступность КОНСТРУКТОРА на сервере: платформа
/// допускает клиентский тип в конструкторе на сервере (`Новый СочетаниеКлавиш` в
/// `&НаСервере` — вендорский код УТ, 35 мест), поэтому серверное направление
/// конструкторов не проверяется. Клиентское — проверяется (кейсы ниже).
#[test]
fn client_only_constructor_on_server_is_silent() {
    let src = "Процедура П()\n    Объект = Новый COMОбъект(\"\");\nКонецПроцедуры\n";
    assert!(findings(&index(), src, MANAGER, None).is_empty());
}

/// Кейс 5: Серверный метод в `&НаКлиенте`: находка; в метках есть «толстый клиент»,
/// поэтому уверенность понижена до `Medium`.
#[test]
fn server_only_global_in_managed_client_is_flagged() {
    let src = "&НаКлиенте\nПроцедура П()\n    ЗаписьЖурналаРегистрации();\nКонецПроцедуры\n";
    let found = findings(&index(), src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].confidence, Confidence::Medium);
    assert!(
        found[0]
            .message
            .contains("недоступен в клиенте управляемого приложения"),
        "{}",
        found[0].message
    );
}

/// Кейс 6: Конструктор серверного типа в `&НаКлиенте`: находка.
#[test]
fn server_constructor_in_managed_client_is_flagged() {
    let src = "&НаКлиенте\nПроцедура П()\n    Запрос = Новый Запрос;\nКонецПроцедуры\n";
    let found = findings(&index(), src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].message.contains("Тип `Запрос`")
            && found[0]
                .message
                .contains("недоступен в клиенте управляемого приложения"),
        "{}",
        found[0].message
    );
}

/// Кейс 7: Клиентский метод в `&НаСервере`: находка (серверная сторона).
#[test]
fn client_only_global_in_server_procedure_is_flagged() {
    let src = "&НаСервере\nПроцедура П()\n    ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";
    let found = findings(&index(), src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].confidence, Confidence::High);
}

/// Кейс 8: Клиентский метод в `&НаСервереБезКонтекста`: та же серверная часть.
#[test]
fn client_only_global_in_no_context_procedure_is_flagged() {
    let src =
        "&НаСервереБезКонтекста\nПроцедура П()\n    ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";
    let found = findings(&index(), src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
}

/// Кейс 9: `&НаКлиентеНаСервере`: клиентский метод доступен хотя бы одной стороне —
/// молчание.
#[test]
fn client_only_global_in_both_procedure_is_silent() {
    let src =
        "&НаКлиентеНаСервере\nПроцедура П()\n    ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Managed)).is_empty());
}

/// Кейс 10: Процедура без директивы в управляемой форме: контекст неизвестен —
/// молчание на оба направления.
#[test]
fn procedure_without_directive_is_silent() {
    let src = "Процедура П()\n    ПоказатьВопрос(\"Вопрос\");\n    ЗаписьЖурналаРегистрации();\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Managed)).is_empty());
}

/// Кейс 11: Обычная форма: клиентская и серверная части не разделены — проверка
/// выключена целиком.
#[test]
fn ordinary_form_is_silent() {
    let src = "Процедура П()\n    ПоказатьВопрос(\"Вопрос\");\n    ЗаписьЖурналаРегистрации();\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Ordinary)).is_empty());
}

/// Кейс 12: Общий модуль: контексты задаются свойствами, которых валидатор не видит.
#[test]
fn common_module_is_silent() {
    assert!(findings(&index(), QUESTION_CALL, COMMON, None).is_empty());
}

/// Кейс 13: Расширение (`&Вместо`): контекст наследуется от расширяемой процедуры.
#[test]
fn extension_directive_is_silent() {
    let src = "&Вместо(\"Х\")\nПроцедура П()\n    ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";
    assert!(findings(&index(), src, OBJECT, None).is_empty());
}

/// Кейс 13a: Расширение с КОНТЕКСТНОЙ директивой (`&НаСервере` + `&Вместо`).
///
/// `directive_of` возвращает контекстную директиву, поэтому без отдельного
/// признака расширения процедура выглядела бы обычной серверной и вызов
/// клиентского метода дал бы находку (дефект, найденный приёмщиком 09.11.2026).
#[test]
fn extension_behind_context_directive_is_silent() {
    let src = "&НаСервере\n&Вместо(\"Х\")\nПроцедура П()\n    ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";
    assert!(findings(&index(), src, OBJECT, None).is_empty());
}

/// Кейс 14: Вызов внутри `#Если … #КонецЕсли`: ветка зависит от сборки — молчание.
#[test]
fn call_inside_preproc_region_is_silent() {
    let src = "Процедура П()\n#Если Сервер Тогда\n    ПоказатьВопрос(\"Вопрос\");\n#КонецЕсли\nКонецПроцедуры\n";
    assert!(findings(&index(), src, OBJECT, None).is_empty());
}

/// Кейс 15: Неизвестная метка «интеграция»: список неполон — молчание.
#[test]
fn integration_mark_is_silent() {
    let src = "&НаКлиенте\nПроцедура П()\n    ИнтеграцияМетод();\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Managed)).is_empty());
}

/// Кейс 16: Страница без главы «Доступность:»: данных нет — молчание.
#[test]
fn page_without_availability_is_silent() {
    let src = "Процедура П()\n    БезДоступности();\nКонецПроцедуры\n";
    assert!(findings(&index(), src, OBJECT, None).is_empty());
}

/// Кейс 17: Тип разрешён неоднозначно (объединение двух конструкторов в ветвях):
/// доступность чужого типа не применяется — молчание.
#[test]
fn ambiguous_type_is_silent() {
    let src = "\
Процедура П()
    Если Истина Тогда
        Таблица = Новый ТаблицаЗначений;
    Иначе
        Таблица = Новый Запрос;
    КонецЕсли;
    Таблица.Найти(\"а\");
КонецПроцедуры
";
    assert!(
        findings(&index(), src, OBJECT, None).is_empty(),
        "метод есть у одной из альтернатив, доступность второй не должна давать находку"
    );
}

/// Кейс 18: Метод типа у однозначно разрешённого типа: серверный `Найти` в клиенте —
/// находка.
#[test]
fn type_member_in_managed_client_is_flagged() {
    let src = "&НаКлиенте\nПроцедура П()\n    Таблица = Новый ТаблицаЗначений;\n    Таблица.Найти(\"а\");\nКонецПроцедуры\n";
    let found: Vec<ExprError> = findings(&index(), src, FORM, Some(FormKind::Managed))
        .into_iter()
        .filter(|error| error.message.contains("Метод `Найти`"))
        .collect();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].message.contains("типа `ТаблицаЗначений`"),
        "{}",
        found[0].message
    );
}

/// Кейс 19: Обращение к свойству без вызова не проверяется (раздел 6.1 плана).
#[test]
fn property_access_without_call_is_not_checked() {
    let src = "&НаКлиенте\nПроцедура П()\n    Х = ТаблицаЗначений.Количество;\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Managed)).is_empty());
}

/// Кейс 20: `Коллекция.Объект.Метод(...)` не проверяется (раздел 6.1 плана).
#[test]
fn manager_call_is_not_checked() {
    let src = "Процедура П()\n    Справочники.Тест.ПоказатьВопрос(\"Вопрос\");\nКонецПроцедуры\n";
    assert!(findings(&index(), src, OBJECT, None).is_empty());
}

/// Кейс 21: Метод с единственной клиентской меткой «толстый клиент» в клиенте
/// управляемой формы: находка есть, но уверенность `Medium` (раздел 6.5.1).
#[test]
fn thick_client_only_method_is_medium() {
    let src = "&НаКлиенте\nПроцедура П()\n    ТолькоТолстый();\nКонецПроцедуры\n";
    let found = findings(&index(), src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].confidence, Confidence::Medium);
}

/// Кейс 22: `&НаКлиентеНаСервере`, метод недоступен обеим сторонам: находка с
/// формулировкой «ни на сервере, ни в клиенте».
#[test]
fn both_context_requires_at_least_one_side() {
    let src = "&НаКлиентеНаСервере\nПроцедура П()\n    ТолькоТолстый();\nКонецПроцедуры\n";
    let found = findings(&index(), src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0]
            .message
            .contains("ни на сервере, ни в клиенте управляемого приложения"),
        "{}",
        found[0].message
    );
}

/// Кейс 23: Контрольный положительный случай для `Both`: метод доступен серверу —
/// молчание, хотя в клиенте его нет.
#[test]
fn both_context_is_silent_when_one_side_available() {
    let src =
        "&НаКлиентеНаСервере\nПроцедура П()\n    ЗаписьЖурналаРегистрации();\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Managed)).is_empty());
}

/// Кейс 24: Модуль без пути (фрагмент) правило не проверяет: контекст неизвестен.
#[test]
fn fragment_without_path_is_silent() {
    let result = validate_module_with_symbols_and_form_kind(
        &index(),
        QUESTION_CALL,
        1,
        Profile::Full,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.kind == ExprErrorKind::UnavailableInContext),
        "{:#?}",
        result.errors
    );
}

/// Серверный метод, доступный только серверу (без «толстого клиента») в клиенте:
/// уверенность высокая — трактовка метки ни при чём.
#[test]
fn server_only_without_thick_client_is_high() {
    let mut index = index();
    index.global_methods.push(method(
        "ТолькоСервер",
        Some(&["Сервер", "внешнее соединение"]),
    ));
    let src = "&НаКлиенте\nПроцедура П()\n    ТолькоСервер();\nКонецПроцедуры\n";
    let found = findings(&index, src, FORM, Some(FormKind::Managed));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].confidence, Confidence::High);
}

/// Доступность типа-конструктора в обычной форме не проверяется (пункт 11).
#[test]
fn ordinary_form_constructor_is_silent() {
    let src = "Процедура П()\n    Запрос = Новый Запрос;\nКонецПроцедуры\n";
    assert!(findings(&index(), src, FORM, Some(FormKind::Ordinary)).is_empty());
}

/// `#Если` внутри модуля менеджера тоже глушит вызов.
#[test]
fn preproc_region_silences_manager_module() {
    let src = "Процедура П()\n#Если ТолстыйКлиентУправляемоеПриложение Тогда\n    ПоказатьВопрос(\"Вопрос\");\n#КонецЕсли\nКонецПроцедуры\n";
    assert!(findings(&index(), src, MANAGER, None).is_empty());
}
