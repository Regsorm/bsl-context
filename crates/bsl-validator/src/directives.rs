//! Whitelist имён директив BSL-компилятора и расширений конфигурации.
//!
//! Директивы в BSL пишутся как `&ИмяДирективы` перед объявлением процедуры
//! или функции. Их немного и меняются они только с релизами платформы —
//! поэтому список зашит в код, а не читается из hbk/конфига.
//!
//! Используется [`crate::module::validate_module_at_level`]: имя директивы
//! (без амперсанда) извлекается текстовым проходом, а не обходом AST —
//! грамматика `tree-sitter-bsl` заводит узел `annotation` только для
//! директив, которые сама знает, а неизвестная (опечатка вроде
//! `&НаКлентее`) попадает в `ERROR`-узел без выделенного имени. Промах по
//! whitelist → fuzzy к нему → `ExprErrorKind::UnknownDirective`.

use crate::expression::lev;

/// Плоский whitelist всех известных имён директив BSL — компиляции
/// (`НаКлиенте`, `НаСервере`, `НаСервереБезКонтекста`, `НаКлиентеНаСервере`,
/// `НаКлиентеНаСервереБезКонтекста` и их English-варианты) и расширений
/// конфигурации (`Перед`/`Before`, `После`/`After`, `Вместо`/`Around`,
/// `ИзменениеИКонтроль`/`ChangeAndValidate`).
pub const KNOWN_DIRECTIVES: &[&str] = &[
    "НаКлиенте",
    "НаСервере",
    "НаСервереБезКонтекста",
    "НаКлиентеНаСервере",
    "НаКлиентеНаСервереБезКонтекста",
    "AtClient",
    "AtServer",
    "AtServerNoContext",
    "AtClientAtServer",
    "AtClientAtServerNoContext",
    "Перед",
    "После",
    "Вместо",
    "ИзменениеИКонтроль",
    "Before",
    "After",
    "Around",
    "ChangeAndValidate",
];

/// Директивы, которые встречаются ТОЛЬКО в модуле расширения конфигурации:
/// такая процедура подключается к процедуре расширяемого модуля.
const EXTENSION_DIRECTIVES: &[&str] = &[
    "перед",
    "после",
    "вместо",
    "изменениеиконтроль",
    "before",
    "after",
    "around",
    "changeandvalidate",
];

/// `str::trim_start` не снимает UTF-8 BOM (U+FEFF не whitespace): строка
/// `\u{FEFF}&Вместо(…)` иначе не распознаётся как директива расширения, и
/// модуль расширения попадает под strict-проверку с массовыми ложными
/// ошибками. Та же функция нужна в `module::scan_directives`.
pub(crate) fn trim_start_bsl(line: &str) -> &str {
    line.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}')
}

/// Текст принадлежит модулю расширения конфигурации?
///
/// Признак — хотя бы одна директива подключения (`&Перед`, `&После`, `&Вместо`,
/// `&ИзменениеИКонтроль`). Такой модуль компилируется ВМЕСТЕ с расширяемым, и
/// его код напрямую вызывает процедуры расширяемого модуля, текста которого у
/// валидатора нет. Значит вывод «вызов не объявлен — описка» для него
/// неправомерен (замер на УТ: 2472 ложных находки на одном лишь `ДобавитьПКС`,
/// объявленном в базовом модуле на 65097-й строке).
///
/// `source` ожидается замаскированным: `&После` внутри строки или комментария
/// признаком не является.
pub fn is_extension_module(cleaned: &str) -> bool {
    cleaned.lines().any(|line| {
        let Some(rest) = trim_start_bsl(line).strip_prefix('&') else {
            return false;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_lowercase();
        EXTENSION_DIRECTIVES.contains(&name.as_str())
    })
}

/// Проверить, что `name` — известное имя директивы. Регистронезависимо.
///
/// Имя ожидается БЕЗ амперсанда (первый `identifier` узла `annotation` — уже
/// без него; см. `code-index-core::parser::bsl::extract_annotation`).
pub fn is_known_directive(name: &str) -> bool {
    let name_lc = name.to_lowercase();
    KNOWN_DIRECTIVES
        .iter()
        .any(|&d| d.to_lowercase() == name_lc)
}

/// Ближайшая по Левенштейну известная директива к `target` (регистронезависимо).
/// Возвращает одно лучшее попадание `(suggestion, distance)`; вызывающий сам
/// решает по порогу, эмиттить ли ошибку. При равном расстоянии побеждает
/// первый по порядку списка — детерминированно.
pub fn closest_directive_with_distance(target: &str) -> Option<(String, usize)> {
    let target_lc = target.to_lowercase();
    let mut best: Option<(String, usize)> = None;
    for &d in KNOWN_DIRECTIVES {
        let dist = lev(&target_lc, &d.to_lowercase());
        match &best {
            Some((_, best_d)) if dist >= *best_d => {}
            _ => best = Some((d.to_string(), dist)),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_directives_include_common_forms() {
        assert!(is_known_directive("НаСервере"));
        assert!(is_known_directive("насервере"));
        assert!(is_known_directive("AtClient"));
        assert!(is_known_directive("Перед"));
        assert!(is_known_directive("ChangeAndValidate"));
    }

    #[test]
    fn unknown_directive_typo_gives_close_suggestion() {
        // «НаКлентее» — опечатка «НаКлиенте», distance 2 (пропущена «и»,
        // добавлена лишняя «е»).
        let (suggestion, distance) = closest_directive_with_distance("НаКлентее").unwrap();
        assert_eq!(suggestion, "НаКлиенте");
        assert_eq!(distance, 2);
    }
}
