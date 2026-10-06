//! Значения перечислений: разворот диапазонов из справки и словарь
//! совместимости (issue #22).
//!
//! Справка платформы хранит часть значений перечисления **диапазоном**, а не
//! конкретными именами. В 8.3.27 это ровно четыре значения, и все — в
//! перечислении `Клавиша`: `_0..._9`, `A...Z`, `F1...F12`, `Num0...Num9`.
//! Платформа принимает конкретные значения (`Клавиша.A`, `Клавиша.F5`,
//! `Клавиша.Num0`), поэтому без разворота они получали ложную находку
//! `unknown_enum_value` с `confidence: high` — автор issue насчитал 258 таких
//! находок на шести конфигурациях (типовой код назначения горячих клавиш).
//!
//! Вторая часть того же issue — значения, которых в справке нет, но платформа
//! их принимает ради совместимости. Их список держится точечно, см.
//! [`DEPRECATED_VALUES`].

use platform_index::EnumValue;

use crate::homoglyphs::same_after_fold;

/// Потолок разворота одного диапазона. Справка использует короткие диапазоны
/// (`A...Z` — 26, `F1...F12` — 12); ограничение защищает от чужеродного
/// `1...1000000` в самодельной справке.
const MAX_RANGE_ITEMS: usize = 1024;

/// Развернуть значение-диапазон в конкретные имена.
///
/// Поддерживаются две формы, обе встречаются в справке:
/// буквенная (`A...Z`) и «префикс + число» (`F1...F12`, `Num0...Num9`,
/// `_0..._9`). Всё остальное — `None`: формат не распознан, и угадывать по нему
/// мы не будем (нам важно не выдать ложную находку, а не развернуть любой текст).
pub fn expand_range(value: &str) -> Option<Vec<String>> {
    let (start, end) = value.split_once("...")?;
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() || end.is_empty() {
        return None;
    }

    // Буквенная форма: обе границы — одна ASCII-буква (`A...Z`).
    let single = |s: &str| -> Option<char> {
        let mut chars = s.chars();
        let c = chars.next()?;
        if chars.next().is_some() || !c.is_ascii_alphabetic() {
            return None;
        }
        Some(c.to_ascii_uppercase())
    };
    if let (Some(a), Some(b)) = (single(start), single(end)) {
        let (a, b) = (a as u32, b as u32);
        if a <= b && (b - a) as usize <= MAX_RANGE_ITEMS {
            return Some(
                (a..=b)
                    .filter_map(char::from_u32)
                    .map(|c| c.to_string())
                    .collect(),
            );
        }
        return None;
    }

    // Форма «префикс + число»: `F1...F12`, `Num0...Num9`, `_0..._9`.
    let split = |s: &str| -> Option<(String, u32)> {
        let idx = s.find(|c: char| c.is_ascii_digit())?;
        let (prefix, num) = s.split_at(idx);
        let num: u32 = num.parse().ok()?;
        Some((prefix.to_string(), num))
    };
    let (prefix_a, num_a) = split(start)?;
    let (prefix_b, num_b) = split(end)?;
    if prefix_a != prefix_b || num_a > num_b || (num_b - num_a) as usize > MAX_RANGE_ITEMS {
        return None;
    }
    Some((num_a..=num_b).map(|n| format!("{prefix_a}{n}")).collect())
}

/// Все имена значений перечисления, включая развёрнутые диапазоны.
///
/// Используется там, где список показывается человеку (допустимые значения,
/// подсказки): увидеть `A...Z` в ответе инструмента бесполезно, нужны `A`, `B`, …
pub fn value_names(values: &[EnumValue]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        for name in expand_range(&v.name_ru).unwrap_or_else(|| vec![v.name_ru.clone()]) {
            if !out.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                out.push(name);
            }
        }
    }
    out
}

/// Есть ли такое значение у перечисления.
///
/// Совпадение — точное (со сведением латинско-кириллических двойников, issue #18)
/// либо попадание в диапазон из справки (issue #22). Оба правила нужны: без
/// первого корректный код получает `high` на опечатке справки, без второго —
/// на каждом `Клавиша.A`.
pub fn has_value(values: &[EnumValue], member: &str) -> bool {
    for v in values {
        if same_after_fold(&v.name_ru, member) || same_after_fold(&v.name_en, member) {
            return true;
        }
        if let Some(expanded) = expand_range(&v.name_ru) {
            if expanded.iter().any(|n| n.eq_ignore_ascii_case(member)) {
                return true;
            }
        }
    }
    false
}

