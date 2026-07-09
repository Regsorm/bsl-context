//! Phase 9 — валидация ЦЕЛОГО BSL-модуля.
//!
//! В отличие от [`crate::expression::validate_expression`], который принимает
//! короткий фрагмент и не отличает вызов «своей» процедуры от опечатки
//! платформенного метода, `validate_module` работает со всем текстом модуля:
//! tree-sitter извлекает все объявления `Процедура ИмяX(...)` и
//! `Функция ИмяY(...)`, собирает whitelist «своих» имён — и `check_global_calls`
//! пропускает эти вызовы без fuzzy-проверки.
//!
//! Так исчезает основной источник false-positive для новой находки
//! `UnknownGlobalMethod`: неизвестный вызов, которого нет в whitelist И
//! который похож на платформенный, — почти всегда опечатка.
//!
//! Кроме того, при обходе AST мы натыкаемся на узлы `annotation`
//! (директивы `&НаСервере`, `&Перед("...")`, ...). Их имя валидируется против
//! статического списка [`crate::directives::KNOWN_DIRECTIVES`] с fuzzy-подсказкой
//! (`&НаКлентее` → `НаКлиенте`) — эмиттится `ExprErrorKind::UnknownDirective`.

use std::collections::HashSet;

use platform_index::PlatformIndex;

use crate::directives::{closest_directive_with_distance, is_known_directive};
use crate::expression::{
    check_global_calls, check_new_expressions, check_type_dot_members, fuzzy_confidence_for,
    mask_strings_and_comments, Confidence, ExprError, ExprErrorKind, ExpressionValidation, Profile,
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
    let cleaned = mask_strings_and_comments(source);

    let scope_map = if level >= 2 {
        let annotations = extract_type_annotations(source);
        Some(extract_scope_map(index, &cleaned, &annotations, level))
    } else {
        None
    };

    let mut errors = Vec::new();
    // Один проход tree-sitter собирает и whitelist процедур/функций,
    // и находки по неизвестным директивам. Whitelist строится на ОРИГИНАЛЬНОМ
    // source (не на cleaned): tree-sitter корректно обрабатывает строки и
    // комментарии сам.
    let user_symbols = walk_module_ast(source, &mut errors);

    check_type_dot_members(index, &cleaned, scope_map.as_ref(), &mut errors);
    check_new_expressions(index, &cleaned, &mut errors);
    check_global_calls(index, &cleaned, Some(&user_symbols), &mut errors);

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

/// Единый проход по AST BSL-модуля: собирает whitelist proc/func-имён и
/// эмиттит находки `UnknownDirective` в `errors`. Один tree-sitter-parse на
/// весь модуль.
///
/// Устойчивость к синтаксическим ошибкам: tree-sitter даёт `ERROR`-узлы и
/// продолжает разбор, поэтому даже поломанный модуль отдаёт часть объявлений.
/// На пустой строке / двоичном мусоре возвращается пустой whitelist,
/// в `errors` ничего не добавляется — вызывающий получит fuzzy как для
/// одиночного выражения.
fn walk_module_ast(source: &str, errors: &mut Vec<ExprError>) -> HashSet<String> {
    let mut names = HashSet::new();

    // Двоичный .bsl (EDT-защищённые модули поставщика) — не отдаём в tree-sitter,
    // иначе он деградирует на бесструктурном вводе. Маркер — NUL-байт
    // в первых 8 КБ (см. `code-index::parser::bsl::looks_binary`).
    if source.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return names;
    }

    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_onescript::LANGUAGE.into())
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

    walk_recursive(tree.root_node(), source_bytes, &mut names, errors, 0);

    names
}

/// Рекурсивный обход AST: собирает имена proc_declaration/func_declaration и
/// эмиттит `UnknownDirective` по annotation-узлам. Ограничение глубины — 80
/// (совпадает с параметром code-index-core:parser/bsl.rs).
fn walk_recursive(
    node: tree_sitter::Node,
    source_bytes: &[u8],
    names: &mut HashSet<String>,
    errors: &mut Vec<ExprError>,
    depth: usize,
) {
    if depth > 80 {
        return;
    }

    match node.kind() {
        "proc_declaration" => {
            if let Some(name) = node
                .child_by_field_name("proc_name")
                .and_then(|n| n.utf8_text(source_bytes).ok())
            {
                if !name.is_empty() {
                    names.insert(name.to_lowercase());
                }
            }
        }
        "func_declaration" => {
            if let Some(name) = node
                .child_by_field_name("func_name")
                .and_then(|n| n.utf8_text(source_bytes).ok())
            {
                if !name.is_empty() {
                    names.insert(name.to_lowercase());
                }
            }
        }
        "annotation" => {
            // Первый identifier внутри annotation — имя директивы без амперсанда.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    if let Ok(name) = child.utf8_text(source_bytes) {
                        emit_directive_check(name, node, errors);
                    }
                    break;
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_recursive(child, source_bytes, names, errors, depth + 1);
    }
}

/// Проверить имя директивы и эмиттить `UnknownDirective`, если промах.
/// Пороги те же, что у `UnknownGlobalMethod`: сильное сходство → High,
/// слабое → Low, далёкое → молча пропускаем.
fn emit_directive_check(
    name: &str,
    annotation_node: tree_sitter::Node,
    errors: &mut Vec<ExprError>,
) {
    if is_known_directive(name) {
        return;
    }
    let Some((suggestion, distance)) = closest_directive_with_distance(name) else {
        return;
    };
    // distance==0 при промахе is_known_directive невозможен: в whitelist есть
    // регистронезависимая нормализация в обе стороны. Но защитимся — молча
    // выйти дешевле, чем эмиттить противоречивую ошибку.
    if distance == 0 {
        return;
    }
    let Some(confidence) = fuzzy_confidence_for(name, &suggestion, distance) else {
        return;
    };
    // Позиция annotation-узла (0-based → 1-based для API).
    let start = annotation_node.start_position();
    let line = (start.row + 1) as u32;
    // column у tree-sitter в UTF-8 байтах: русские буквы — многобайтные, поэтому
    // «колонка» может показаться странной пользователю. На MVP оставляем как
    // есть; MCP-клиенты обычно всё равно ориентируются по номеру строки.
    let col = (start.column + 1) as u32;
    errors.push(ExprError::new_with_confidence(
        line,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_names(src: &str) -> HashSet<String> {
        let mut errors = Vec::new();
        walk_module_ast(src, &mut errors)
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
        let mut errors = Vec::new();
        walk_module_ast(src, &mut errors);
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
        let mut errors = Vec::new();
        walk_module_ast(src, &mut errors);
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
        let mut errors = Vec::new();
        walk_module_ast(src, &mut errors);
        assert!(
            errors.is_empty(),
            "директива расширения дала ошибку: {:?}",
            errors
        );
    }
}
