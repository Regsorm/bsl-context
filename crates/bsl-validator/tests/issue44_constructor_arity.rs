//! Интеграционные тесты issue #44: число аргументов КОНСТРУКТОРА.
//!
//! До правки аргументы `Новый <Тип>(...)` не считались вовсе, и вызов с лишними
//! аргументами проходил молча, хотя справка описывает параметры конструктора.
//! Индекс синтетический — справка не нужна; проверяется и счёт аргументов
//! (он приходит из `bsl-parse`), и сама сверка.
//!
//! Проверяется только верхняя граница: «мало аргументов» — класс, где справка
//! расходится с платформой, и на первом заходе он не трогается.

use bsl_validator::{validate_module_with_symbols, ExprError, ExprErrorKind, Profile};
use platform_index::{Constructor, Parameter, PlatformIndex, Type};

fn param(name: &str, required: bool) -> Parameter {
    Parameter {
        name: name.to_string(),
        type_name: String::new(),
        required,
        description: String::new(),
    }
}

fn ctor(name: &str, params: Vec<Parameter>) -> Constructor {
    Constructor {
        name: name.to_string(),
        name_en: String::new(),
        syntax: String::new(),
        description: String::new(),
        note: None,
        parameters: params,
    }
}

fn ty(name: &str, constructors: Vec<Constructor>) -> Type {
    Type {
        name_ru: name.to_string(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        methods: Vec::new(),
        properties: Vec::new(),
        constructors,
        enum_values: Vec::new(),
    }
}

/// Индекс: `Файл` (один обязательный параметр), `Массив` (вариативный конструктор),
/// `ТаблицаЗначений` (конструктор без параметров), `ОписаниеТипов` (7 опциональных),
/// `Двухвидовой` (две перегрузки), `БезКонструкторов` (справка молчит).
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.insert_type(ty(
        "Файл",
        vec![ctor("По имени файла", vec![param("Имя", true)])],
    ));
    index.insert_type(ty(
        "Массив",
        vec![
            ctor(
                "На основании фиксированного массива",
                vec![param("Массив", false)],
            ),
            ctor("По количеству элементов", vec![param("Количество", false)]),
        ],
    ));
    index.insert_type(ty(
        "ТаблицаЗначений",
        vec![ctor("По умолчанию", Vec::new())],
    ));
    // ФорматированнаяСтрока: справка описывает 1..5 параметров, платформа
    // принимает сколько угодно пар значение-оформление. На выгрузке УТ такие
    // вызовы есть (5–9 аргументов) — правка без этого типа дала 12 ложных находок.
    index.insert_type(ty(
        "ФорматированнаяСтрока",
        vec![
            ctor(
                "Из значения и оформления",
                (1..=5)
                    .map(|i| param(&format!("Параметр{i}"), false))
                    .collect(),
            ),
            ctor("По умолчанию", Vec::new()),
        ],
    ));
    index.insert_type(ty(
        "ОписаниеТипов",
        vec![ctor(
            "На основании объекта ОписаниеТипов",
            (1..=7)
                .map(|i| param(&format!("Параметр{i}"), false))
                .collect(),
        )],
    ));
    index.insert_type(ty(
        "Двухвидовой",
        vec![
            ctor("Пустой", Vec::new()),
            ctor("Пара", vec![param("Ключ", true), param("Значение", true)]),
        ],
    ));
    index.insert_type(ty("БезКонструкторов", Vec::new()));
    index
}

fn wrong_argument_count(src: &str) -> Vec<ExprError> {
    validate_module_with_symbols(&index(), src, 3, Profile::Full, None, None, None)
        .errors
        .into_iter()
        .filter(|e| e.kind == ExprErrorKind::WrongArgumentCount)
        .collect()
}

fn new_expr(expr: &str) -> String {
    format!("Процедура Зонд()\n\t{expr};\nКонецПроцедуры\n")
}

// ── Лишние аргументы — находка ───────────────────────────────────────────────

#[test]
fn file_with_three_arguments_is_reported() {
    let found = wrong_argument_count(&new_expr("Ф = Новый Файл(\"a\", \"b\", \"c\")"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].message.contains("Файл"), "{found:?}");
    assert!(found[0].message.contains("Допустимо"), "{found:?}");
}

#[test]
fn value_table_with_arguments_is_reported() {
    let found = wrong_argument_count(&new_expr("Т = Новый ТаблицаЗначений(1, 2)"));
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn nine_arguments_for_seven_optional_is_reported() {
    let found = wrong_argument_count(&new_expr(
        "З = Новый ОписаниеТипов(\"Строка\", , , , , , , , , )",
    ));
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn widest_constructor_sets_the_bound() {
    // Две перегрузки (0 и 2 параметра): три аргумента — уже мимо обеих.
    let found = wrong_argument_count(&new_expr("Д = Новый Двухвидовой(1, 2, 3)"));
    assert_eq!(found.len(), 1, "{found:?}");
}

// ── Корректный код молчит ────────────────────────────────────────────────────

#[test]
fn file_with_one_argument_is_silent() {
    assert!(wrong_argument_count(&new_expr("Ф = Новый Файл(\"a\")")).is_empty());
}

#[test]
fn file_without_arguments_is_silent() {
    // Ветка «мало аргументов» не проверяется: справка помечает параметр
    // обязательным не всегда верно.
    assert!(wrong_argument_count(&new_expr("Ф = Новый Файл")).is_empty());
}

#[test]
fn variadic_array_is_silent() {
    for expr in [
        "М = Новый Массив(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)",
        "М = Новый Массив(1)",
        "М = Новый Массив",
    ] {
        let found = wrong_argument_count(&new_expr(expr));
        assert!(found.is_empty(), "{expr}: {found:?}");
    }
}

#[test]
fn variadic_formatted_string_is_silent() {
    // Вызовы из выгрузки УТ: пар значение-оформление больше, чем в справке.
    for expr in [
        "С = Новый ФорматированнаяСтрока(\"а\", Новый Шрифт(,,Истина))",
        "С = Новый ФорматированнаяСтрока(\" | \", А, \" \", Б, В, Г, Д)",
        "С = Новый ФорматированнаяСтрока",
    ] {
        let found = wrong_argument_count(&new_expr(expr));
        assert!(found.is_empty(), "{expr}: {found:?}");
    }
}

#[test]
fn second_constructor_of_type_accepts_the_call() {
    // Две перегрузки: два аргумента попадают во вторую — молчание.
    assert!(wrong_argument_count(&new_expr("Д = Новый Двухвидовой(\"к\", 1)")).is_empty());
}

#[test]
fn type_without_constructors_is_silent() {
    assert!(wrong_argument_count(&new_expr("Б = Новый БезКонструкторов(1, 2, 3)")).is_empty());
}

#[test]
fn arguments_inside_literals_are_not_counted() {
    // Запятые внутри строкового литерала аргументами не считаются (тот же счётчик,
    // что у вызовов методов).
    let found = wrong_argument_count(&new_expr("Ф = Новый Файл(\"a,b,c,d,e\")"));
    assert!(found.is_empty(), "{found:?}");
}
