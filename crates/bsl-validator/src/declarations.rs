//! Проверки объявлений процедур/функций и структуры модуля.
//!
//! Три находки, закрывающие самый частый класс отказов, который до этого не
//! ловил никто: модуль не компилируется вовсе, и платформа сообщает об этом
//! только при открытии обработки в базе.
//!
//! # Почему разбор текстовый, а не по дереву
//!
//! Ровно эти модули и НЕ разбираются: у `tree-sitter-bsl` на них `ERROR`, и
//! объявления вокруг теряются (см. `collect_facts` — там объявления собираются
//! из двух источников именно поэтому). Строить проверку сломанного модуля на
//! дереве, которое ломается вместе с ним, нельзя. Текстовый проход по
//! замаскированному тексту устойчив: строки и комментарии уже вычищены, длина
//! сохранена байт-в-байт, поэтому смещения совпадают с оригиналом.
//!
//! Брать сырые `ERROR`-узлы тоже нельзя: у грамматики 0.1.7 шесть собственных
//! дефектов (перечислены в `bsl_parse::normalize_for_parser`), и такое правило
//! сообщало бы в основном о них.
//!
//! # Что ловится
//!
//! 1. [`ExprErrorKind::ReservedProcedureName`] — имя процедуры совпало с
//!    ключевым словом языка (`Выполнить`) либо входит в наблюдательный список
//!    [`PLATFORM_REJECTED`] (`Найти`). Платформа отвечает «Ожидается имя
//!    процедуры» и модуль не компилируется. Замер на выгрузке УТ: среди 14905
//!    модулей таких имён нет ни одного — ложных срабатываний правило не даёт.
//! 2. [`ExprErrorKind::DuplicateDeclaration`] — имя объявлено дважды.
//!    «Процедура или функция с указанным именем уже определена».
//! 3. [`ExprErrorKind::UnbalancedModuleBlock`] — лишний или недостающий
//!    `КонецПроцедуры`/`КонецФункции`. Платформа отвечает «Обнаружено
//!    логическое завершение исходного текста модуля»: всё, что идёт после
//!    лишнего конца блока, в модуль просто не попадает.

use std::collections::HashMap;

use crate::expression::{pos_at, Confidence, ExprError, ExprErrorKind};

/// Ключевые слова языка. Имя из этого списка платформа не примет как имя
/// процедуры ни при каких условиях — это не эвристика, а грамматика языка.
/// Английские формы включены: платформа принимает оба написания.
const KEYWORDS: &[&str] = &[
    "если",
    "тогда",
    "иначе",
    "иначеесли",
    "конецесли",
    "для",
    "каждого",
    "из",
    "по",
    "цикл",
    "конеццикла",
    "пока",
    "прервать",
    "продолжить",
    "процедура",
    "функция",
    "конецпроцедуры",
    "конецфункции",
    "перем",
    "возврат",
    "попытка",
    "исключение",
    "конецпопытки",
    "вызватьисключение",
    "экспорт",
    "знач",
    "новый",
    "выполнить",
    "перейти",
    "и",
    "или",
    "не",
    "истина",
    "ложь",
    "неопределено",
    "null",
    "if",
    "then",
    "else",
    "elsif",
    "endif",
    "for",
    "each",
    "in",
    "to",
    "do",
    "enddo",
    "while",
    "break",
    "continue",
    "procedure",
    "function",
    "endprocedure",
    "endfunction",
    "var",
    "return",
    "try",
    "except",
    "endtry",
    "raise",
    "export",
    "val",
    "new",
    "execute",
    "goto",
    "and",
    "or",
    "not",
    "true",
    "false",
    "undefined",
];

/// Имена, которые платформа отвергает как имя процедуры, хотя ключевыми словами
/// они не являются. Список **наблюдательный**: сюда попадает только то, на чём
/// реально получен отказ компилятора, — догадки здесь недопустимы.
///
/// Признак «имя совпало с глобальной функцией платформы» для этого НЕ годится и
/// был отвергнут по контрпримерам: `Функция СтрЗаканчиваетсяНа(...)` и
/// `Функция ПроверитьЗаполнение()` платформа принимает (в обоих случаях она
/// сообщила «уже определена», то есть объявление состоялось), хотя обе —
/// платформенные имена. Что именно отличает `Найти`, по имеющимся наблюдениям
/// установить не удалось: BSL Language Server считает это имя обычным
/// идентификатором, а платформа отказала трижды — 24.04, 17.06 и 03.08.2026,
/// каждый раз в модуле управляемой формы, на обработчике команды.
///
/// Пополнять список — только по факту нового отказа платформы, с датой.
const PLATFORM_REJECTED: &[&str] = &[
    // «Ожидается имя процедуры: Процедура <<?>>Найти(Команда)»
    "найти",
];

