//! Integration-тесты Phase 9 (`validate_module`) на реальном `shcntx_ru.hbk`.
//!
//! Проверяет:
//! - извлечение whitelist пользовательских процедур/функций через tree-sitter
//!   (свой вызов не считается опечаткой платформенного метода);
//! - fuzzy-эмиссию `UnknownGlobalMethod` для опечатки платформенного метода
//!   в модуле, где также объявлена своя процедура;
//! - валидацию имён директив: `&НаСервере` молчит, `&НаКлентее` даёт
//!   `UnknownDirective` с suggestion `НаКлиенте`;
//! - обработку override-директив `&Перед("Foo")` без ошибок независимо от Foo.

use std::path::PathBuf;

use bsl_validator::{validate_module, Confidence, ExprErrorKind};
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

#[test]
fn user_procedure_call_is_silent() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    let src = "\
Процедура МояПроцедура() Экспорт
    // Тело
КонецПроцедуры

Процедура Точка()
    МояПроцедура();
КонецПроцедуры
";
    let result = validate_module(&index, src);
    println!("{result:#?}");
    assert!(
        result.valid,
        "вызов своей процедуры не должен давать ошибок: {:#?}",
        result.errors
    );
}

#[test]
fn platform_typo_in_module_flagged() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    // В модуле есть своя процедура «МояПроцедура» и опечатка «СтрНайит»
    // (правильно — СтрНайти). Валидатор должен пропустить «МояПроцедура»
    // (по whitelist) и эмиттить UnknownGlobalMethod для «СтрНайит».
    let src = "\
Процедура МояПроцедура() Экспорт
КонецПроцедуры

Процедура Точка()
    МояПроцедура();
    Поз = СтрНайит(\"abc\", \"b\");
КонецПроцедуры
";
    let result = validate_module(&index, src);
    println!("{result:#?}");
    assert!(!result.valid, "ожидается valid=false");
    let err = result
        .errors
        .iter()
        .find(|e| e.kind == ExprErrorKind::UnknownGlobalMethod)
        .expect("должна быть ошибка UnknownGlobalMethod");
    assert_eq!(err.suggestion.as_deref(), Some("СтрНайти"));
    assert_eq!(err.confidence, Confidence::High);
    // На «МояПроцедура» ошибки НЕТ.
    assert!(
        !result.errors.iter().any(|e| e
            .message
            .to_lowercase()
            .contains("мояпроцедура")),
        "своя процедура не должна попадать в ошибки: {:#?}",
        result.errors
    );
}

#[test]
fn known_directive_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    let src = "\
&НаСервере
Процедура ОбработатьНаСервере()
КонецПроцедуры

&НаКлиенте
Процедура ОбработатьНаКлиенте()
КонецПроцедуры
";
    let result = validate_module(&index, src);
    println!("{result:#?}");
    assert!(
        result.valid,
        "известные директивы не должны давать ошибок: {:#?}",
        result.errors
    );
}

#[test]
fn directive_typo_flagged() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    // «НаКлентее» — опечатка «НаКлиенте»: distance 2 при len 9 → strong → High.
    let src = "\
&НаКлентее
Процедура ОпечаткаВДирективе()
КонецПроцедуры
";
    let result = validate_module(&index, src);
    println!("{result:#?}");
    let err = result
        .errors
        .iter()
        .find(|e| e.kind == ExprErrorKind::UnknownDirective)
        .expect("должна быть ошибка UnknownDirective");
    assert_eq!(err.suggestion.as_deref(), Some("НаКлиенте"));
    assert_eq!(err.confidence, Confidence::High);
}

/// Аннотация `// @type` относится к ПЕРВОМУ присваиванию после неё и не
/// перебивает честный вывод по `Новый ТипX` у следующих. До правки она
/// цеплялась ко всем присваиваниям в окне 200 байт, и `МойЗапрос` получал тип
/// «Массив» → ложная находка «у типа Массив нет члена Выполнить».
#[test]
fn type_annotation_binds_only_to_next_assignment() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    let src = "\
Функция ПолучитьДанные() Экспорт
\t// @type Массив
\tСписок = ПолучитьСписок();
\tМойЗапрос = Новый Запрос;
\tВозврат МойЗапрос.Выполнить();
КонецФункции
";
    let result = bsl_validator::validate_module_at_level(&index, src, 3);
    println!("{result:#?}");
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.message.contains("Массив") && e.message.contains("Выполнить")),
        "аннотация не должна распространяться на следующее присваивание: {:#?}",
        result.errors
    );
}

/// Выбор аннотации детерминирован: из двух подходящих берётся ближайшая, а не
/// произвольная по обходу `HashMap`. Один и тот же текст обязан давать один и
/// тот же результат от прогона к прогону.
#[test]
fn nearest_type_annotation_wins_deterministically() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    // Второй тип — с фиксированным составом членов: у `Структура` свойства
    // произвольные, и проверка членов по ней намеренно молчит.
    let src = "\
Процедура Тест()
\t// @type Структура
\tА = Получить();
\t// @type Массив
\tБ = Получить();
\tБ.НетТакогоЧлена();
КонецПроцедуры
";
    let first = validate_module_at_level_messages(&index, src);
    for _ in 0..5 {
        assert_eq!(
            first,
            validate_module_at_level_messages(&index, src),
            "набор находок обязан быть воспроизводимым"
        );
    }
    assert!(
        first.iter().any(|m| m.contains("Массив")),
        "для Б должна выбираться ближайшая аннотация (Массив): {first:?}"
    );
}

fn validate_module_at_level_messages(
    index: &platform_index::PlatformIndex,
    src: &str,
) -> Vec<String> {
    bsl_validator::validate_module_at_level(index, src, 3)
        .errors
        .into_iter()
        .map(|e| e.message)
        .collect()
}

/// Ключевое слово отделяется от имени любым пробельным символом: `Функция\t\tИмя()`
/// — законное объявление, и проверки по объявлениям обязаны его видеть.
#[test]
fn declaration_with_tab_after_keyword_is_seen() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    let src = "Функция\t\tДубль()\nКонецФункции\nФункция\tДубль()\nКонецФункции\n";
    let result = validate_module(&index, src);
    println!("{result:#?}");
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::DuplicateDeclaration),
        "дубль объявления с табуляцией обязан находиться: {:#?}",
        result.errors
    );
}

#[test]
fn override_directive_ok() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");

    // &Перед("...") — известная директива расширения. Target-имя внутри кавычек
    // сейчас НЕ валидируется (нужен доступ к другим модулям конфигурации).
    let src = "\
&Перед(\"ОригинальнаяПроцедура\")
Процедура Ext_ОригинальнаяПроцедура()
КонецПроцедуры
";
    let result = validate_module(&index, src);
    println!("{result:#?}");
    assert!(
        result.valid,
        "&Перед(\"Foo\") не должна давать ошибок независимо от Foo: {:#?}",
        result.errors
    );
}
