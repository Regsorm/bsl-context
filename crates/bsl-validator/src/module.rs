//! Phase 9 — валидация ЦЕЛОГО BSL-модуля.
//!
//! В отличие от [`crate::expression::validate_expression`], который принимает
//! короткий фрагмент и не отличает вызов «своей» процедуры от опечатки
//! платформенного метода, `validate_module` работает со всем текстом модуля:
//! из него извлекаются все объявления `Процедура ИмяX(...)` и `Функция ИмяY(...)`
//! (один проход [`crate::ast::collect_facts`], общий с фактами `TypeDotMember`/
//! `NewExpression`/`GlobalCall`), собирается whitelist «своих» имён — и
//! `check_global_calls` пропускает эти вызовы без fuzzy-проверки.
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
//!
//! Присваивания и объявления `Перем`, чьё имя занято членом контекста модуля,
//! которому нельзя присвоить (метод либо свойство «только чтение» — глобального
//! контекста или, в модуле формы, типа `ФормаКлиентскогоПриложения`), эмиттятся
//! как `ExprErrorKind::ShadowedContextName` (см. `crate::context_names`): такое
//! присваивание не создаёт локальную переменную и падает в рантайме.

use std::collections::HashSet;

use platform_index::PlatformIndex;

use bsl_parse::{collect_facts, scan_declarations};

use crate::context_names::check_shadowed_context_names;
use crate::directives::{closest_directive_with_distance, is_extension_module, is_known_directive};
use crate::expression::{
    check_global_calls, check_new_expressions, check_type_dot_members, fuzzy_confidence_for,
    mask_strings_and_comments, strip_extension_directives, Confidence, ExprError, ExprErrorKind,
    ExpressionValidation, Profile,
};
use crate::scope::{extract_scope_map, extract_type_annotations};
use crate::symbols::SymbolSource;

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
    validate_module_at_level_inner(index, source, level, None, None, None, None)
}

