//! Интеграционные тесты трёх классов ложных `unknown_type_member` (issue #36)
//! на СИНТЕТИЧЕСКОМ индексе платформы — гоняются обычным `cargo test -p
//! bsl-validator`, справка не нужна. Источник имён — стаб, умеющий отвечать на
//! `object_exists` (`SessionParameters`) и `object_schema` (`AccumulationRegisters`).
//!
//! Классы:
//! 1. `ПараметрыСеанса.<параметр>` — состав объявляет конфигурация, в справке
//!    членов у типа нет; параметр сверяется с источником, а не со справкой.
//! 2. Переменная цикла `Для Каждого` переопределяет имя с начала тела цикла:
//!    тип из присваивания ВЫШЕ цикла к телу не относится.
//! 3. `Отбор` набора записей регистра — фильтр набора: поля = измерения регистра
//!    + стандартные поля, а не члены одноимённого платформенного типа.

use std::collections::HashSet;

use bsl_validator::{
    validate_module_with_symbols, ExprErrorKind, ObjectField, ObjectSchema, Profile, SymbolSource,
};
use platform_index::{Method, PlatformIndex, Property, Type};

/// Источник-заглушка: параметры сеанса и схема одного регистра накопления.
struct Stub {
    /// Параметры сеанса, которые «есть в конфигурации» (нижний регистр).
    session_params: HashSet<String>,
    /// `true` — отвечать «не знаю» на всё (как ненастроенный источник).
    silent: bool,
}

impl Stub {
    fn known() -> Self {
        Self {
            session_params: ["версиярасширений"].into_iter().map(String::from).collect(),
            silent: false,
        }
    }

    fn silent() -> Self {
        Self {
            silent: true,
            ..Self::known()
        }
    }
}

impl SymbolSource for Stub {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        if self.silent {
            return None;
        }
        match collection {
            "SessionParameters" => Some(self.session_params.contains(name_lower)),
            _ => None,
        }
    }

    fn object_schema(&self, collection: &str, name_lower: &str) -> Option<ObjectSchema> {
        if self.silent {
            return None;
        }
        if collection != "AccumulationRegisters" || name_lower != "товарынаскладах" {
            return None;
        }
        Some(ObjectSchema {
            attributes: Vec::new(),
            dimensions: ["Номенклатура", "Организация"]
                .into_iter()
                .map(|n| ObjectField {
                    name: n.to_string(),
                    indexing: None,
                })
                .collect(),
            resources: Vec::new(),
            register_type: Some("Balance".to_string()),
        })
    }

    fn describe(&self) -> String {
        "stub-issue36".to_string()
    }
}

/// Синтетический индекс: типы `ПараметрыСеанса` (метод `Очистить`, без свойств),
/// `Отбор` (метод `Добавить`, без свойств), `Массив` и `Соответствие` — и
/// глобальное свойство `ПараметрыСеанса` (только чтение, тип совпадает с именем).
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();

    index.global_properties.push(Property {
        name_ru: "ПараметрыСеанса".into(),
        name_en: "SessionParameters".into(),
        description: String::new(),
        type_name: "ПараметрыСеанса".into(),
        readonly: true,
        note: None,
    });

    let ty = |name: &str, methods: Vec<&str>| Type {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        methods: methods
            .into_iter()
            .map(|m| Method {
                name_ru: m.into(),
                name_en: String::new(),
                description: String::new(),
                return_type: String::new(),
                signatures: Vec::new(),
                note: None,
            })
            .collect(),
        properties: Vec::new(),
        constructors: Vec::new(),
        enum_values: Vec::new(),
        note: None,
    };
    index.insert_type(ty("ПараметрыСеанса", vec!["Очистить"]));
    index.insert_type(ty("Отбор", vec!["Добавить"]));
    index.insert_type(ty("Массив", vec!["Добавить"]));
    index.insert_type(ty("Соответствие", Vec::new()));

    index
}

/// Находки `UnknownTypeMember` после проверки фрагмента/модуля.
fn unknown_type_member(
    src: &str,
    level: u8,
    module_path: Option<&str>,
    symbols: Option<&dyn SymbolSource>,
) -> Vec<String> {
    let result = validate_module_with_symbols(
        &index(),
        src,
        level,
        Profile::Full,
        module_path,
        None,
        symbols,
    );
    result
        .errors
        .into_iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownTypeMember)
        .map(|e| e.message)
        .collect()
}

// ── Класс 1: ПараметрыСеанса.<параметр> ─────────────────────────────────────

#[test]
fn session_parameter_that_exists_is_silent() {
    let src = "Процедура Т()\nП = ПараметрыСеанса.ВерсияРасширений;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, None, Some(&Stub::known()));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn invented_session_parameter_is_reported() {
    let src = "Процедура Т()\nП = ПараметрыСеанса.НетТакогоПараметра123;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, None, Some(&Stub::known()));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("ПараметрыСеанса"), "{errors:?}");
}