/// Лексема, важная для структуры модуля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// Заголовок `Процедура`/`Функция`.
    Header,
    /// `КонецПроцедуры`/`КонецФункции`.
    End,
}

/// Символ, из которых состоит идентификатор BSL.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Найти в замаскированном тексте все заголовки и концы блоков — как ОТДЕЛЬНЫЕ
/// слова, в любом месте строки.
///
/// Построчного разбора («ключевое слово в начале строки») недостаточно:
/// защищённые модули поставщиков ужаты так, что конец одной функции и заголовок
/// следующей стоят на одной строке — `конецфункции  функция Гео(...)`. На
/// выгрузке УТ это давало 12 ложных находок `UnbalancedModuleBlock` в двух
/// обфусцированных модулях.
///
/// `lower` — результат `to_lowercase()` от `cleaned`; смещения совпадают,
/// потому что смена регистра не меняет длину ни кириллических, ни латинских
/// букв в UTF-8. Прочих букв в ключевых словах BSL нет.
fn scan_structure_tokens(cleaned: &str) -> Vec<(usize, Token)> {
    const WORDS: &[(&str, Token)] = &[
        ("конецпроцедуры", Token::End),
        ("конецфункции", Token::End),
        ("endprocedure", Token::End),
        ("endfunction", Token::End),
        ("процедура", Token::Header),
        ("функция", Token::Header),
        ("procedure", Token::Header),
        ("function", Token::Header),
    ];

    let lower = cleaned.to_lowercase();
    debug_assert_eq!(lower.len(), cleaned.len(), "смена регистра изменила длину");

    let mut out = Vec::new();
    let mut i = 0usize;
    // Конец предыдущей принятой лексемы: `КонецФункции  Функция Б()` в одной
    // строке — второе слово начинает оператор, хотя слева от него буква.
    let mut prev_end = 0usize;

    while i < lower.len() {
        if !lower.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &lower[i..];
        let Some((word, token)) = WORDS.iter().find(|(w, _)| rest.starts_with(w)) else {
            i += 1;
            continue;
        };

        let after = i + word.len();
        // Справа — граница слова: `ФункцияРасчёта` заголовком не является.
        let right_ok = after >= lower.len()
            || !lower[after..].chars().next().is_some_and(is_word_char);

        if right_ok && starts_statement(&lower, i, prev_end) {
            out.push((i, *token));
            prev_end = after;
            i = after;
            continue;
        }
        i += 1;
    }

    out
}

/// Начинает ли слово в позиции `at` новый оператор?
///
/// Иначе `Обработчик.Процедура = "..."` (свойство обработчика обновления БСП)
/// читается как заголовок процедуры: на выгрузке УТ это дало 1317 ложных
/// находок. Оператор начинается после начала текста, перевода строки, `;` —
/// либо сразу за предыдущей принятой лексемой, если между ними одни пробелы.
fn starts_statement(lower: &str, at: usize, prev_end: usize) -> bool {
    // BOM в начале файла — не значащий символ, но и не пробел по Unicode.
    let raw_left = &lower[..at];
    let mut left = raw_left.trim_end_matches(is_indent_char);

    // `Асинх Процедура …`: модификатор оператор не заканчивает, снимаем его.
    // Пробел между ним и ключевым словом обязателен, иначе `АсинхПроцедура`
    // (обычное имя переменной) читалось бы как заголовок.
    if left.len() < raw_left.len() {
        for kw in ["асинх", "async"] {
            if let Some(head) = left.strip_suffix(kw) {
                let boundary_ok = head.chars().next_back().is_none_or(|c| !is_word_char(c));
                if boundary_ok {
                    left = head.trim_end_matches(is_indent_char);
                    break;
                }
            }
        }
    }

    if left.is_empty() {
        return true;
    }
    // Между нами и предыдущей принятой лексемой — только пробелы.
    if left.len() == prev_end && prev_end > 0 {
        return true;
    }
    matches!(left.chars().next_back(), Some('\n' | '\r' | ';'))
}