/// Общее тело `validate_module_at_level`/`validate_module_with_symbols`.
///
/// `module_path` — относительный путь модуля в выгрузке; нужен, чтобы понять,
/// что это модуль формы (`.../Forms/<Имя>/Ext/Form/Module.bsl` или
/// `.../Form/<Имя>/Form.obj.bsl`), и включить проверку имён, занятых членами
/// `ФормаКлиентскогоПриложения` (`crate::context_names`). `None` — правило по
/// членам формы не применяется, находки по свойствам глобального контекста
/// остаются.
///
/// `form_attributes` — имена реквизитов формы (нижний регистр), если вызывающий
/// их знает: реквизит перекрывает имя контекста. Передан — внутри модуля формы
/// включается и проверка имён глобального контекста; `None` — она остаётся
/// выключенной, чтобы не ругаться на реквизит, которого валидатор не видит.
///
/// `symbols`/`owner_exports` — внешний источник имён (см.
/// [`crate::symbols::SymbolSource`]) и предзагруженный набор экспортных имён
/// модуля объекта-владельца; оба `None` при вызове без источника — поведение
/// не отличается от прежнего `validate_module_at_level`.
#[allow(clippy::too_many_arguments)]
fn validate_module_at_level_inner(
    index: &PlatformIndex,
    source: &str,
    level: u8,
    module_path: Option<&str>,
    form_attributes: Option<&HashSet<String>>,
    symbols: Option<&dyn SymbolSource>,
    owner_exports: Option<&HashSet<String>>,
) -> ExpressionValidation {
    // Модуль расширения: блоки `#Удаление … #КонецУдаления` в скомпилированный
    // модуль не попадают, но могут обрывать строковый литерал на середине —
    // тогда файл не является корректным BSL, маскировка «съезжает» и текст
    // запроса ниже принимается за код. Убираем их до всякого разбора.
    let source = &strip_extension_directives(source);
    let cleaned = mask_strings_and_comments(source);
    let facts = collect_facts(source);

    let scope_map = if level >= 2 {
        let annotations = extract_type_annotations(source);
        Some(extract_scope_map(index, &cleaned, &annotations, level))
    } else {
        None
    };
    let mut errors = Vec::new();

    // Объявления из ДВУХ источников: дерево теряет их на файлах с ERROR,
    // текстовый проход не видит заголовков с переносом перед скобкой.
    let mut user_symbols = facts.declarations.clone();
    user_symbols.extend(scan_declarations(source));

    scan_directives(&cleaned, &mut errors);

    // Модуль расширения компилируется вместе с расширяемым и напрямую зовёт его
    // процедуры. Их текста у валидатора нет, поэтому вывод «вызов не объявлен —
    // описка» здесь неправомерен: строгий режим выключаем, whitelist остаётся.
    let strict_unknown = !is_extension_module(&cleaned);

    check_type_dot_members(index, source, &facts.dots, scope_map.as_ref(), &mut errors);
    check_new_expressions(index, source, &facts.news, &mut errors);
    check_global_calls(
        index,
        source,
        &facts.calls,
        Some(&user_symbols),
        strict_unknown,
        symbols,
        owner_exports,
        &mut errors,
    );
    let form_module = module_path
        .map(crate::context_names::is_form_module)
        .unwrap_or(false);
    check_shadowed_context_names(
        index,
        source,
        &facts,
        form_module,
        form_attributes,
        &mut errors,
    );
    errors.sort_by_key(|e| (e.line, e.col));

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
/// - `module_path` — см. [`validate_module_at_level_inner`]: нужен, чтобы
///   применить правило по членам формы (`ФормаКлиентскогоПриложения`).
/// - `form_attributes` — имена реквизитов формы, если известны; см. там же.
pub fn validate_module_with_profile(
    index: &PlatformIndex,
    source: &str,
    module_path: Option<&str>,
    form_attributes: Option<&HashSet<String>>,
    level: u8,
    profile: Profile,
) -> ExpressionValidation {
    let effective_level = if profile == Profile::Strict { 1 } else { level };
    let mut result = validate_module_at_level_inner(
        index,
        source,
        effective_level,
        module_path,
        form_attributes,
        None,
        None,
    );

    if profile == Profile::Strict {
        result
            .errors
            .retain(|e| e.confidence == Confidence::High);
        result.valid = result.errors.is_empty();
    }

    result
}

/// Проверка модуля с внешним источником имён (см. [`crate::symbols::SymbolSource`]).
///
/// Как [`validate_module_with_profile`], но `check_global_calls` дополнительно
/// получает `symbols` и предзагруженные экспортные имена модуля
/// объекта-владельца — для этого нужен `module_path` (относительный путь
/// модуля в выгрузке); `None`, если он неизвестен или не нужен.
#[allow(clippy::too_many_arguments)]
pub fn validate_module_with_symbols(
    index: &PlatformIndex,
    source: &str,
    level: u8,
    profile: Profile,
    module_path: Option<&str>,
    form_attributes: Option<&HashSet<String>>,
    symbols: Option<&dyn SymbolSource>,
) -> ExpressionValidation {
    let effective_level = if profile == Profile::Strict { 1 } else { level };
    let owner_exports = module_path
        .zip(symbols)
        .and_then(|(path, src)| src.owner_exports(path));
    let mut result = validate_module_at_level_inner(
        index,
        source,
        effective_level,
        module_path,
        form_attributes,
        symbols,
        owner_exports.as_ref(),
    );

    if profile == Profile::Strict {
        result
            .errors
            .retain(|e| e.confidence == Confidence::High);
        result.valid = result.errors.is_empty();
    }

    result
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
        let mut names = collect_facts(src).declarations;
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

    // ── validate_module_with_symbols ───────────────────────────────────────

    /// Источник-заглушка: поведение каждого метода задаётся явно на тест,
    /// без похода в реальный индекс/БД.
    struct StubSource {
        global_export: bool,
        exists: bool,
        owner: Option<HashSet<String>>,
    }

    impl SymbolSource for StubSource {
        fn is_global_export(&self, _name_lower: &str) -> bool {
            self.global_export
        }

        fn method_exists(&self, _name_lower: &str) -> bool {
            self.exists
        }

        fn owner_exports(&self, _module_path: &str) -> Option<HashSet<String>> {
            self.owner.clone()
        }

        fn describe(&self) -> String {
            "stub".to_string()
        }
    }

    /// Модуль с ровно одним неизвестным вызовом — общая фикстура для тестов ниже.
    fn module_with_unknown_call() -> &'static str {
        "\
Процедура Тест()
НеизвестныйВызов();
КонецПроцедуры
"
    }

    #[test]
    fn symbols_global_export_suppresses_finding() {
        let index = PlatformIndex::new();
        let source = StubSource {
            global_export: true,
            exists: false,
            owner: None,
        };
        let result = validate_module_with_symbols(
            &index,
            module_with_unknown_call(),
            1,
            Profile::Full,
            None,
            None,
            Some(&source),
        );
        assert!(
            result.errors.is_empty(),
            "экспорт глобального модуля не должен давать находку: {:?}",
            result.errors
        );
    }

    #[test]
    fn symbols_owner_export_suppresses_finding() {
        let index = PlatformIndex::new();
        let mut owner = HashSet::new();
        owner.insert("неизвестныйвызов".to_string());
        let source = StubSource {
            global_export: false,
            exists: false,
            owner: Some(owner),
        };
        let result = validate_module_with_symbols(
            &index,
            module_with_unknown_call(),
            1,
            Profile::Full,
            Some("external/Обр/Form/Ф/Form.obj.bsl"),
            None,
            Some(&source),
        );
        assert!(
            result.errors.is_empty(),
            "метод модуля объекта-владельца не должен давать находку: {:?}",
            result.errors
        );
    }

    #[test]
    fn symbols_method_exists_downgrades_confidence() {
        let index = PlatformIndex::new();
        let source = StubSource {
            global_export: false,
            exists: true,
            owner: None,
        };
        let result = validate_module_with_symbols(
            &index,
            module_with_unknown_call(),
            1,
            Profile::Full,
            None,
            None,
            Some(&source),
        );
        let finding = result
            .errors
            .iter()
            .find(|e| matches!(e.kind, ExprErrorKind::UndeclaredMethod))
            .expect("метод есть в конфигурации — находка должна остаться");
        assert_eq!(finding.confidence, Confidence::Low);
    }

    #[test]
    fn no_symbol_source_keeps_high_confidence() {
        let index = PlatformIndex::new();
        let result = validate_module_with_symbols(
            &index,
            module_with_unknown_call(),
            1,
            Profile::Full,
            None,
            None,
            None,
        );
        let finding = result
            .errors
            .iter()
            .find(|e| matches!(e.kind, ExprErrorKind::UndeclaredMethod))
            .expect("без источника поведение должно быть прежним — находка есть");
        assert_eq!(finding.confidence, Confidence::High);
    }

    // ── validate_module_with_profile: сигнатура с module_path ──────────────

    #[test]
    fn validate_module_with_profile_accepts_module_path() {
        // Сторожевой тест на сигнатуру: на пустом PlatformIndex реквизит формы
        // «Результат» ни с чем не совпадает, находок нет ни при каком module_path.
        let index = PlatformIndex::new();
        let result = validate_module_with_profile(
            &index,
            "Процедура Т()\nРезультат = \"текст\";\nКонецПроцедуры\n",
            Some("base/Catalogs/Х/Forms/Ф/Ext/Form/Module.bsl"),
            None,
            1,
            Profile::Full,
        );
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }
}