/// Значения, которые платформа принимает ради совместимости, а справка уже не
/// перечисляет (issue #22).
///
/// Список **точечный** и намеренно НЕ обобщается правилом «нет в справке — значит
/// устаревшее»: часть таких имён совпадает с именами ТИПОВ платформы (`Линия` —
/// тип `Line`, `РамкаГруппы` — `GroupBox`), и общее правило замаскировало бы
/// настоящую ошибку в имени значения.
const DEPRECATED_VALUES: &[(&str, &str)] = &[
    // Проверено автором issue на живой платформе (8.3.17.1549 и 8.3.27.2214):
    // `Строка(ОтображениеОбычнойГруппы.Линия)` → «Слабое выделение»,
    // `…РамкаГруппы` → «Сильное выделение». В справке 8.3.25–8.3.27 этих имён
    // нет, и ни в одном из 670 перечислений они не встречаются.
    ("ОтображениеОбычнойГруппы", "Линия"),
    ("ОтображениеОбычнойГруппы", "РамкаГруппы"),
    // `GCM` есть в справке 8.3.25 и 8.3.27, но отсутствует в более старых —
    // в списке ради версий платформы, где его нет (типовой модуль мобильного
    // приложения).
    ("ТипПодписчикаДоставляемыхУведомлений", "GCM"),
];

/// Значение отсутствует в справке, но платформа его принимает?
pub fn is_deprecated_value(type_name: &str, member: &str) -> bool {
    DEPRECATED_VALUES
        .iter()
        .any(|(ty, value)| same_after_fold(ty, type_name) && same_after_fold(value, member))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(name_ru: &str) -> EnumValue {
        EnumValue {
            name_ru: name_ru.to_string(),
            name_en: String::new(),
            description: String::new(),
            note: None,
        }
    }

    #[test]
    fn expands_all_four_shapes_from_help() {
        assert_eq!(expand_range("_0..._9").unwrap().len(), 10);
        assert_eq!(expand_range("A...Z").unwrap().len(), 26);
        assert_eq!(expand_range("F1...F12").unwrap().len(), 12);
        assert_eq!(expand_range("Num0...Num9").unwrap().len(), 10);
        assert_eq!(expand_range("A...Z").unwrap()[0], "A");
        assert_eq!(expand_range("F1...F12").unwrap()[11], "F12");
        assert_eq!(expand_range("Num0...Num9").unwrap()[0], "Num0");
        assert_eq!(expand_range("_0..._9").unwrap()[0], "_0");
    }

    #[test]
    fn rejects_unrecognized_shapes() {
        assert!(expand_range("BackSpace").is_none());
        assert!(expand_range("A...").is_none());
        assert!(expand_range("...").is_none());
        assert!(expand_range("A...1").is_none());
        assert!(expand_range("F1...G12").is_none(), "разные префиксы");
        assert!(expand_range("F12...F1").is_none(), "обратный порядок");
        assert!(expand_range("1...1000000").is_none(), "слишком длинный");
    }

    #[test]
    fn key_values_are_accepted() {
        // Перечисление `Клавиша` из справки: четыре диапазона и конкретные имена.
        let values = vec![
            value("_0..._9"),
            value("A...Z"),
            value("BackSpace"),
            value("F1...F12"),
            value("Num0...Num9"),
        ];
        for member in [
            "A",
            "F1",
            "F5",
            "Num0",
            "Num9",
            "_1",
            "BackSpace",
            "backspace",
        ] {
            assert!(has_value(&values, member), "{member} должно приниматься");
        }
        for member in ["A1", "F13", "Num10", "Клавиша", "_10", "Zz"] {
            assert!(
                !has_value(&values, member),
                "{member} не должно приниматься"
            );
        }
    }

    #[test]
    fn value_names_expands_ranges() {
        let values = vec![value("A...Z"), value("BackSpace")];
        let names = value_names(&values);
        assert_eq!(names.len(), 27);
        assert!(names.contains(&"A".to_string()));
        assert!(names.contains(&"BackSpace".to_string()));
    }

    #[test]
    fn deprecated_values_are_accepted_and_narrow() {
        assert!(is_deprecated_value("ОтображениеОбычнойГруппы", "Линия"));
        assert!(is_deprecated_value(
            "ОтображениеОбычнойГруппы",
            "РамкаГруппы"
        ));
        assert!(is_deprecated_value(
            "ТипПодписчикаДоставляемыхУведомлений",
            "GCM"
        ));
        // Правило не распространяется на другие типы и на чужие имена.
        assert!(!is_deprecated_value("ОтображениеОбычнойГруппы", "Нет"));
        assert!(!is_deprecated_value("Клавиша", "Линия"));
    }
}
