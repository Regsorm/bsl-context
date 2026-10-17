//! Интеграционные тесты issue #40: устаревшие, но живые глобальные методы.
//!
//! Справка 8.3.27 этих имён не описывает, платформа их исполняет — значит вызов
//! не должен получать ни `undeclared_method` (`confidence: high`), ни
//! `unknown_global_method` с подсказкой. Индекс синтетический: справка не нужна,
//! тест гоняется обычным `cargo test -p bsl-validator`.
//!
//! Контроли держат точность списка: опечатка известного метода, выдуманное имя и
//! имя, отличающееся от устаревшего на цифру, обязаны остаться находками.

use bsl_validator::{validate_module_with_symbols, Confidence, ExprError, ExprErrorKind, Profile};
use platform_index::{Method, PlatformIndex};

/// Индекс с единственным глобальным методом `Сообщить` — как «пустая» справка
/// без семейства заголовка приложения.
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.global_methods.push(Method {
        name_ru: "Сообщить".into(),
        name_en: "Message".into(),
        description: String::new(),
        return_type: String::new(),
        signatures: Vec::new(),
        note: None,
    });
    index
}

fn errors(src: &str) -> Vec<ExprError> {
    validate_module_with_symbols(&index(), src, 3, Profile::Full, None, None, None).errors
}

// ── Устаревшие глобальные методы молчат ──────────────────────────────────────

#[test]
fn caption_family_is_silent() {
    let src = "Процедура Проба()\n\
               \tУстановитьЗаголовокПриложения(\"X\");\n\
               \tТ1 = ПолучитьЗаголовокПриложения();\n\
               \tУстановитьКраткийЗаголовокПриложения(\"Y\");\n\
               \tТ2 = ПолучитьКраткийЗаголовокПриложения();\n\
               \tУстановитьЗаголовокКлиентскогоПриложения(\"Z\");\n\
               \tТ3 = ПолучитьЗаголовокКлиентскогоПриложения();\n\
               \tТ4 = ТекущийВариантОсновногоШрифтаКлиентскогоПриложения();\n\
               \tТ5 = ТекущийВариантИнтерфейсаКлиентскогоПриложения();\n\
               \tСообщить(Т1 + Т2 + Т3 + Т4 + Т5);\n\
               КонецПроцедуры\n";
    let found = errors(src);
    assert!(found.is_empty(), "{found:?}");
}

// ── Контроли: точность списка, а не «похожие имена молчат» ───────────────────

#[test]
fn typo_of_known_global_is_still_reported() {
    let src = "Процедура Проба()\n\tСообщитть(\"x\");\nКонецПроцедуры\n";
    let found = errors(src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ExprErrorKind::UnknownGlobalMethod);
    assert_eq!(found[0].suggestion.as_deref(), Some("Сообщить"));
}

#[test]
fn invented_global_is_still_reported() {
    let src = "Процедура Проба()\n\tПолностьюВыдуманныйМетод123();\nКонецПроцедуры\n";
    let found = errors(src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ExprErrorKind::UndeclaredMethod);
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn name_next_to_legacy_one_is_still_reported() {
    // Список точечный: имя, отличающееся от устаревшего, не покрывается.
    let src = "Процедура Проба()\n\tУстановитьЗаголовокПриложения123(\"X\");\nКонецПроцедуры\n";
    let found = errors(src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ExprErrorKind::UndeclaredMethod);
}

#[test]
fn legacy_names_do_not_shadow_arity_check() {
    // Имя, которое ЕСТЬ в индексе, проверяется как раньше — список устаревших
    // ветку известных методов не перехватывает.
    let src = "Процедура Проба()\n\tСообщить(\"a\", \"b\", \"c\", \"d\");\nКонецПроцедуры\n";
    let found = errors(src);
    assert!(
        found.is_empty(),
        "у синтетического `Сообщить` нет сигнатур — арность не проверяется: {found:?}"
    );
}