/// Пробельный символ отступа: всё пробельное, кроме перевода строки (он сам —
/// признак начала оператора), плюс метка порядка байт.
///
/// Обычным пробелом набор не ограничен: в модулях 1С встречается неразрывный
/// пробел (U+00A0) — им набран отступ перед `КонецПроцедуры` в типовой форме
/// «Подтверждение зачисления зарплаты». Пока он не считался отступом, конец
/// блока не распознавался.
fn is_indent_char(c: char) -> bool {
    (c.is_whitespace() && c != '\n' && c != '\r') || c == '\u{FEFF}'
}

/// Обрезать ведущие пробелы вместе с меткой порядка байт.
///
/// Выгрузки 1С пишутся в UTF-8 с BOM, а `str::trim_start` его не снимает:
/// U+FEFF по Unicode не пробел. Из-за этого первый заголовок модуля не
/// распознавался, и его `КонецПроцедуры` считался лишним — на выгрузке УТ это
/// давало 197 ложных находок `UnbalancedModuleBlock`.
fn trim_start_bsl(line: &str) -> &str {
    line.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}')
}

#[cfg(test)]
mod indent_tests {
    use super::*;

    #[test]
    fn nbsp_is_indent_but_newline_is_not() {
        assert!(is_indent_char('\u{00A0}'), "неразрывный пробел — отступ");
        assert!(is_indent_char('\u{FEFF}'), "BOM — не значащий символ");
        assert!(is_indent_char(' ') && is_indent_char('\t'));
        assert!(!is_indent_char('\n') && !is_indent_char('\r'));
    }
}

/// Снять необязательный модификатор `Асинх` перед `Процедура`/`Функция`.
///
/// Асинхронные методы (платформа 8.3.2x) объявляются как
/// `Асинх Процедура Имя(...)`. Без этого заголовок не распознаётся, а его
/// `КонецПроцедуры` считается лишним — на выгрузке УТ это давало 853 ложных
/// находки `UnbalancedModuleBlock` при семи зелёных модульных тестах.
///
/// Возвращает длину снятого префикса в БАЙТАХ (0, если модификатора нет).
/// `lower` обязан быть результатом `to_lowercase()` от того же среза: длины
/// у кириллицы совпадают, потому что регистр не меняет длину букв в UTF-8.
fn async_prefix_len(lower: &str) -> usize {
    for kw in ["асинх", "async"] {
        let Some(rest) = lower.strip_prefix(kw) else {
            continue;
        };
        // За модификатором обязателен пробел: `АсинхПроцедура` — одно имя.
        let trimmed = rest.trim_start();
        if trimmed.len() < rest.len() {
            return lower.len() - trimmed.len();
        }
    }
    0
}

/// Объявление процедуры/функции, найденное текстовым проходом.
pub(crate) struct DeclFact {
    /// Имя как в исходном тексте — им же и сообщаем автору.
    pub name: String,
    pub name_lower: String,
    /// Начало ИМЕНИ в байтах исходного текста (для `pos_at`).
    pub byte: usize,
}

/// Собрать объявления процедур и функций с позициями.
///
/// `cleaned` — замаскированный текст (строки и комментарии заменены пробелами
/// байт-в-байт), поэтому смещения годятся для исходного текста напрямую.
/// Заголовок с переносом перед скобкой (`Процедура Имя\n(А)`) не поддержан
/// намеренно: в реальном коде он не встречается, а поиск имени «до первой
/// скобки где-то дальше» ловил бы соседние строки.
pub(crate) fn scan_declarations_with_pos(cleaned: &str) -> Vec<DeclFact> {
    let mut out = Vec::new();
    let mut line_start = 0usize;

    for line in cleaned.split_inclusive('\n') {
        let full = trim_start_bsl(line);
        let indent = line.len() - full.len();
        let full_lower = full.to_lowercase();
        // `Асинх Процедура Имя(...)` — модификатор снимается, дальше как обычно.
        let async_len = async_prefix_len(&full_lower);
        let trimmed = &full[async_len..];
        let lower = &full_lower[async_len..];

        let kw_len = ["процедура ", "функция ", "procedure ", "function "]
            .iter()
            .find(|kw| lower.starts_with(**kw))
            .map(|kw| kw.len());

        if let Some(kw_len) = kw_len {
            let rest = &trimmed[kw_len..];
            // Имя — до открывающей скобки; пробелы между именем и скобкой
            // платформа допускает.
            if let Some((raw_name, _)) = rest.split_once('(') {
                let name = raw_name.trim();
                if !name.is_empty() && !name.contains(char::is_whitespace) {
                    // Смещение имени = начало строки + отступ + слово + пробелы
                    // между словом и именем.
                    let lead = raw_name.len() - raw_name.trim_start().len();
                    out.push(DeclFact {
                        name: name.to_string(),
                        name_lower: name.to_lowercase(),
                        byte: line_start + indent + async_len + kw_len + lead,
                    });
                }
            }
        }

        line_start += line.len();
    }

    out
}

