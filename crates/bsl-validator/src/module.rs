//! Phase 9 — валидация ЦЕЛОГО BSL-модуля.
//!
//! В отличие от [`crate::expression::validate_expression`], который принимает
//! короткий фрагмент и не отличает вызов «своей» процедуры от опечатки
//! платформенного метода, `validate_module` работает со всем текстом модуля:
//! из него извлекаются все объявления `Процедура ИмяX(...)` и `Функция ИмяY(...)`,
//! собирается whitelist «своих» имён — и `check_global_calls` пропускает эти
//! вызовы без fuzzy-проверки.
//!
//! Так исчезает основной источник false-positive для новой находки
//! `UnknownGlobalMethod`: неизвестный вызов, которого нет в whitelist И
//! который похож на платформенный, — почти всегда опечатка.
//!
//! Раз текст модуля прислан целиком, вызов `Имя(...)` без объявления здесь и без
//! платформенного метода — описка либо забытая процедура, даже когда он ни на
//! что не похож. Такой вызов эмиттится как `ExprErrorKind::UndeclaredMethod`
//! (строгий режим `check_global_calls`, включается передачей whitelist).
//! Исключение, дающее false-positive: процедуры глобальных общих модулей
//! (флаг «Глобальный») зовутся без префикса и валидатору не видны.
//!
//! Имена директив (`&НаСервере`, `&Перед("...")`) валидируются против
//! статического списка [`crate::directives::KNOWN_DIRECTIVES`] с fuzzy-подсказкой
//! (`&НаКлентее` → `НаКлиенте`) — эмиттится `ExprErrorKind::UnknownDirective`.
//! Проверка идёт по ТЕКСТУ, а не по AST: грамматика `tree-sitter-bsl` заводит
//! узел `annotation` только для директив, которые знает сама, а неизвестная —
//! ровно та, что нас интересует, — попадает в `ERROR` без выделенного имени.

use std::collections::HashSet;

use platform_index::PlatformIndex;

use crate::directives::{closest_directive_with_distance, is_extension_module, is_known_directive};
use crate::expression::{
    check_global_calls, check_new_expressions, check_type_dot_members, fuzzy_confidence_for,
    mask_strings_and_comments, strip_extension_directives, Confidence, ExprError, ExprErrorKind,
    ExpressionValidation, Profile,
};
use crate::scope::{extract_scope_map, extract_type_annotations};

/// Главный API: проверить целый BSL-модуль. Дефолтный уровень — 1.
pub fn validate_module(index: &PlatformIndex, source: &str) -> ExpressionValidation {
    validate_module_at_level(index, source, 1)
}

/// Проверка модуля с явным уровнем валидации.
///
/// Семантика уровней ровно та же, что у [`crate::expression::validate_expression_at_level`],
/// плюс:
/// - Извлекается whitelist объявленных в модуле процедур/функций (proc/func-имена).
///   При проверке глобальных вызовов вызовы из whitelist пропускаются до
///   fuzzy-этапа — false-positive «опечатка платформенного метода» на своей
///   процедуре не эмиттится.
/// - Проверяются имена директив (`&НаСервере`, `&Перед`, …) через
///   [`crate::directives::KNOWN_DIRECTIVES`]; промах → `UnknownDirective`.
pub fn validate_module_at_level(
    index: &PlatformIndex,
    source: &str,
    level: u8,
) -> ExpressionValidation {
    // Модуль расширения: блоки `#Удаление … #КонецУдаления` в скомпилированный
    // модуль не попадают, но могут обрывать строковый литерал на середине —
    // тогда файл не является корректным BSL, маскировка «съезжает» и текст
    // запроса ниже принимается за код. Убираем их до всякого разбора.
    let source = &strip_extension_directives(source);
    let cleaned = mask_strings_and_comments(source);

    let scope_map = if level >= 2 {
        let annotations = extract_type_annotations(source);
        Some(extract_scope_map(index, &cleaned, &annotations, level))
    } else {
        None
    };

    let mut errors = Vec::new();
    // Whitelist объявлений строится из ДВУХ источников, потому что ни один не
    // полон (замер на 14905 модулях УТ): разбор находит 2753 имени, невидимых
    // текстовому проходу (заголовок с переносом строки перед скобкой), а текст
    // спасает 34 имени, теряемых разбором на файлах с ERROR. Пропущенное имя в
    // строгом режиме сразу даёт ложный `UndeclaredMethod`, поэтому берётся
    // объединение. AST читает ОРИГИНАЛЬНЫЙ source: строки и комментарии
    // tree-sitter отсекает сам.
    let mut user_symbols = walk_module_ast(source);
    user_symbols.extend(scan_declarations(source));

    scan_directives(&cleaned, &mut errors);

    // Модуль расширения компилируется вместе с расширяемым и напрямую зовёт его
    // процедуры. Их текста у валидатора нет, поэтому вывод «вызов не объявлен —
    // описка» здесь неправомерен: строгий режим выключаем, whitelist остаётся.
    let strict_unknown = !is_extension_module(&cleaned);

    check_type_dot_members(index, &cleaned, scope_map.as_ref(), &mut errors);
    check_new_expressions(index, &cleaned, &mut errors);
    check_global_calls(
        index,
        &cleaned,
        Some(&user_symbols),
        strict_unknown,
        &mut errors,
    );

    ExpressionValidation {
        valid: errors.is_empty(),
        errors,
    }
}

