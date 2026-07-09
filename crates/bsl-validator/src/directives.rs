//! Whitelist имён директив BSL-компилятора и расширений конфигурации.
//!
//! Директивы в BSL пишутся как `&ИмяДирективы` перед объявлением процедуры
//! или функции. Их немного и меняются они только с релизами платформы —
//! поэтому список зашит в код, а не читается из hbk/конфига.
//!
//! Используется [`crate::module::validate_module_at_level`]: при обходе
//! AST tree-sitter-onescript узлы `annotation` содержат `identifier` с именем
//! директивы (без амперсанда). Промах по whitelist → fuzzy к нему →
//! `ExprErrorKind::UnknownDirective`.

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
/// Возвращает `(suggestion, distance)` для всех попаданий; вызывающий сам
/// решает по порогу, эмиттить ли ошибку.
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

/// Локальная копия расстояния Левенштейна, чтобы не тянуть зависимость
/// от приватного `expression::lev`. Идентичная реализация.
fn lev(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let (n, m) = (av.len(), bv.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if av[i - 1] == bv[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
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