/// Находки по объявлениям: занятое имя и повторное объявление.
pub(crate) fn check_declarations(source: &str, cleaned: &str, errors: &mut Vec<ExprError>) {
    let decls = scan_declarations_with_pos(cleaned);
    let mut seen: HashMap<&str, usize> = HashMap::new();

    for decl in &decls {
        let (line, col) = pos_at(source, decl.byte);

        if KEYWORDS.contains(&decl.name_lower.as_str()) {
            errors.push(ExprError::new_with_confidence(
                line,
                col,
                ExprErrorKind::ReservedProcedureName,
                format!(
                    "'{}' — ключевое слово языка, платформа не примет его как имя процедуры \
                     («Ожидается имя процедуры»). Переименуйте, например '{}Команда'.",
                    decl.name, decl.name
                ),
                Confidence::High,
                None,
                Vec::new(),
            ));
        } else if PLATFORM_REJECTED.contains(&decl.name_lower.as_str()) {
            errors.push(ExprError::new_with_confidence(
                line,
                col,
                ExprErrorKind::ReservedProcedureName,
                format!(
                    "'{}' платформа не принимает как имя процедуры («Ожидается имя процедуры») — \
                     проверено на реальных обработках. Переименуйте, например '{}Команда'.",
                    decl.name, decl.name
                ),
                Confidence::High,
                None,
                Vec::new(),
            ));
        }

        match seen.get(decl.name_lower.as_str()) {
            Some(&first_line) => errors.push(ExprError::new_with_confidence(
                line,
                col,
                ExprErrorKind::DuplicateDeclaration,
                format!(
                    "Процедура или функция '{}' уже объявлена в этом модуле (строка {}). \
                     Платформа откажется компилировать модуль.",
                    decl.name, first_line
                ),
                Confidence::High,
                None,
                Vec::new(),
            )),
            None => {
                seen.insert(decl.name_lower.as_str(), line as usize);
            }
        }
    }
}