/// Проверка модуля с учётом профиля потребителя (см. `Profile`).
///
/// - [`Profile::Full`] — `level` берётся как передан, возвращаются все находки.
/// - [`Profile::Strict`] — `level` форсируется в `1`, остаются только
///   high-confidence находки; `valid` пересчитывается.
pub fn validate_module_with_profile(
    index: &PlatformIndex,
    source: &str,
    level: u8,
    profile: Profile,
) -> ExpressionValidation {
    let effective_level = if profile == Profile::Strict { 1 } else { level };
    let mut result = validate_module_at_level(index, source, effective_level);

    if profile == Profile::Strict {
        result
            .errors
            .retain(|e| e.confidence == Confidence::High);
        result.valid = result.errors.is_empty();
    }

    result
}

/// Запасной сбор объявлений построчно — на случай, когда tree-sitter не смог
/// разобрать модуль и часть `proc_declaration`/`func_declaration` потерялась.
///
/// Строки и комментарии предварительно замаскированы, поэтому слово `Процедура`
/// внутри строкового литерала объявлением не станет. Имя берётся до первой
/// открывающей скобки; строки без скобки игнорируются.
fn scan_declarations(source: &str) -> HashSet<String> {
    let cleaned = mask_strings_and_comments(source);
    let mut names = HashSet::new();
    for line in cleaned.lines() {
        let trimmed = line.trim_start();
        let lower = trimmed.to_lowercase();
        let rest = ["процедура ", "функция ", "procedure ", "function "]
            .iter()
            .find_map(|kw| lower.strip_prefix(kw));
        let Some(rest) = rest else { continue };
        let Some((name, _)) = rest.split_once('(') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            names.insert(name.to_string());
        }
    }
    names
}

/// Проход по AST BSL-модуля: собирает whitelist proc/func-имён.
/// Один tree-sitter-parse на весь модуль.
///
/// Устойчивость к синтаксическим ошибкам: tree-sitter даёт `ERROR`-узлы и
/// продолжает разбор, поэтому даже поломанный модуль отдаёт часть объявлений.
/// На пустой строке / двоичном мусоре возвращается пустой whitelist —
/// недостающие имена подберёт `scan_declarations`.
fn walk_module_ast(source: &str) -> HashSet<String> {
    let mut names = HashSet::new();

    // Двоичный .bsl (EDT-защищённые модули поставщика) — не отдаём в tree-sitter,
    // иначе он деградирует на бесструктурном вводе. Маркер — NUL-байт
    // в первых 8 КБ (см. `code-index::parser::bsl::looks_binary`).
    if source.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return names;
    }

    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_bsl::LANGUAGE.into())
        .is_err()
    {
        // Не смогли выставить язык — молча возвращаем пустой whitelist.
        // Валидатор продолжит без него, это не блокирующая ошибка.
        return names;
    }
    // Страховка от патологического ввода: 10-секундный дедлайн парсинга.
    // При превышении parse() вернёт None → пустой whitelist.
    #[allow(deprecated)]
    parser.set_timeout_micros(10_000 * 1000);

    let Some(tree) = parser.parse(source, None) else {
        return names;
    };
    let source_bytes = source.as_bytes();

    walk_recursive(tree.root_node(), source_bytes, &mut names, 0);

    names
}

/// Рекурсивный обход AST: собирает имена объявлений процедур и функций.
/// Ограничение глубины — 80 (совпадает с code-index-core:parser/bsl.rs).
///
/// Директивы здесь НЕ проверяются: грамматика `tree-sitter-bsl` заводит узел
/// `annotation` только для КОРРЕКТНЫХ директив, а опечатка (`&НаКлентее`)
/// приходит как `ERROR`. Опечатки ловит `scan_directives` по тексту.
fn walk_recursive(
    node: tree_sitter::Node,
    source_bytes: &[u8],
    names: &mut HashSet<String>,
    depth: usize,
) {
    if depth > 80 {
        return;
    }

    if matches!(node.kind(), "procedure_definition" | "function_definition") {
        if let Some(name) = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source_bytes).ok())
        {
            if !name.is_empty() {
                names.insert(name.to_lowercase());
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_recursive(child, source_bytes, names, depth + 1);
    }
}

