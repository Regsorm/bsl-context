//! Issue #40, дополнение автора: дополнения к справке из файла.
//!
//! Автор дописывал недостающие методы прямо в дисковый кэш платформенного
//! индекса — это требовало знать внутренний формат и молча пропадало при
//! обновлении сервера. Здесь проверяется то же поведение, но через файл
//! дополнений: метод, которого нет в справке, после наложения не даёт находки,
//! а его арность проверяется; без дополнения тот же код — находка.

use bsl_context_server::platform_overlay;
use bsl_validator::{validate_module_with_profile, ExprErrorKind, Profile};
use platform_index::{PlatformIndex, Type};

/// Имя, которого нет ни в справке, ни в списке устаревших глобальных методов:
/// проверяем МЕХАНИЗМ дополнения, а не конкретное имя из issue.
const SRC_METHOD: &str = "Процедура Зонд()\n\
                          \tУстаревшийМетодПробы(\"Заголовок\");\n\
                          \tУстаревшийМетодПробы(\"а\", \"б\", \"в\");\n\
                          КонецПроцедуры\n";

const SRC_MEMBER: &str = "Процедура Зонд()\n\
                          \tМ = Новый Массив;\n\
                          \tМ.УстаревшийЧлен();\n\
                          КонецПроцедуры\n";

/// Индекс с одним типом `Массив` — глобальных методов в нём нет вовсе.
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.insert_type(Type {
        name_ru: "Массив".to_string(),
        name_en: "Array".to_string(),
        description: String::new(),
        note: None,
        methods: Vec::new(),
        properties: Vec::new(),
        constructors: Vec::new(),
        enum_values: Vec::new(),
    });
    index
}

fn kinds(source: &str, index: &PlatformIndex) -> Vec<ExprErrorKind> {
    validate_module_with_profile(index, source, None, None, 3, Profile::Full)
        .errors
        .iter()
        .map(|e| e.kind)
        .collect()
}

fn overlay_file(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("platform-overlay.toml");
    std::fs::write(&path, text).expect("запись файла дополнений");
    (dir, path)
}

#[test]
fn global_method_from_overlay_is_known_and_its_arity_is_checked() {
    let (_dir, path) = overlay_file(
        r#"
[[global_methods]]
name = "УстаревшийМетодПробы"
description = "Метод, которого нет в справке"

[[global_methods.signatures]]
parameters = ["Заголовок"]
"#,
    );

    // Без дополнения оба вызова — «метод не объявлен».
    let before = kinds(SRC_METHOD, &index());
    assert_eq!(
        before,
        [
            ExprErrorKind::UndeclaredMethod,
            ExprErrorKind::UndeclaredMethod
        ],
        "без дополнения метод неизвестен"
    );

    let overlay = platform_overlay::load(&path).expect("файл дополнений должен читаться");
    let mut index = index();
    let report = platform_overlay::apply(&mut index, &overlay);
    assert_eq!(report.methods_added, 1, "{report:?}");
    assert!(report.problems.is_empty(), "{report:?}");

    // С дополнением: вызов с одним аргументом молчит, с тремя — превышение.
    let after = kinds(SRC_METHOD, &index);
    assert_eq!(
        after,
        [ExprErrorKind::WrongArgumentCount],
        "дополнение должно включить проверку арности"
    );
}

#[test]
fn type_member_from_overlay_is_known() {
    let (_dir, path) = overlay_file(
        r#"
[[type_members]]
type_name = "Массив"
kind = "method"
name = "УстаревшийЧлен"
"#,
    );

    let before = kinds(SRC_MEMBER, &index());
    assert_eq!(
        before,
        [ExprErrorKind::UnknownTypeMember],
        "без дополнения члена нет"
    );

    let overlay = platform_overlay::load(&path).expect("файл дополнений");
    let mut index = index();
    let report = platform_overlay::apply(&mut index, &overlay);
    assert_eq!(report.members_added, 1, "{report:?}");

    let after = kinds(SRC_MEMBER, &index);
    assert!(
        after.is_empty(),
        "член из дополнения должен быть известен: {after:?}"
    );
}

#[test]
fn broken_overlay_file_is_a_loud_error() {
    // Опечатка в поле — ошибка разбора с указанием пути, а не молчание.
    let (_dir, path) = overlay_file(
        r#"
[[global_methods]]
name = "Проба"
variadic = true
"#,
    );
    let err = platform_overlay::load(&path).expect_err("лишнее поле должно быть ошибкой");
    let text = format!("{err:#}");
    assert!(text.contains("platform-overlay.toml"), "{text}");
    assert!(text.contains("variadic"), "{text}");
}

#[test]
fn missing_overlay_file_is_a_loud_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = platform_overlay::load(&dir.path().join("нет-такого.toml"))
        .expect_err("отсутствующий файл должен быть ошибкой");
    assert!(format!("{err:#}").contains("нет-такого.toml"), "{err:#}");
}