/// Находка по структуре модуля: лишний или недостающий конец блока.
///
/// Считаем только заголовки процедур и функций: вложенные `Если`/`Цикл` дают
/// свои сообщения платформы, а их баланс на сломанных модулях зашумлён
/// препроцессором (`#Если` конфигурации). Здесь важен именно тот случай,
/// который встречался в работе: лишний `КонецФункции` обрывает модуль, и всё
/// написанное ниже платформа просто не видит.
pub(crate) fn check_module_structure(source: &str, cleaned: &str, errors: &mut Vec<ExprError>) {
    let mut depth: i32 = 0;
    let mut open_byte: Option<usize> = None;

    for (byte, token) in scan_structure_tokens(cleaned) {
        match token {
            Token::Header => {
                if depth > 0 {
                    // Заголовок внутри незакрытого блока — предыдущий не закрыт.
                    let (l, c) = pos_at(source, open_byte.unwrap_or(byte));
                    errors.push(ExprError::new_with_confidence(
                        l,
                        c,
                        ExprErrorKind::UnbalancedModuleBlock,
                        "Блок процедуры/функции не закрыт: следующий заголовок начинается раньше, \
                         чем встретился «КонецПроцедуры»/«КонецФункции»."
                            .to_string(),
                        Confidence::High,
                        None,
                        Vec::new(),
                    ));
                } else {
                    open_byte = Some(byte);
                    depth += 1;
                }
            }
            Token::End => {
                depth -= 1;
                if depth < 0 {
                    let (l, c) = pos_at(source, byte);
                    errors.push(ExprError::new_with_confidence(
                        l,
                        c,
                        ExprErrorKind::UnbalancedModuleBlock,
                        "Лишний «КонецПроцедуры»/«КонецФункции»: соответствующего заголовка нет. \
                         Платформа обрывает модуль на этом месте («Обнаружено логическое \
                         завершение исходного текста модуля»), всё написанное ниже теряется."
                            .to_string(),
                        Confidence::High,
                        None,
                        Vec::new(),
                    ));
                    depth = 0;
                }
                open_byte = None;
            }
        }
    }

    if depth > 0 {
        let (l, c) = pos_at(source, open_byte.unwrap_or(0));
        errors.push(ExprError::new_with_confidence(
            l,
            c,
            ExprErrorKind::UnbalancedModuleBlock,
            "Блок процедуры/функции не закрыт до конца модуля — не хватает \
             «КонецПроцедуры»/«КонецФункции»."
                .to_string(),
            Confidence::High,
            None,
            Vec::new(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::mask_strings_and_comments;

    fn run(src: &str) -> Vec<ExprError> {
        let cleaned = mask_strings_and_comments(src);
        let mut errors = Vec::new();
        check_declarations(src, &cleaned, &mut errors);
        check_module_structure(src, &cleaned, &mut errors);
        errors
    }

    fn kinds(errors: &[ExprError]) -> Vec<ExprErrorKind> {
        errors.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn keyword_as_procedure_name_is_caught() {
        let errors = run("&НаКлиенте\nПроцедура Выполнить(Команда)\nКонецПроцедуры");
        assert!(kinds(&errors).contains(&ExprErrorKind::ReservedProcedureName));
    }

    #[test]
    fn ordinary_name_is_silent() {
        let errors = run("&НаКлиенте\nПроцедура ВыполнитьКоманду(Команда)\nКонецПроцедуры");
        assert!(errors.is_empty(), "неожиданные находки: {errors:?}");
    }

    /// Контрпример, отменивший правило «имя совпало с глобальной функцией».
    ///
    /// `СтрЗаканчиваетсяНа` и `ПроверитьЗаполнение` — платформенные имена, но
    /// объявлять процедуры с такими именами платформа разрешает: в обоих
    /// реальных случаях она сообщила «уже определена», то есть объявление
    /// состоялось. Находка здесь была бы ложной.
    #[test]
    fn platform_method_name_is_allowed_as_declaration() {
        let src = "Функция СтрЗаканчиваетсяНа(Стр, Ок)\n\tВозврат Истина;\nКонецФункции\n\n\
                   Функция ПроверитьЗаполнение()\n\tВозврат Истина;\nКонецФункции";
        assert!(run(src).is_empty(), "ложные находки: {:?}", run(src));
    }

    /// Имя из наблюдательного списка отказов платформы.
    #[test]
    fn platform_rejected_name_is_caught() {
        let errors = run("&НаКлиенте\nПроцедура Найти(Команда)\nКонецПроцедуры");
        assert!(kinds(&errors).contains(&ExprErrorKind::ReservedProcedureName));
    }

    #[test]
    fn duplicate_declaration_is_caught() {
        let src = "Функция СтрЗаканчиваетсяНа(А, Б)\n\tВозврат Истина;\nКонецФункции\n\n\
                   Функция СтрЗаканчиваетсяНа(А, Б)\n\tВозврат Ложь;\nКонецФункции";
        let errors = run(src);
        assert!(kinds(&errors).contains(&ExprErrorKind::DuplicateDeclaration));
    }

    #[test]
    fn extra_end_of_block_is_caught() {
        let src = "Функция Значение()\n\tВозврат 1;\nКонецФункции\nКонецФункции\n";
        let errors = run(src);
        assert!(kinds(&errors).contains(&ExprErrorKind::UnbalancedModuleBlock));
    }

    #[test]
    fn missing_end_of_block_is_caught() {
        let src = "Процедура Раз()\n\tСообщить(1);\n";
        let errors = run(src);
        assert!(kinds(&errors).contains(&ExprErrorKind::UnbalancedModuleBlock));
    }

    #[test]
    fn balanced_module_is_silent() {
        let src = "&НаСервере\nПроцедура Раз()\n\tДва();\nКонецПроцедуры\n\n\
                   &НаСервере\nФункция Два()\n\tВозврат 1;\nКонецФункции\n";
        assert!(run(src).is_empty());
    }

    /// Неразрывный пробел в отступе перед `КонецПроцедуры` — так набран отступ
    /// в типовой форме «Подтверждение зачисления зарплаты».
    #[test]
    fn nbsp_indent_does_not_hide_end_of_block() {
        let src = "&НаКлиенте\nПроцедура Раз(Команда)\n\tСообщить(1);\n\u{00A0}КонецПроцедуры\n\n\
                   &НаКлиенте\nПроцедура Два(Команда)\n\tСообщить(2);\nКонецПроцедуры\n";
        assert!(run(src).is_empty(), "ложные находки: {:?}", run(src));
    }

    /// Защищённый модуль поставщика: конец одной функции и заголовок следующей
    /// на одной строке. Пока разбор был построчным — 12 ложных находок на УТ.
    #[test]
    fn obfuscated_one_line_module_is_balanced() {
        let src = "функция a()возврат 1;конецфункции  функция б()возврат 2;конецфункции\n";
        assert!(run(src).is_empty(), "ложные находки: {:?}", run(src));
    }

    /// Слово-часть идентификатора заголовком не считается.
    #[test]
    fn word_inside_identifier_is_not_a_keyword() {
        let src = "Процедура Раз()\n\tМояФункцияРасчёта = 1;\n\tПроцедураПодготовки = 2;\nКонецПроцедуры\n";
        assert!(run(src).is_empty(), "ложные находки: {:?}", run(src));
    }

    /// Свойство с именем `Процедура` — не заголовок. Обработчики обновления
    /// БСП пишутся именно так, и это дало 1317 ложных находок на выгрузке УТ.
    #[test]
    fn property_named_procedure_is_not_a_header() {
        let src = "Процедура ПриДобавленииОбработчиковОбновления(Обработчики) Экспорт\n\
                   \tОбработчик = Обработчики.Добавить();\n\
                   \tОбработчик.Процедура = \"РегистрыНакопления.Х.Обработать\";\n\
                   \tОбработчик.ПроцедураПроверки = \"ОбновлениеИнформационнойБазы.Данные\";\n\
                   КонецПроцедуры\n";
        assert!(run(src).is_empty(), "ложные находки: {:?}", run(src));
    }

    /// Выгрузки 1С идут в UTF-8 с BOM. Пока метка не снималась, первый
    /// заголовок модуля терялся: 197 ложных находок на выгрузке УТ.
    #[test]
    fn bom_does_not_break_first_header() {
        let src = "\u{FEFF}Процедура Раз() Экспорт\n\tСообщить(1);\nКонецПроцедуры\n";
        assert!(run(src).is_empty(), "находки на корректном модуле: {:?}", run(src));
    }

    /// Асинхронный метод — полноценный заголовок. Пока модификатор не
    /// снимался, его `КонецПроцедуры` считался лишним: 853 ложных находки на
    /// выгрузке УТ.
    #[test]
    fn async_procedure_is_a_header() {
        let src = "&НаКлиенте\nАсинх Процедура Раз(Команда)\n\tСообщить(1);\nКонецПроцедуры\n";
        assert!(run(src).is_empty(), "находки на корректном модуле");
    }

    /// Имя асинхронной процедуры проверяется наравне с обычной.
    #[test]
    fn async_procedure_name_is_checked() {
        let src = "Асинх Процедура Выполнить(Команда)\nКонецПроцедуры\n";
        assert!(kinds(&run(src)).contains(&ExprErrorKind::ReservedProcedureName));
    }

    /// `АсинхПроцедура` без пробела — одно имя, а не модификатор с заголовком.
    #[test]
    fn async_without_space_is_not_a_modifier() {
        let src = "АсинхПроцедура = 1;\n";
        assert!(run(src).is_empty());
    }

    /// Слово `Функция` внутри строкового литерала объявлением не считается:
    /// маскировка вычищает литералы до разбора.
    #[test]
    fn keyword_inside_string_literal_is_ignored() {
        let src = "Процедура Раз()\n\tТекст = \"Функция Выполнить(А)\";\nКонецПроцедуры\n";
        assert!(run(src).is_empty());
    }
}