/// Проверка имён директив (`&НаСервере`, `&Перед("Foo")`) текстовым проходом.
///
/// Через AST это сделать нельзя: `tree-sitter-bsl` заводит узел `annotation`
/// только для директив, которые знает сама грамматика, а неизвестная (то есть
/// ровно та, что нас интересует) попадает в `ERROR`-узел без выделенного имени.
///
/// `source` берётся замаскированным: `&` внутри строки или комментария
/// директивой не считается. Пороги — те же, что у `UnknownGlobalMethod`:
/// сильное сходство → High, слабое → Low, далёкое → молча пропускаем.
fn scan_directives(cleaned: &str, errors: &mut Vec<ExprError>) {
    for (row, line) in cleaned.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('&') else {
            continue;
        };
        // Имя — до открывающей скобки (`&Перед("Foo")`) либо до конца слова.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() || is_known_directive(&name) {
            continue;
        }
        let Some((suggestion, distance)) = closest_directive_with_distance(&name) else {
            continue;
        };
        if distance == 0 {
            continue;
        }
        let Some(confidence) = fuzzy_confidence_for(&name, &suggestion, distance) else {
            continue;
        };
        let line_no = (row + 1) as u32;
        let col = (line.len() - trimmed.len() + 1) as u32;
        errors.push(ExprError::new_with_confidence(
            line_no,
            col,
            ExprErrorKind::UnknownDirective,
            format!(
                "Неизвестная директива '&{}'. Возможно, вы имели в виду '&{}'.",
                name, suggestion
            ),
            confidence,
            Some(suggestion),
            Vec::new(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_names(src: &str) -> HashSet<String> {
        let mut names = walk_module_ast(src);
        names.extend(scan_declarations(src));
        names
    }

    /// Находки по директивам — тем же путём, что и в `validate_module_at_level`.
    fn collect_directive_errors(src: &str) -> Vec<ExprError> {
        let cleaned = mask_strings_and_comments(src);
        let mut errors = Vec::new();
        scan_directives(&cleaned, &mut errors);
        errors
    }

    #[test]
    fn extract_declarations_finds_proc_and_func() {
        let src = "\
Процедура МояПроцедура() Экспорт
КонецПроцедуры

Функция МояФункция(Пар)
    Возврат Пар;
КонецФункции
";
        let names = collect_names(src);
        assert!(names.contains("мояпроцедура"), "proc не найдена: {:?}", names);
        assert!(names.contains("мояфункция"), "func не найдена: {:?}", names);
    }

    #[test]
    fn extract_declarations_empty_on_binary() {
        let names = collect_names("\u{0}\u{2}garbage\u{0}");
        assert!(names.is_empty());
    }

    #[test]
    fn bom_does_not_break_declarations() {
        // Выгрузка 1С пишет модули с UTF-8 BOM. Он попадает первым символом
        // перед `Процедура` и не должен мешать сбору объявлений.
        let src = "\u{FEFF}Процедура МояПроцедура() Экспорт\nКонецПроцедуры\n";
        let names = collect_names(src);
        assert!(
            names.contains("мояпроцедура"),
            "BOM сломал сбор объявлений: {:?}",
            names
        );
    }

    #[test]
    fn extract_declarations_english_keywords() {
        let src = "\
Procedure OnOpen() Export
EndProcedure

Function GetData(P) Export
    Return P;
EndFunction
";
        let names = collect_names(src);
        assert!(names.contains("onopen"), "OnOpen не найден: {:?}", names);
        assert!(names.contains("getdata"), "GetData не найден: {:?}", names);
    }

    #[test]
    fn known_directive_yields_no_error() {
        let src = "\
&НаСервере
Процедура ОбработатьНаСервере()
КонецПроцедуры
";
        let errors = collect_directive_errors(src);
        assert!(
            errors.is_empty(),
            "известная директива дала ошибку: {:?}",
            errors
        );
    }

    #[test]
    fn typo_directive_yields_error() {
        let src = "\
&НаКлентее
Процедура НаКлиенте()
КонецПроцедуры
";
        let errors = collect_directive_errors(src);
        let unknown = errors
            .iter()
            .filter(|e| matches!(e.kind, ExprErrorKind::UnknownDirective))
            .count();
        assert_eq!(unknown, 1, "должна быть одна UnknownDirective: {:?}", errors);
        let e = errors
            .iter()
            .find(|e| matches!(e.kind, ExprErrorKind::UnknownDirective))
            .unwrap();
        assert_eq!(e.suggestion.as_deref(), Some("НаКлиенте"));
    }

    #[test]
    fn override_directive_ok() {
        let src = "\
&Перед(\"ОригинальнаяПроцедура\")
Процедура Ext_ОригинальнаяПроцедура()
КонецПроцедуры
";
        let errors = collect_directive_errors(src);
        assert!(
            errors.is_empty(),
            "директива расширения дала ошибку: {:?}",
            errors
        );
    }
}
