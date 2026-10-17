//! Тесты трёх классов ложных `unknown_type_member` (issue #36) на НАСТОЯЩЕЙ
//! справке платформы (`shcntx_ru.hbk`). Пропускаются, если справки нет —
//! `BSL_CONTEXT_PLATFORM_PATH` не задан.
//!
//! Источник имён конфигурации — стаб (у настоящего источника своя приёмка):
//! здесь проверяется связка «реальный платформенный тип + правило класса».
//!
//! ```pwsh
//! $env:BSL_CONTEXT_PLATFORM_PATH = "C:\Program Files\1cv8\8.3.27.1786"
//! cargo test -p bsl-validator --test real_issue36 -- --nocapture
//! ```

use std::collections::HashSet;
use std::path::PathBuf;

use bsl_validator::{
    validate_module_with_symbols, ExprErrorKind, ObjectField, ObjectSchema, Profile, SymbolSource,
};
use platform_index::load_from_hbk;

fn hbk_path() -> Option<PathBuf> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH")
        .ok()
        .map(PathBuf::from)?;
    let candidates = [
        root.join("shcntx_ru.hbk"),
        root.join("bin").join("shcntx_ru.hbk"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Стаб источника имён: параметры сеанса и схема регистра накопления.
struct Stub {
    session_params: HashSet<String>,
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
            tabular_sections: Vec::new(),
            register_type: Some("Balance".to_string()),
            enum_values: Vec::new(),
        })
    }

    fn describe(&self) -> String {
        "stub-real-issue36".to_string()
    }
}

fn unknown_type_member(
    index: &platform_index::PlatformIndex,
    src: &str,
    module_path: Option<&str>,
    symbols: Option<&dyn SymbolSource>,
) -> Vec<String> {
    let result =
        validate_module_with_symbols(index, src, 3, Profile::Full, module_path, None, symbols);
    result
        .errors
        .into_iter()
        .filter(|e| e.kind == ExprErrorKind::UnknownTypeMember)
        .map(|e| e.message)
        .collect()
}

/// Класс 1: `ПараметрыСеанса.<параметр>` — состав объявляет конфигурация,
/// в справке у типа `ПараметрыСеанса` членов-свойств нет.
#[test]
fn session_parameter_on_real_help() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    // Параметр «есть в конфигурации» (стаб) — молчание.
    let known = "Процедура Т()\nП = ПараметрыСеанса.ВерсияРасширений;\nКонецПроцедуры\n";
    assert!(
        unknown_type_member(&index, known, None, Some(&Stub::known())).is_empty(),
        "существующий параметр сеанса — молчание"
    );

    // Выдуманный параметр — находка.
    let invented = "Процедура Т()\nП = ПараметрыСеанса.НетТакогоПараметра123;\nКонецПроцедуры\n";
    assert_eq!(
        unknown_type_member(&index, invented, None, Some(&Stub::known())).len(),
        1,
        "выдуманный параметр сеанса — находка"
    );

    // Метод `Очистить` объявлен в справке — его опечатка обязана остаться находкой.
    let method_typo = "Процедура Т()\nПараметрыСеанса.Очистьть();\nКонецПроцедуры\n";
    assert_eq!(
        unknown_type_member(&index, method_typo, None, Some(&Stub::known())).len(),
        1,
        "опечатка в методе ПараметрыСеанса — находка"
    );
    let method_ok = "Процедура Т()\nПараметрыСеанса.Очистить();\nКонецПроцедуры\n";
    assert!(
        unknown_type_member(&index, method_ok, None, Some(&Stub::known())).is_empty(),
        "метод Очистить существует"
    );
}

/// Класс 2: переменная цикла `Для Каждого` сбрасывает прежний тип с начала тела
/// цикла. На справке `Массив` есть, и до правки `Х.Значение` давало находку.
#[test]
fn loop_variable_on_real_help() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    let src = "Процедура Т()\nХ = Новый Массив;\nС = Новый Соответствие;\n\
               Для Каждого Х Из С Цикл\nЗ = Х.Значение;\nКонецЦикла;\nКонецПроцедуры\n";
    let errors = unknown_type_member(&index, src, None, None);
    assert!(errors.is_empty(), "{errors:?}");
}

/// Класс 3: `Отбор` набора записей регистра. В справке тип `Отбор` — фильтр с
/// методами и БЕЗ свойств; в модуле набора записей его поля — измерения регистра
/// плюс стандартные поля.
#[test]
fn record_set_filter_on_real_help() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let module = "AccumulationRegisters/ТоварыНаСкладах/Ext/RecordSetModule.bsl";

    for code in [
        "Р = Отбор.Регистратор.Значение;",
        "Р = Отбор.Номенклатура.Значение;",
    ] {
        let src = format!("Процедура Т()\n{code}\nКонецПроцедуры\n");
        assert!(
            unknown_type_member(&index, &src, Some(module), Some(&Stub::known())).is_empty(),
            "поле фильтра набора записей '{code}' — молчание"
        );
    }

    let invented = "Процедура Т()\nР = Отбор.НетТакогоИмени123;\nКонецПроцедуры\n";
    assert_eq!(
        unknown_type_member(&index, invented, Some(module), Some(&Stub::known())).len(),
        1,
        "выдуманное поле фильтра — находка"
    );

    // Без состава (источник молчит) свойства `Отбор` не проверяются.
    let no_schema = "Процедура Т()\nР = Отбор.Регистратор.Значение;\nКонецПроцедуры\n";
    assert!(
        unknown_type_member(&index, no_schema, Some(module), Some(&Stub::silent())).is_empty(),
        "без состава свойства Отбор молчат"
    );

    // Методы `Отбор` сверяются со справкой как обычно: `Добавить` есть,
    // `Вставить` — нет (проверено по `get_members`: у типа `Отбор` только `Добавить`).
    let method_ok =
        "Процедура Т()\nОтбор.Добавить(\"Регистратор\", Неопределено);\nКонецПроцедуры\n";
    assert!(
        unknown_type_member(&index, method_ok, Some(module), Some(&Stub::known())).is_empty(),
        "метод Добавить у Отбор существует"
    );
    let method_bad =
        "Процедура Т()\nОтбор.Вставить(\"Регистратор\", Неопределено);\nКонецПроцедуры\n";
    assert_eq!(
        unknown_type_member(&index, method_bad, Some(module), Some(&Stub::known())).len(),
        1,
        "у типа Отбор нет метода Вставить — находка верна"
    );
}
