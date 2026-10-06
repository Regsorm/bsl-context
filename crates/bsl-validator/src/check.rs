//! Phase 5: точечные валидаторы по точному имени.
//!
//! - [`validate_enum`] — проверка `<ТипX>.<ЗначениеY>` против `PlatformIndex.types`.
//! - [`validate_method_call`] — проверка вызова глобального метода
//!   (число аргументов + наличие именованных параметров).
//!
//! Возвращают структуры с булевым `valid`, списком похожих значений и
//! явным человеко-читаемым сообщением об ошибке (для модели). Без парсинга
//! BSL — это уровень MCP-tool, на вход приходят уже извлечённые имена.

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use platform_index::{PlatformIndex, Signature};

/// Подсказка похожего значения. Сортируется по убыванию `score`
/// (расстояние Левенштейна, инвертированное в [0..1]).
#[derive(Debug, Clone, Serialize)]
pub struct SimilarValue {
    pub name: String,
    pub score: f32,
}

/// Краткое описание сигнатуры для возврата клиенту (без полных описаний).
#[derive(Debug, Clone, Serialize)]
pub struct SignatureBrief {
    pub name: String,
    pub min_args: usize,
    pub max_args: usize,
    /// `true` — функция принимает неограниченное число аргументов (вариативная,
    /// напр. `Макс`/`Мин`); тогда верхняя граница `max_args` не проверяется.
    pub variadic: bool,
    pub formatted: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnumValidation {
    pub valid: bool,
    pub type_name: String,
    pub value_name: String,
    /// Все легальные значения у типа (для подсказки модели). Пуст, когда тип не enum или не найден.
    pub all_valid_values: Vec<String>,
    /// Топ-5 ближайших по Левенштейну значений (только при `valid=false`).
    pub similar: Vec<SimilarValue>,
    /// `true` — тип является открытой коллекцией (`ЦветаСтиля`,
    /// `БиблиотекаКартинок`): значения добавляет конфигурация, и отсутствие
    /// в списке справки не означает ошибку.
    pub open_collection: bool,
    /// Удобочитаемый текст. Включается всегда (и при ok, и при ошибке).
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodCallValidation {
    pub valid: bool,
    pub method_name: String,
    pub arg_count: usize,
    /// Все известные сигнатуры метода. Когда метод не найден — пуст.
    pub signatures: Vec<SignatureBrief>,
    pub message: String,
}

/// Проверить вызов МЕТОДА ТИПА (не глобального) — по описанию типа из справки.
///
/// Нужно для методов контекста модуля (issue #19): неквалифицированный вызов в
/// модуле менеджера, объекта или обычной формы разрешается методом самого объекта
/// модуля, а не глобальной функцией. `None` — у типа такого метода нет (тогда
/// вызывающий код решает сам, обычно сверяясь с глобальной сигнатурой).
pub fn validate_type_method_call(
    ty: &platform_index::Type,
    method_name: &str,
    arg_count: usize,
) -> Option<MethodCallValidation> {
    let method = ty.methods.iter().find(|m| {
        m.name_ru.to_lowercase() == method_name.to_lowercase()
            || m.name_en.to_lowercase() == method_name.to_lowercase()
    })?;
    let signatures: Vec<SignatureBrief> = method
        .signatures
        .iter()
        .map(|s| brief_signature(&method.name_ru, s))
        .collect();
    if signatures.is_empty() {
        // Сигнатура в справке не описана — число аргументов не проверяем.
        return Some(MethodCallValidation {
            valid: true,
            method_name: method.name_ru.clone(),
            arg_count,
            signatures,
            message: format!(
                "⚠️ У метода '{}' нет описанных сигнатур — число аргументов не проверено.",
                method.name_ru
            ),
        });
    }
    let any_match = signatures
        .iter()
        .any(|s| arg_count >= s.min_args && (s.variadic || arg_count <= s.max_args));
    let allowed_ranges = signatures
        .iter()
        .map(|s| {
            if s.variadic {
                format!("{}+", s.min_args)
            } else if s.min_args == s.max_args {
                format!("{}", s.min_args)
            } else {
                format!("{}..{}", s.min_args, s.max_args)
            }
        })
        .collect::<Vec<_>>()
        .join(" / ");
    Some(if any_match {
        MethodCallValidation {
            valid: true,
            method_name: method.name_ru.clone(),
            arg_count,
            signatures,
            message: format!(
                "✅ Вызов '{}' с {} аргументами допустим.",
                method.name_ru, arg_count
            ),
        }
    } else {
        MethodCallValidation {
            valid: false,
            method_name: method.name_ru.clone(),
            arg_count,
            signatures,
            message: format!(
                "❌ Метод '{}.{}' не принимает {} аргументов. Допустимо: {}.",
                ty.name_ru, method.name_ru, arg_count, allowed_ranges
            ),
        }
    })
}

/// Проверить значение системного перечисления.
pub fn validate_enum(index: &PlatformIndex, type_name: &str, value_name: &str) -> EnumValidation {
    let type_name = type_name.trim();
    let value_name = value_name.trim();
    let Some(ty) = index.find_type(type_name) else {
        return EnumValidation {
            valid: false,
            type_name: type_name.to_string(),
            value_name: value_name.to_string(),
            all_valid_values: Vec::new(),
            similar: Vec::new(),
            open_collection: false,
            message: format!("❌ Тип '{type_name}' не найден в платформенном контексте."),
        };
    };
    if !ty.is_enum() {
        return EnumValidation {
            valid: false,
            type_name: ty.name_ru.clone(),
            value_name: value_name.to_string(),
            all_valid_values: Vec::new(),
            similar: Vec::new(),
            open_collection: false,
            message: format!(
                "❌ Тип '{}' не является системным перечислением.",
                ty.name_ru
            ),
        };
    }

    let value_lower = value_name.to_lowercase();
    // Значение принимается, если оно есть в справке (со сведением
    // латинско-кириллических двойников — issue #18), попадает в диапазон из
    // справки (`A...Z`, `F1...F12` у перечисления `Клавиша` — issue #22) либо
    // входит в точечный словарь значений, которые платформа принимает ради
    // совместимости.
    let valid = crate::enum_values::has_value(&ty.enum_values, value_name)
        || crate::enum_values::is_deprecated_value(&ty.name_ru, value_name);

    // Диапазоны показываем развёрнутыми: `A...Z` в списке допустимых значений
    // бесполезен, нужны сами имена.
    let all_valid_values: Vec<String> = crate::enum_values::value_names(&ty.enum_values);
    let open_collection = ty.is_open_enum();

    if valid {
        EnumValidation {
            valid: true,
            type_name: ty.name_ru.clone(),
            value_name: value_name.to_string(),
            all_valid_values,
            similar: Vec::new(),
            open_collection,
            message: format!(
                "✅ Значение '{}' допустимо для типа '{}'.",
                value_name, ty.name_ru
            ),
        }
    } else if open_collection {
        // Значение не из справки, но тип открытый: его могла добавить
        // конфигурация. Отвергнуть по справке платформы нельзя.
        EnumValidation {
            valid: true,
            type_name: ty.name_ru.clone(),
            value_name: value_name.to_string(),
            all_valid_values,
            similar: Vec::new(),
            open_collection,
            message: format!(
                "ℹ️ Значение '{}' в справке платформы не найдено, но тип '{}' — открытая коллекция: значения добавляет конфигурация. Проверьте имя по конфигурации.",
                value_name, ty.name_ru
            ),
        }
    } else {
        let similar = top_similar(&value_lower, &ty.enum_values, 5);
        let suggestion = similar
            .first()
            .map(|s| format!(" Похожее: '{}'.", s.name))
            .unwrap_or_default();
        EnumValidation {
            valid: false,
            type_name: ty.name_ru.clone(),
            value_name: value_name.to_string(),
            all_valid_values,
            similar,
            open_collection,
            message: format!(
                "❌ Значение '{}' не существует у типа '{}'.{}",
                value_name, ty.name_ru, suggestion
            ),
        }
    }
}

/// Проверить вызов глобального метода: число аргументов попадает в диапазон [min..=max]
/// хотя бы одной перегрузки. Если метод не найден — `valid=false`.
pub fn validate_method_call(
    index: &PlatformIndex,
    method_name: &str,
    arg_count: usize,
) -> MethodCallValidation {
    let method_name = method_name.trim();
    let Some(method) = index.find_global_method(method_name) else {
        return MethodCallValidation {
            valid: false,
            method_name: method_name.to_string(),
            arg_count,
            signatures: Vec::new(),
            message: format!(
                "❌ Глобальный метод '{method_name}' не найден в платформенном контексте."
            ),
        };
    };

    let signatures: Vec<SignatureBrief> = method
        .signatures
        .iter()
        .map(|s| brief_signature(&method.name_ru, s))
        .collect();
    if signatures.is_empty() {
        // Метод без описанной сигнатуры — формально не можем проверить число аргументов.
        return MethodCallValidation {
            valid: true,
            method_name: method.name_ru.clone(),
            arg_count,
            signatures,
            message: format!(
                "⚠️ У метода '{}' нет описанных сигнатур — число аргументов не проверено.",
                method.name_ru
            ),
        };
    }

    let any_match = signatures
        .iter()
        .any(|s| arg_count >= s.min_args && (s.variadic || arg_count <= s.max_args));

    if any_match {
        MethodCallValidation {
            valid: true,
            method_name: method.name_ru.clone(),
            arg_count,
            signatures,
            message: format!(
                "✅ Вызов '{}' с {} {} допустим.",
                method.name_ru,
                arg_count,
                argument_forms(arg_count).0
            ),
        }
    } else {
        let allowed_ranges = signatures
            .iter()
            .map(|s| {
                if s.variadic {
                    format!("{}+", s.min_args)
                } else if s.min_args == s.max_args {
                    format!("{}", s.min_args)
                } else {
                    format!("{}..{}", s.min_args, s.max_args)
                }
            })
            .collect::<Vec<_>>()
            .join(" / ");
        MethodCallValidation {
            valid: false,
            method_name: method.name_ru.clone(),
            arg_count,
            signatures,
            message: format!(
                "❌ Метод '{}' не принимает {} {}. Допустимо: {}.",
                method.name_ru,
                arg_count,
                argument_forms(arg_count).1,
                allowed_ranges
            ),
        }
    }
}

/// Потолок числа аргументов сигнатуры: защита от враждебного или повреждённого
/// индекса с гигантскими диапазонами имён параметров.
const MAX_SIGNATURE_ARGS: usize = 1024;

/// Формы слова «аргумент»: `(творительный, винительный)` — «с 1 аргументом»,
/// «не принимает 2 аргумента», «с 5 аргументами».
fn argument_forms(n: usize) -> (&'static str, &'static str) {
    let d = n % 10;
    let dd = n % 100;
    if d == 1 && dd != 11 {
        ("аргументом", "аргумент")
    } else if (2..=4).contains(&d) && !(12..=14).contains(&dd) {
        ("аргументами", "аргумента")
    } else {
        ("аргументами", "аргументов")
    }
}

fn brief_signature(method_name: &str, s: &Signature) -> SignatureBrief {
    // Обязательные параметры могут стоять ПОСЛЕ опциональных: минимум — позиция
    // последнего обязательного, иначе принимался вызов без него. На 8.3.27 это
    // 61 сигнатура: 17 глобальных (включая `ПоказатьЗначение`) и 44 метода типов.
    let min_args = s
        .parameters
        .iter()
        .rposition(|p| p.required)
        .map_or(0, |i| i + 1);
    let mut max_args = s.parameters.len();

    // Диапазонный параметр hbk вида `Значение1-Значение10` — один слот в
    // `parameters`, но синтаксически несколько: добавляем недостающие
    // (верх − низ), без переполнения и с потолком.
    for p in &s.parameters {
        if let Some((lower, upper)) = parse_range_bounds(&p.name) {
            max_args = max_args.saturating_add(upper.saturating_sub(lower));
        }
    }
    max_args = max_args.min(MAX_SIGNATURE_ARGS);

    // Семантически вариативные глобальные функции (`Макс`/`Мин`): hbk описывает
    // один параметр, а функция принимает неограниченное число. Признака в
    // структуре нет — список фиксирован.
    let variadic = is_variadic_global(method_name);

    let formatted = s
        .parameters
        .iter()
        .map(|p| {
            format!(
                "{}{}: {}",
                p.name,
                if p.required { "" } else { "?" },
                p.type_name
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    SignatureBrief {
        name: s.name.clone(),
        min_args,
        max_args,
        variadic,
        formatted,
    }
}

/// Глобальные функции платформы с неограниченным числом аргументов, у которых
/// hbk-сигнатура показывает лишь один параметр (вариативность — только в тексте
/// описания). Список фиксирован — таких функций единицы.
fn is_variadic_global(method_name: &str) -> bool {
    matches!(
        method_name.to_lowercase().as_str(),
        "макс" | "мин" | "max" | "min"
        // ПродолжитьВызов — спецконструкция расширений (вызов оригинала из
        // &Вместо-перехватчика): число аргументов равно сигнатуре
        // перехватываемого метода, т.е. произвольное → верх не проверяем.
        | "продолжитьвызов" | "continuecall"
    )
}

/// Границы диапазонного имени параметра `Значение1-Значение10` → `(1, 10)`.
/// Имя анкорировано целиком: подстрочные совпадения не должны завышать верх.
fn parse_range_bounds(param_name: &str) -> Option<(usize, usize)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(\D*?)(\d+)\D+?(\d+)$").unwrap());
    let caps = re.captures(param_name.trim())?;
    let lower = caps.get(2)?.as_str().parse::<usize>().ok()?;
    let upper = caps.get(3)?.as_str().parse::<usize>().ok()?;
    (upper > lower).then_some((lower, upper))
}

fn top_similar(query: &str, values: &[platform_index::EnumValue], top: usize) -> Vec<SimilarValue> {
    let mut scored: Vec<(f32, &str)> = values
        .iter()
        .flat_map(|v| {
            [v.name_ru.as_str(), v.name_en.as_str()]
                .into_iter()
                .filter(|n| !n.is_empty())
                // Опечатку справки со смешанными алфавитами (`БлокироватьВеcьИнтерфейс`)
                // в подсказку не отдаём: её повторят в коде дословно, а платформа
                // такое имя не примет. Заодно отсеиваем имя, которое после сведения
                // двойников совпадает с написанным — это то же самое имя (issue #18).
                .filter(|n| !crate::homoglyphs::is_mixed_alphabet(n))
                .filter(|n| !crate::homoglyphs::same_after_fold(n, query))
                .map(move |n| {
                    (
                        similarity_score(query, &n.to_lowercase()),
                        v.name_ru.as_str(),
                    )
                })
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    scored
        .into_iter()
        .filter(|(_, n)| seen.insert(n.to_lowercase()))
        .take(top)
        .map(|(s, n)| SimilarValue {
            name: n.to_string(),
            score: s,
        })
        .collect()
}

/// Расстояние Левенштейна, нормированное в [0..=1] (1 = полное совпадение).
fn similarity_score(a: &str, b: &str) -> f32 {
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    let dist = levenshtein(a, b) as f32;
    1.0 - (dist / max_len as f32)
}

fn levenshtein(a: &str, b: &str) -> usize {
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
            curr[j] = (curr[j - 1] + 1) // вставка
                .min(prev[j] + 1) // удаление
                .min(prev[j - 1] + cost); // замена
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_index::EnumValue;

    fn enum_v(ru: &str) -> EnumValue {
        EnumValue {
            name_ru: ru.to_string(),
            name_en: String::new(),
            description: String::new(),
        }
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("Перенос", "Переносить"), 3);
        assert_eq!(levenshtein("Авто", "Авто"), 0);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn similarity_finds_closest_value() {
        let values = vec![
            enum_v("Авто"),
            enum_v("Забивать"),
            enum_v("Обрезать"),
            enum_v("Переносить"),
        ];
        let top = top_similar("перенос", &values, 3);
        assert_eq!(top[0].name, "Переносить");
    }

    #[test]
    fn open_collection_unknown_value_is_not_rejected() {
        use platform_index::{PlatformIndex, Type};
        let mut idx = PlatformIndex::new();
        idx.insert_type(Type {
            name_ru: "КартинкиТест".into(),
            name_en: String::new(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![enum_v("<Имя картинки>"), enum_v("Лупа")],
        });
        let r = validate_enum(&idx, "КартинкиТест", "МояКартинка");
        assert!(r.valid, "открытую коллекцию по справке не отвергаем");
        assert!(r.open_collection);
        assert!(validate_enum(&idx, "КартинкиТест", "Лупа").valid);
    }

    /// Issue #22: значения-диапазоны из справки (`A...Z`, `F1...F12`) платформа
    /// принимает конкретными именами, а устаревшие значения — ради совместимости.
    #[test]
    fn enum_range_values_and_deprecated_names_are_accepted() {
        use platform_index::{PlatformIndex, Type};
        let mut idx = PlatformIndex::new();
        idx.insert_type(Type {
            name_ru: "Клавиша".into(),
            name_en: String::new(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![
                enum_v("_0..._9"),
                enum_v("A...Z"),
                enum_v("BackSpace"),
                enum_v("F1...F12"),
                enum_v("Num0...Num9"),
            ],
        });
        idx.insert_type(Type {
            name_ru: "ОтображениеОбычнойГруппы".into(),
            name_en: String::new(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![enum_v("Нет"), enum_v("СлабоеВыделение")],
        });

        for value in ["A", "F5", "F12", "Num0", "Num9", "_1", "BackSpace"] {
            assert!(
                validate_enum(&idx, "Клавиша", value).valid,
                "Клавиша.{value} платформа принимает"
            );
        }
        for value in ["F13", "Num10", "_10", "A1"] {
            assert!(
                !validate_enum(&idx, "Клавиша", value).valid,
                "Клавиша.{value} платформа не принимает"
            );
        }
        // Устаревшие имена — точечный словарь, а не общее правило.
        assert!(validate_enum(&idx, "ОтображениеОбычнойГруппы", "Линия").valid);
        assert!(!validate_enum(&idx, "ОтображениеОбычнойГруппы", "Рамка").valid);
        // Диапазоны показываются развёрнутыми.
        let r = validate_enum(&idx, "Клавиша", "A");
        assert!(r.all_valid_values.contains(&"F5".to_string()));
        assert!(!r.all_valid_values.iter().any(|v| v.contains("...")));
    }

    #[test]
    fn parse_range_upper_works() {
        assert_eq!(parse_range_bounds("Значение1-Значение10"), Some((1, 10)));
        assert_eq!(parse_range_bounds("Шаблон"), None);
        assert_eq!(parse_range_bounds("Параметр2-Параметр7"), Some((2, 7)));
    }

    /// Issue #18: в справке платформы значение записано с ЛАТИНСКОЙ буквой
    /// (`БлокироватьВеcьИнтерфейс`), платформа принимает кириллическую. Корректный
    /// код не должен получать находку, а подсказка — предлагать латиницу.
    #[test]
    fn homoglyph_in_help_name_is_not_a_finding() {
        use platform_index::{PlatformIndex, Type};
        let mut idx = PlatformIndex::new();
        idx.insert_type(Type {
            name_ru: "РежимОткрытияОкнаФормы".into(),
            name_en: String::new(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![
                enum_v("БлокироватьВеcьИнтерфейс"), // латинская `c` — как в справке
                enum_v("Независимый"),
            ],
        });

        let r = validate_enum(&idx, "РежимОткрытияОкнаФормы", "БлокироватьВесьИнтерфейс");
        assert!(
            r.valid,
            "кириллическое написание обязано приниматься: {r:?}"
        );

        // Настоящая опечатка по-прежнему ловится, и латиницу в подсказку не даём.
        let bad = validate_enum(&idx, "РежимОткрытияОкнаФормы", "БлокироватьВесьИнтерфейсX");
        assert!(!bad.valid);
        assert!(
            bad.similar.iter().all(|s| !s.name.contains('c')),
            "в подсказке не должно быть имени со смешанными алфавитами: {:?}",
            bad.similar
        );
    }

    fn method_1param(name_ru: &str, param: &str, required: bool) -> platform_index::Method {
        use platform_index::{Method, Parameter, Signature};
        Method {
            name_ru: name_ru.into(),
            name_en: String::new(),
            description: String::new(),
            return_type: String::new(),
            signatures: vec![Signature {
                name: "Основная".into(),
                syntax: String::new(),
                description: String::new(),
                parameters: vec![Parameter {
                    name: param.into(),
                    type_name: String::new(),
                    required,
                    description: String::new(),
                }],
            }],
        }
    }

    #[test]
    fn variadic_max_accepts_many_args() {
        use platform_index::PlatformIndex;
        let mut idx = PlatformIndex::new();
        idx.global_methods
            .push(method_1param("Макс", "Значение1", true));
        assert!(validate_method_call(&idx, "Макс", 1).valid);
        assert!(
            validate_method_call(&idx, "Макс", 5).valid,
            "Макс вариативна"
        );
        assert!(!validate_method_call(&idx, "Макс", 0).valid, "ниже min");
    }

    #[test]
    fn strshablon_range_param_expands_max() {
        use platform_index::{Method, Parameter, PlatformIndex, Signature};
        let mut idx = PlatformIndex::new();
        idx.global_methods.push(Method {
            name_ru: "СтрШаблон".into(),
            name_en: String::new(),
            description: String::new(),
            return_type: "Строка".into(),
            signatures: vec![Signature {
                name: "Основная".into(),
                syntax: String::new(),
                description: String::new(),
                parameters: vec![
                    Parameter {
                        name: "Шаблон".into(),
                        type_name: String::new(),
                        required: true,
                        description: String::new(),
                    },
                    Parameter {
                        name: "Значение1-Значение10".into(),
                        type_name: String::new(),
                        required: false,
                        description: String::new(),
                    },
                ],
            }],
        });
        assert!(
            validate_method_call(&idx, "СтрШаблон", 3).valid,
            "Шаблон + 2 значения"
        );
        assert!(
            validate_method_call(&idx, "СтрШаблон", 11).valid,
            "Шаблон + 10 значений"
        );
        assert!(
            !validate_method_call(&idx, "СтрШаблон", 12).valid,
            "11 значений — превышение"
        );
    }
}