#[test]
fn session_parameter_without_source_is_silent() {
    // Без источника (или источник не знает коллекцию) — молчание, а не ложная
    // находка: прежнее поведение без имён конфигурации.
    let src = "Процедура Т()\nП = ПараметрыСеанса.ВерсияРасширений;\nКонецПроцедуры\n";
    let silent = Stub::silent();
    let sources: [Option<&dyn SymbolSource>; 2] = [None, Some(&silent)];
    for symbols in sources {
        let errors = unknown_type_member(src, 3, None, symbols);
        assert!(errors.is_empty(), "{errors:?}");
    }
}

#[test]
fn session_parameters_method_is_still_checked() {
    // Метод `Очистить` объявлен в справке, а не конфигурацией — его опечатка
    // обязана остаться находкой (правило класса 1 касается только свойств).
    let src = "Процедура Т()\nПараметрыСеанса.Очистьть();\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, None, Some(&Stub::known()));
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── Класс 2: переменная цикла сбрасывает прежний тип ─────────────────────────

#[test]
fn loop_variable_resets_type_from_assignment_above() {
    let src = "Процедура Т()\nХ = Новый Массив;\nС = Новый Соответствие;\nДля Каждого Х Из С Цикл\nЗ = Х.Значение;\nКонецЦикла;\nКонецПроцедуры\n";
    // До правки `Х.Значение` давало «У типа 'Массив' нет члена 'Значение'».
    let errors = unknown_type_member(src, 3, None, Some(&Stub::known()));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn type_from_assignment_above_loop_still_applies_above_loop() {
    // Контроль: ВЫШЕ цикла прежний тип действует — правило не выключает проверку
    // целиком, а только с начала тела цикла.
    let src = "Процедура Т()\nХ = Новый Массив;\nХ.НетТакогоЧлена123();\nДля Каждого Х Из Новый Массив Цикл\nКонецЦикла;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, None, Some(&Stub::known()));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Массив"), "{errors:?}");
}

// ── Класс 3: Отбор набора записей регистра ──────────────────────────────────

const RECORD_SET: &str = "AccumulationRegisters/ТоварыНаСкладах/Ext/RecordSetModule.bsl";

#[test]
fn record_set_filter_dimension_is_silent() {
    let src = "Процедура Т()\nР = Отбор.Номенклатура.Значение;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, Some(RECORD_SET), Some(&Stub::known()));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn record_set_filter_standard_field_is_silent() {
    let src = "Процедура Т()\nР = Отбор.Регистратор.Значение;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, Some(RECORD_SET), Some(&Stub::known()));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn record_set_filter_under_dump_wrapper_is_silent() {
    // Выгрузка целиком: перед коллекцией стоит каталог-обёртка `base` — `owner_of`
    // такую раскладку не разбирает, но модуль набора записей опознаётся.
    let src = "Процедура Т()\nР = Отбор.Регистратор.Значение;\nКонецПроцедуры\n";
    let errors = unknown_type_member(
        src,
        3,
        Some("base/AccumulationRegisters/ТоварыНаСкладах/Ext/RecordSetModule.bsl"),
        Some(&Stub::known()),
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn invented_record_set_filter_field_is_reported() {
    let src = "Процедура Т()\nР = Отбор.НетТакогоИмени123;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, Some(RECORD_SET), Some(&Stub::known()));
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn record_set_filter_without_schema_is_silent() {
    // Состав взять неоткуда (источник молчит) — обращение к свойству `Отбор` не
    // проверяем, как у `XBase` (#31).
    let src = "Процедура Т()\nР = Отбор.Регистратор.Значение;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, Some(RECORD_SET), Some(&Stub::silent()));
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn record_set_filter_methods_are_still_checked() {
    // Методы `Отбор` сверяются с платформенным типом как обычно: `Добавить` есть,
    // `Вставить` — нет (в справке у `Отбор` только `Добавить`, не `Вставить`).
    let ok = unknown_type_member(
        "Процедура Т()\nОтбор.Добавить(\"Регистратор\", Неопределено);\nКонецПроцедуры\n",
        3,
        Some(RECORD_SET),
        Some(&Stub::known()),
    );
    assert!(ok.is_empty(), "{ok:?}");

    let bad = unknown_type_member(
        "Процедура Т()\nОтбор.Вставить(\"Регистратор\", Неопределено);\nКонецПроцедуры\n",
        3,
        Some(RECORD_SET),
        Some(&Stub::known()),
    );
    assert_eq!(bad.len(), 1, "{bad:?}");
}

#[test]
fn otbor_outside_record_set_module_is_unchanged() {
    // Вне модуля набора записей (например, в общем модуле) `Отбор` остаётся
    // обычным платформенным типом: у него нет свойства `Регистратор` — находка.
    let src = "Процедура Т()\nР = Отбор.Регистратор;\nКонецПроцедуры\n";
    let errors = unknown_type_member(src, 3, None, Some(&Stub::known()));
    assert_eq!(errors.len(), 1, "{errors:?}");
}
