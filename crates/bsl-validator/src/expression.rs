//! Phase 6 — валидация BSL-выражений (Уровень 1, MVP).
//!
//! Извлекаем три класса конструкций без полного парсера BSL — этого хватает
//! для статических ссылок на платформенный контекст:
//!
//! - **TypeDotMember**: `<Идентификатор1>.<Идентификатор2>`.
//!   Проверяется, если `<Идентификатор1>` совпадает с именем типа в
//!   `PlatformIndex.types`. Для типа-перечисления — `<Идентификатор2>`
//!   должно быть среди `enum_values`. Для обычного типа — среди
//!   `methods/properties`. Чужие случаи (имя слева — переменная, не тип)
//!   пропускаются — для этого нужен Уровень 2 (type inference, Phase 8).
//!
//! - **NewExpression**: `Новый <Идентификатор>` или `Новый <Идентификатор>(args)`.
//!   `<Идентификатор>` должен быть в `PlatformIndex.types`.
//!
//! - **GlobalCall**: `<Идентификатор>(args)` на верхнем уровне (без точки слева).
//!   Если `<Идентификатор>` есть в `global_methods` — проверяем число аргументов
//!   через `validate_method_call`.
//!
//! Перед извлечением исходник проходит через [`mask_strings_and_comments`],
//! где `"..."` / `|...` / `//...` заменяются на пробелы той же длины: это
//! сохраняет line/col, но не даёт regex захватить содержимое строк.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use platform_index::PlatformIndex;

use bsl_parse::{collect_facts, CallFact, DotFact, NewFact};

use crate::check::{validate_method_call, SimilarValue};
use crate::scope::{extract_scope_map, extract_type_annotations, ScopeMap};
use crate::symbols::SymbolSource;

/// Результат валидации выражения.
#[derive(Debug, Clone, Serialize)]
pub struct ExpressionValidation {
    pub valid: bool,
    pub errors: Vec<ExprError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExprError {
    pub line: u32,
    pub col: u32,
    pub kind: ExprErrorKind,
    pub message: String,
    /// Надёжность находки. Производна от `kind`, но дублируется в ответ явно,
    /// чтобы потребитель (особенно слабая модель) не зависел от внешних правил
    /// маппинга «kind → надёжность» (карточка-decision #1230).
    pub confidence: Confidence,
    /// Топ-1 ближайшая подсказка (если есть).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Список похожих значений (для перечислений / членов типа).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub similar: Vec<SimilarValue>,
}

impl ExprError {
    /// Сконструировать ошибку, проставив `confidence` из `kind` (единый источник
    /// истины — [`ExprErrorKind::confidence`]).
    fn new(
        line: u32,
        col: u32,
        kind: ExprErrorKind,
        message: String,
        suggestion: Option<String>,
        similar: Vec<SimilarValue>,
    ) -> Self {
        Self {
            line,
            col,
            kind,
            message,
            confidence: kind.confidence(),
            suggestion,
            similar,
        }
    }

    /// Сконструировать ошибку с явно заданным `confidence`. Нужно для случаев,
    /// когда `kind` не однозначно определяет надёжность — например,
    /// `UnknownGlobalMethod` и `UnknownDirective` эмиттятся с двухпороговым
    /// Confidence по fuzzy-расстоянию (High при сильном сходстве, Low при
    /// слабом), а не хардкодом от kind.
    pub(crate) fn new_with_confidence(
        line: u32,
        col: u32,
        kind: ExprErrorKind,
        message: String,
        confidence: Confidence,
        suggestion: Option<String>,
        similar: Vec<SimilarValue>,
    ) -> Self {
        Self {
            line,
            col,
            kind,
            message,
            confidence,
            suggestion,
            similar,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExprErrorKind {
    UnknownEnumValue,
    UnknownTypeMember,
    UnknownNewType,
    WrongArgumentCount,
    UnknownGlobalMethod,
    /// Вызов `Имя(...)`, которого нет ни среди объявлений присланного модуля,
    /// ни среди платформенных методов, и который ни на что не похож (fuzzy
    /// промолчал). Эмиттится только при проверке ЦЕЛОГО модуля — там отсутствие
    /// объявления означает описку либо забытую процедуру. На голом фрагменте
    /// такой вывод неправомерен: объявление просто осталось за кадром.
    UndeclaredMethod,
    /// Имя директивы (`&НаСервере`, `&Перед`, …) не входит в whitelist
    /// известных директив. Эмиттится только из `validate_module` при обходе
    /// `annotation`-узлов AST. Confidence проставляется явно по двухпороговой
    /// эвристике `fuzzy_confidence_for` (тот же механизм, что для
    /// `UnknownGlobalMethod`).
    UnknownDirective,
}

impl ExprErrorKind {
    /// Надёжность находки этого вида (fallback, если конструктор не задал явно).
    ///
    /// `High` (false-positive ≈ 0) — точная сверка с реальным индексом платформы:
    /// несуществующее значение перечисления и неверное число аргументов.
    ///
    /// `Low` (возможен false-positive) — зависит от эвристического type inference
    /// (Уровень 2) либо от полноты hbk: член типа, тип в `Новый`.
    ///
    /// `UnknownGlobalMethod` — Confidence проставляется НЕ через этот метод,
    /// а явно через [`ExprError::new_with_confidence`] по двухпороговой эвристике
    /// от `fuzzy_confidence_for`: High при сильном сходстве, Low при слабом.
    /// Хардкод здесь — только как safe fallback.
    pub fn confidence(self) -> Confidence {
        match self {
            ExprErrorKind::UnknownEnumValue
            | ExprErrorKind::WrongArgumentCount
            | ExprErrorKind::UndeclaredMethod => Confidence::High,
            ExprErrorKind::UnknownTypeMember
            | ExprErrorKind::UnknownNewType
            | ExprErrorKind::UnknownGlobalMethod
            | ExprErrorKind::UnknownDirective => Confidence::Low,
        }
    }
}

/// Уровень надёжности находки валидатора.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Точная сверка с индексом платформы, false-positive ≈ 0.
    High,
    /// Зависит от эвристики (type inference) или полноты hbk, возможен false-positive.
    Low,
}

/// Профиль потребителя валидатора (карточка-decision #1230).
///
/// Терпимость к ложным срабатываниям — свойство потребителя, а не валидатора.
/// Профиль выбирает, что вернуть конкретному клиенту.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Для слабых моделей (LibreChat/DeepSeek): форсирует `level=1` и возвращает
    /// только high-confidence находки. Ложное срабатывание клиенту не приходит —
    /// нечем зацикливаться.
    Strict,
    /// Для сильных моделей (десктопный Opus/Sonnet, дефолт): `level` из параметра/
    /// конфига, все находки — модель сама отбросит сомнительные.
    #[default]
    Full,
}

impl Profile {
    /// Толерантный парсинг строки от клиента. Неизвестное значение → дефолт (`Full`).
    pub fn parse_or_default(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("strict") => Profile::Strict,
            Some("full") => Profile::Full,
            _ => Profile::default(),
        }
    }
}

/// Главный API: проверить произвольный BSL-фрагмент. Дефолтный уровень — 1.
pub fn validate_expression(index: &PlatformIndex, source: &str) -> ExpressionValidation {
    validate_expression_at_level(index, source, 1)
}

/// Проверка с явным уровнем валидации.
///
/// - `level=1` — статический анализ ссылок с явным именем типа в исходнике
///   (TypeDotMember, NewExpression, GlobalCall). Дефолт.
/// - `level=2` — дополнительно локальный type inference в пределах процедуры
///   (Phase 8 MVP): переменные, выведенные из `Х = Новый ТипX`, `Х = ТипY.ЗначениеZ`
///   и аннотации `// @type ТипX`. У ложно-срабатываний больше — поэтому отдельный флаг.
/// - `level=3` — дополнительно return-type tracking (Уровень 2.5): тип переменной
///   выводится из возвращаемого типа метода/свойства, в т.ч. по цепочке
///   `Х = Запрос.Выполнить().Выбрать()`. Находки — те же `unknown_type_member`
///   (confidence Low). Интеграция с метаданными конфигурации — в server-слое.
pub fn validate_expression_at_level(
    index: &PlatformIndex,
    source: &str,
    level: u8,
) -> ExpressionValidation {
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
    check_type_dot_members(index, source, &facts.dots, scope_map.as_ref(), &mut errors);
    check_new_expressions(index, source, &facts.news, &mut errors);
    check_global_calls(index, source, &facts.calls, None, false, None, None, &mut errors);
    errors.sort_by_key(|e| (e.line, e.col));

    ExpressionValidation {
        valid: errors.is_empty(),
        errors,
    }
}

/// Проверка с учётом профиля потребителя (карточка-decision #1230).
///
/// - [`Profile::Full`] — `level` берётся как передан, возвращаются все находки.
/// - [`Profile::Strict`] — `level` форсируется в `1`, после прогона остаются
///   только high-confidence находки ([`Confidence::High`]); `valid` пересчитывается.
///   Слабому потребителю ложное срабатывание (low-confidence) физически не приходит.
pub fn validate_expression_with_profile(
    index: &PlatformIndex,
    source: &str,
    level: u8,
    profile: Profile,
) -> ExpressionValidation {
    let effective_level = if profile == Profile::Strict { 1 } else { level };
    let mut result = validate_expression_at_level(index, source, effective_level);

    if profile == Profile::Strict {
        result
            .errors
            .retain(|e| e.confidence == Confidence::High);
        result.valid = result.errors.is_empty();
    }

    result
}

// ── Очистка строк и комментариев ──────────────────────────────────────────
//
// Перенесены в крейт `bsl-parse` (нужны и внешнему индексатору кода),
// здесь — публичный реэкспорт для обратной совместимости: на них ссылается
// bench-код и `scope.rs`.
pub use bsl_parse::{mask_strings_and_comments, strip_extension_directives};

// ── Проверки ──────────────────────────────────────────────────────────────

pub(crate) fn check_type_dot_members(
    index: &PlatformIndex,
    src: &str,
    dots: &[DotFact],
    scope_map: Option<&ScopeMap>,
    errors: &mut Vec<ExprError>,
) {
    for dot in dots {
        let head = dot.head.as_str();
        let member = dot.member.as_str();

        // Уровень 1: head — это имя платформенного типа.
        // Уровень 2: head может быть локальной переменной с известным типом.
        let resolved_type_name: Option<String> = match index.find_type(head) {
            Some(_) => Some(head.to_string()),
            None => scope_map
                .and_then(|sm| sm.type_of_var(dot.head_byte, head))
                .cloned(),
        };
        let Some(type_name) = resolved_type_name else {
            continue; // head — обычная переменная без выведенного типа
        };
        let Some(ty) = index.find_type(&type_name) else {
            continue;
        };

        if ty.is_enum() {
            // Проверяем что member — одно из enum_values (ru/en).
            let m_lower = member.to_lowercase();
            let exists = ty
                .enum_values
                .iter()
                .any(|v| v.name_ru.to_lowercase() == m_lower || v.name_en.to_lowercase() == m_lower);
            if !exists {
                let (line, col) = pos_at(src, dot.member_byte);
                let allowed: Vec<String> =
                    ty.enum_values.iter().map(|v| v.name_ru.clone()).collect();
                let suggestion = closest_str(member, &allowed);
                errors.push(ExprError::new(
                    line,
                    col,
                    ExprErrorKind::UnknownEnumValue,
                    format!(
                        "Значение '{}' не существует у типа-перечисления '{}'.{}",
                        member,
                        ty.name_ru,
                        suggestion
                            .as_ref()
                            .map(|s| format!(" Возможно, вы имели в виду '{s}'."))
                            .unwrap_or_default()
                    ),
                    suggestion,
                    Vec::new(),
                ));
            }
        } else if is_dynamic_member_type(&ty.name_ru) {
            // Типы с динамическими членами: поля задаются в runtime (колонки
            // выборки запроса / таблицы значений / дерева, произвольные ключи
            // структуры) и в hbk отсутствуют. Проверка членов для них даёт
            // массовый false-positive (`Выборка.Регистратор`,
            // `СтрокаТЗ.ОбъектОплаты`). Пропускаем целиком — размен: не ловим
            // опечатку в статическом методе такого типа (`Выборка.Слндующий`),
            // зато не плодим FP на полях. Выявлено регресс-прогоном level=3
            // (карточка #1232 — урок про массовый FP).
            continue;
        } else {
            let m_lower = member.to_lowercase();
            let exists_method = ty
                .methods
                .iter()
                .any(|m| m.name_ru.to_lowercase() == m_lower || m.name_en.to_lowercase() == m_lower);
            let exists_prop = ty
                .properties
                .iter()
                .any(|p| p.name_ru.to_lowercase() == m_lower || p.name_en.to_lowercase() == m_lower);
            if !exists_method && !exists_prop {
                let (line, col) = pos_at(src, dot.member_byte);
                let mut allowed: Vec<String> =
                    ty.methods.iter().map(|m| m.name_ru.clone()).collect();
                allowed.extend(ty.properties.iter().map(|p| p.name_ru.clone()));
                let suggestion = closest_str(member, &allowed);
                errors.push(ExprError::new(
                    line,
                    col,
                    ExprErrorKind::UnknownTypeMember,
                    format!(
                        "У типа '{}' нет члена '{}'.{}",
                        ty.name_ru,
                        member,
                        suggestion
                            .as_ref()
                            .map(|s| format!(" Возможно: '{s}'."))
                            .unwrap_or_default()
                    ),
                    suggestion,
                    Vec::new(),
                ));
            }
        }
    }
}

/// Типы платформы, члены которых задаются в runtime, а не описаны в hbk:
/// колонки выборки запроса / таблицы значений / дерева значений, произвольные
/// ключи структуры. Для них проверка `Объект.Член` бессмысленна (массовый FP:
/// `Выборка.Регистратор`, `СтрокаТЗ.ОбъектОплаты`). На уровнях 1/2 такие типы
/// как `head` почти не встречаются; проблема всплывает на level=3, где
/// return-type tracking выводит их как тип переменной (`Выб = Рез.Выбрать()`).
fn is_dynamic_member_type(name_ru: &str) -> bool {
    matches!(
        name_ru.to_lowercase().as_str(),
        // Колонки выборок и строк коллекций задаются текстом запроса / составом ТЗ.
        "выборкаизрезультатазапроса"
            | "выборкаданных"
            | "строкатаблицызначений"
            | "строкадеревазначений"
            // Произвольные ключи.
            | "структура"
            | "фиксированнаяструктура"
            // Реквизиты и элементы конкретной формы — в метаданных формы, не в hbk.
            | "форма"
            | "управляемаяформа"
            | "элементыформы"
            // Свойства XDTO задаются схемой/пакетом в runtime.
            | "объектxdto"
            | "значениеxdto"
    )
}

pub(crate) fn check_new_expressions(
    index: &PlatformIndex,
    src: &str,
    news: &[NewFact],
    errors: &mut Vec<ExprError>,
) {
    for n in news {
        if index.find_type(&n.type_name).is_none() {
            let (line, col) = pos_at(src, n.byte);
            let all_types: Vec<String> = index.types.values().map(|t| t.name_ru.clone()).collect();
            let suggestion = closest_str(&n.type_name, &all_types);
            errors.push(ExprError::new(
                line,
                col,
                ExprErrorKind::UnknownNewType,
                format!(
                    "Тип '{}' не найден в платформенном контексте (Новый '{}').{}",
                    n.type_name,
                    n.type_name,
                    suggestion
                        .as_ref()
                        .map(|s| format!(" Возможно: '{s}'."))
                        .unwrap_or_default()
                ),
                suggestion,
                Vec::new(),
            ));
        }
    }
}

/// Проверка глобальных вызовов `Имя(args)`, извлечённых деревом
/// ([`crate::ast::collect_facts`]) в виде [`CallFact`]. `user_symbols` —
/// необязательный whitelist имён своих процедур/функций (в lowercase),
/// извлечённых из модуля вызывающим слоем (см. `module::validate_module_at_level`).
/// Если вызов попадает в whitelist — пропускаем без проверки. Для
/// `validate_expression` передаётся `None`.
///
/// `strict_unknown` включает СТРОГИЙ режим: вызов, которого нет ни в whitelist,
/// ни в платформе, и который ни на что не похож, эмиттится как
/// `UndeclaredMethod`. Правомерен только для ЦЕЛОГО модуля, и только если это
/// не модуль расширения (там половина имён приходит из расширяемого модуля,
/// текста которого у валидатора нет).
///
/// `symbols` — необязательный внешний источник имён (см.
/// [`crate::symbols::SymbolSource`]): методы других модулей конфигурации.
/// Используется ТОЛЬКО внутри `strict_unknown`, чтобы закрыть два случая
/// false-positive — экспорт глобального общего модуля и метод модуля
/// объекта-владельца (`owner_exports`, предзагруженный набор lowercase-имён).
pub(crate) fn check_global_calls(
    index: &PlatformIndex,
    src: &str,
    calls: &[CallFact],
    user_symbols: Option<&HashSet<String>>,
    strict_unknown: bool,
    symbols: Option<&dyn SymbolSource>,
    owner_exports: Option<&HashSet<String>>,
    errors: &mut Vec<ExprError>,
) {
    // Методы собственного объекта/формы/менеджера зовутся из его модуля без
    // префикса (`Закрыть()`, `ЭтоНовый()`, `ПустаяСсылка()`). Кэш в индексе —
    // считается один раз на процесс.
    let type_methods = index.all_type_method_names();

    for call in calls {
        // Своя процедура/функция из этого же модуля — пропускаем.
        if let Some(whitelist) = user_symbols {
            if whitelist.contains(&call.name.to_lowercase()) {
                continue;
            }
        }

        // Если имя — известный глобальный метод, попытаемся посчитать аргументы.
        let Some(_method) = index.find_global_method(&call.name) else {
            // Метод платформенного типа, вызванный без префикса из собственного
            // модуля. Это ни неизвестный глобальный метод, ни описка: fuzzy тут
            // выдавал уверенную чушь (`ПустаяСсылка()` в модуле менеджера →
            // «возможно, вы имели в виду ПустаяСтрока», 343 находки на УТ).
            if type_methods.contains(&call.name.to_lowercase()) {
                continue;
            }
            // Неизвестный глобальный вызов: пробуем fuzzy к платформенным.
            // Строгого совпадения нет — либо это опечатка платформенного метода,
            // либо процедура общего модуля/БСП (валидатор её не видит). Различаем
            // по расстоянию: сильное сходство → High (уверенно опечатка), слабое →
            // Low (возможная опечатка), далёкое → молча пропускаем.
            let fuzzy = closest_global_method_with_distance(index, &call.name)
                .and_then(|(s, d)| fuzzy_confidence_for(&call.name, &s, d).map(|c| (s, c)));
            if let Some((suggestion, confidence)) = fuzzy {
                let (line, col) = pos_at(src, call.byte);
                errors.push(ExprError::new_with_confidence(
                    line,
                    col,
                    ExprErrorKind::UnknownGlobalMethod,
                    format!(
                        "Глобальный метод '{}' не найден в платформенном контексте. \
                         Возможно, вы имели в виду '{}'.",
                        call.name, suggestion
                    ),
                    confidence,
                    Some(suggestion),
                    Vec::new(),
                ));
            } else if strict_unknown {
                // Целый модуль: вызов не объявлен здесь, не платформенный, не метод
                // какого-либо платформенного типа (отсечено выше) и ни на что не
                // похож. Процедуры общих модулей вызываются через точку и сюда не
                // попадают, поэтому остаётся описка либо забытое объявление.
                // Исключение — глобальные общие модули (флаг «Глобальный»): их
                // процедуры зовутся без префикса, валидатор их не видит и даст
                // здесь false-positive. Внешний источник имён (`symbols`) закрывает
                // этот случай и ещё один — метод модуля объекта-владельца
                // (`owner_exports`) для модуля обычной формы внешней обработки.
                let lc = call.name.to_lowercase();
                let is_owner_export = owner_exports.map(|s| s.contains(&lc)).unwrap_or(false);
                let is_global_export = symbols.map(|s| s.is_global_export(&lc)).unwrap_or(false);
                if is_owner_export || is_global_export {
                    // Не описка: метод виден отсюда через внешний источник.
                } else if symbols.map(|s| s.method_exists(&lc)).unwrap_or(false) {
                    // Имя объявлено где-то в конфигурации, но отсюда может быть
                    // не видно по правилам видимости — находка остаётся, но
                    // с пониженной уверенностью.
                    let (line, col) = pos_at(src, call.byte);
                    errors.push(ExprError::new_with_confidence(
                        line,
                        col,
                        ExprErrorKind::UndeclaredMethod,
                        format!(
                            "Метод '{}' не объявлен в этом модуле. В конфигурации он есть, \
                             но отсюда может быть не виден — проверьте правила видимости.",
                            call.name
                        ),
                        Confidence::Low,
                        None,
                        Vec::new(),
                    ));
                } else {
                    let (line, col) = pos_at(src, call.byte);
                    errors.push(ExprError::new_with_confidence(
                        line,
                        col,
                        ExprErrorKind::UndeclaredMethod,
                        format!(
                            "Метод '{}' не объявлен в этом модуле и не найден в платформенном контексте.",
                            call.name
                        ),
                        Confidence::High,
                        None,
                        Vec::new(),
                    ));
                }
            }
            continue;
        };

        let result = validate_method_call(index, &call.name, call.arg_count);
        if !result.valid {
            let (line, col) = pos_at(src, call.byte);
            errors.push(ExprError::new(
                line,
                col,
                ExprErrorKind::WrongArgumentCount,
                result.message,
                None,
                Vec::new(),
            ));
        }
    }
}

// ── Вспомогательные ──────────────────────────────────────────────────────

fn pos_at(src: &str, byte_idx: usize) -> (u32, u32) {
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for (i, ch) in src.char_indices() {
        if i >= byte_idx {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn closest_str(target: &str, candidates: &[String]) -> Option<String> {
    let target_l = target.to_lowercase();
    candidates
        .iter()
        .map(|c| (similarity(&target_l, &c.to_lowercase()), c.clone()))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .filter(|(s, _)| *s > 0.5)
        .map(|(_, c)| c)
}

/// Ближайший глобальный метод к `target` по обоим языкам (name_ru + name_en).
/// Возвращает (имя-победитель, distance). Distance — расстояние Левенштейна
/// на lowercase формах.
///
/// Используется в `check_global_calls`, когда прямой lookup `find_global_method`
/// вернул None: нужен не только suggestion, но и distance, чтобы выбрать
/// Confidence по двухпороговой эвристике [`fuzzy_confidence_for`].
///
/// distance==0 в паре с промахом `find_global_method` означает case-mismatch
/// (сам find регистронезависим, но при истинном совпадении мы бы не оказались
/// в этой ветке; distance=0 возможен только если у нас не хватило нормализации
/// на входе). Здесь возвращаем как есть — вызывающий сам решит, эмиттить ли.
fn closest_global_method_with_distance(
    index: &PlatformIndex,
    target: &str,
) -> Option<(String, usize)> {
    let target_lc = target.to_lowercase();
    let mut best: Option<(String, usize)> = None;
    for m in &index.global_methods {
        let d_ru = lev(&target_lc, &m.name_ru.to_lowercase());
        match &best {
            Some((_, d)) if d_ru >= *d => {}
            _ => best = Some((m.name_ru.clone(), d_ru)),
        }
        if !m.name_en.is_empty() {
            let d_en = lev(&target_lc, &m.name_en.to_lowercase());
            match &best {
                Some((_, d)) if d_en >= *d => {}
                _ => best = Some((m.name_en.clone(), d_en)),
            }
        }
    }
    best
}

/// Двухпороговая эвристика Confidence по длине идентификатора и расстоянию
/// Левенштейна. Возвращает None — значит эмиттить находку не надо.
///
/// - Сильное сходство (High): distance ≤ 2 при len ≥ 5, либо distance ≤ 1 при len < 5.
/// - Слабое сходство (Low): distance ≤ 3 при len ≥ 6.
/// - Иначе: None.
///
/// Пороги подобраны так, чтобы длинные имена (типа `СтрНайти`, 8 символов)
/// с 1-2 опечатками ловились уверенно, а короткие имена (типа `Мин`, 3 символа)
/// требовали distance ≤ 1 — иначе `Мин` fuzzy к `Макс` даст ложный High.
///
/// Два отсекателя перед порогами:
/// 1. `distance == 0` — совпадение точное, находку эмиттить бессмысленно
///    (сообщение «X не найден, возможно вы имели в виду X»). Защита-дублёр:
///    после расширения `find_global_method` на `name_en` такой случай не
///    должен возникать, но молча выйти дешевле, чем врать.
/// 2. `suggestion` — строгое начало `head`, а хвост это цифры либо 2+ символа.
///    Это осознанно другое, более длинное имя (`СтрокаТЧ` = `Строка` + `ТЧ`,
///    `Сообщить2` = `Сообщить` + `2`), а не опечатка. Опечатка приписыванием
///    одной буквы (`Строкаа`) под правило не попадает и по-прежнему ловится.
/// 3. Симметрично: `suggestion` — строгий конец `head`, а приставка это цифры,
///    2+ символа (`тзСтрока`, `ТЗСтрока`) либо одна строчная буква перед
///    заглавной (`тФормат` — венгерская нотация). Удвоение первой буквы
///    (`ССообщить`, `ФФормат`) под правило не попадает и по-прежнему ловится.
pub(crate) fn fuzzy_confidence_for(
    head: &str,
    suggestion: &str,
    distance: usize,
) -> Option<Confidence> {
    if distance == 0 {
        return None;
    }
    if let Some(suffix) = strip_prefix_ci(head, suggestion) {
        let all_digits = !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
        if all_digits || suffix.chars().count() >= 2 {
            return None;
        }
    }
    if let Some(prefix) = strip_suffix_ci(head, suggestion) {
        let all_digits = !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit());
        if all_digits || prefix.chars().count() >= 2 {
            return None;
        }
        // Приставка ровно из одного символа. Строчная буква перед заглавной —
        // венгерская нотация (`тФормат`), а не опечатка. Заглавная приставка —
        // удвоение первой буквы (`ССообщить`), её по-прежнему ловим.
        let starts_lowercase = prefix.chars().next().is_some_and(char::is_lowercase);
        let next_is_uppercase = head.chars().nth(1).is_some_and(char::is_uppercase);
        if starts_lowercase && next_is_uppercase {
            return None;
        }
    }
    let len = head.chars().count();
    let strong = (len >= 5 && distance <= 2) || (len < 5 && distance <= 1);
    let weak = len >= 6 && distance <= 3;
    if strong {
        Some(Confidence::High)
    } else if weak {
        Some(Confidence::Low)
    } else {
        None
    }
}

/// Если `prefix` — строгое начало `head` (регистронезависимо), вернуть остаток.
/// Равные строки дают `None`: остатка нет, это не «имя с суффиксом».
fn strip_prefix_ci(head: &str, prefix: &str) -> Option<String> {
    let head_lc = head.to_lowercase();
    let prefix_lc = prefix.to_lowercase();
    if prefix_lc.is_empty() || head_lc == prefix_lc {
        return None;
    }
    head_lc.strip_prefix(&prefix_lc).map(|s| s.to_string())
}

/// Если `suffix` — строгий конец `head` (регистронезависимо), вернуть приставку
/// в ИСХОДНОМ регистре: вызывающему нужно отличить `тФормат` от `ФФормат`.
/// Равные строки дают `None`: приставки нет, это не «имя с приставкой».
fn strip_suffix_ci(head: &str, suffix: &str) -> Option<String> {
    let head_lc = head.to_lowercase();
    let suffix_lc = suffix.to_lowercase();
    if suffix_lc.is_empty() || head_lc == suffix_lc {
        return None;
    }
    head_lc.strip_suffix(&suffix_lc)?;
    let prefix_len = head.chars().count().checked_sub(suffix.chars().count())?;
    Some(head.chars().take(prefix_len).collect())
}

fn similarity(a: &str, b: &str) -> f32 {
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (lev(a, b) as f32 / max_len as f32)
}

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
    fn nstr_with_string_arg_is_valid() {
        // End-to-end: НСтр("ru = '...'") не должен давать wrong_argument_count.
        use platform_index::{Method, Parameter, PlatformIndex, Signature};
        let mut index = PlatformIndex::new();
        index.global_methods.push(Method {
            name_ru: "НСтр".into(),
            name_en: "NStr".into(),
            description: String::new(),
            return_type: "Строка".into(),
            signatures: vec![Signature {
                name: "Основная".into(),
                description: String::new(),
                parameters: vec![
                    Parameter { name: "ИсходнаяСтрока".into(), type_name: String::new(), required: true, description: String::new() },
                    Parameter { name: "КодЯзыка".into(), type_name: String::new(), required: false, description: String::new() },
                ],
            }],
        });
        let src = "Текст = НСтр(\"ru = 'Неверный тип запроса.'\");";
        let res = validate_expression_at_level(&index, src, 1);
        assert!(res.valid, "НСтр с одним строковым аргументом ложно помечен: {:?}", res.errors);
    }

    // ── fuzzy_confidence_for: дефект хотфикса 0.5.1 ─────────────────────────

    /// ВРЕМЕННЫЙ: где маскировка пропускает слова языка запросов.
    #[test]
    fn fuzzy_zero_distance_is_silent() {
        assert_eq!(fuzzy_confidence_for("Сообщить", "Сообщить", 0), None);
    }

    #[test]
    fn fuzzy_deliberate_suffix_is_not_a_typo() {
        // Кандидат — строгое начало имени, хвост осмысленный или цифровой.
        assert_eq!(fuzzy_confidence_for("СтрокаТЧ", "Строка", 2), None);
        assert_eq!(fuzzy_confidence_for("Сообщить2", "Сообщить", 1), None);
        assert_eq!(fuzzy_confidence_for("СокрЛ2", "СокрЛ", 1), None);
        assert_eq!(fuzzy_confidence_for("Формат1", "Формат", 1), None);
    }

    #[test]
    fn fuzzy_real_typo_still_high() {
        // «СтрНайит» — перестановка букв, suggestion НЕ является началом head.
        assert_eq!(
            fuzzy_confidence_for("СтрНайит", "СтрНайти", 2),
            Some(Confidence::High)
        );
    }

    #[test]
    fn fuzzy_single_letter_doubling_still_caught() {
        // Приписана одна буква — это правдоподобная опечатка, не суффикс.
        assert_eq!(
            fuzzy_confidence_for("Строкаа", "Строка", 1),
            Some(Confidence::High)
        );
    }

    #[test]
    fn fuzzy_deliberate_prefix_is_not_a_typo() {
        // Кандидат — строгий конец имени: венгерская нотация, не опечатка.
        assert_eq!(fuzzy_confidence_for("тФормат", "Формат", 1), None);
        assert_eq!(fuzzy_confidence_for("тзСтрока", "Строка", 2), None);
        assert_eq!(fuzzy_confidence_for("ТЗСтрока", "Строка", 2), None);
        assert_eq!(fuzzy_confidence_for("1Формат", "Формат", 1), None);
    }

    #[test]
    fn fuzzy_first_letter_doubling_still_caught() {
        // Удвоена первая буква — приставка заглавная, это опечатка.
        assert_eq!(
            fuzzy_confidence_for("ССообщить", "Сообщить", 1),
            Some(Confidence::High)
        );
        assert_eq!(
            fuzzy_confidence_for("ФФормат", "Формат", 1),
            Some(Confidence::High)
        );
    }

    // ── Профиль потребителя и надёжность (карточка #1230) ──────────────────

    #[test]
    fn confidence_mapping() {
        assert_eq!(ExprErrorKind::UnknownEnumValue.confidence(), Confidence::High);
        assert_eq!(
            ExprErrorKind::WrongArgumentCount.confidence(),
            Confidence::High
        );
        assert_eq!(ExprErrorKind::UnknownTypeMember.confidence(), Confidence::Low);
        assert_eq!(ExprErrorKind::UnknownNewType.confidence(), Confidence::Low);
        assert_eq!(
            ExprErrorKind::UnknownGlobalMethod.confidence(),
            Confidence::Low
        );
    }

    #[test]
    fn profile_parse_or_default() {
        assert_eq!(Profile::parse_or_default(Some("strict")), Profile::Strict);
        assert_eq!(Profile::parse_or_default(Some("  STRICT ")), Profile::Strict);
        assert_eq!(Profile::parse_or_default(Some("full")), Profile::Full);
        assert_eq!(Profile::parse_or_default(Some("чтотоиное")), Profile::Full);
        assert_eq!(Profile::parse_or_default(None), Profile::Full);
        // Дефолт enum — Full.
        assert_eq!(Profile::default(), Profile::Full);
    }

    /// Минимальный индекс: одно перечисление (`ЦветТест`) и один обычный тип
    /// (`СтруктураТест` с единственным методом `Вставить`). Достаточно, чтобы
    /// получить high-confidence (несуществующее значение перечисления) и
    /// low-confidence (несуществующий член типа) находки.
    fn test_index() -> PlatformIndex {
        use platform_index::{EnumValue, Method, Type};

        let mut index = PlatformIndex::new();

        index.insert_type(Type {
            name_ru: "ЦветТест".into(),
            name_en: "ColorTest".into(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![EnumValue {
                name_ru: "Красный".into(),
                name_en: "Red".into(),
                description: String::new(),
            }],
        });

        index.insert_type(Type {
            name_ru: "СтруктураТест".into(),
            name_en: "StructTest".into(),
            description: String::new(),
            methods: vec![Method {
                name_ru: "Вставить".into(),
                name_en: "Insert".into(),
                description: String::new(),
                return_type: String::new(),
                signatures: Vec::new(),
            }],
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: Vec::new(),
        });

        index
    }

    #[test]
    fn profile_full_returns_all_findings() {
        let index = test_index();
        // Первая строка — high (значение перечисления), вторая — low (член типа).
        let src = "А = ЦветТест.Синий;\nБ = СтруктураТест.Опечатка;";
        let result = validate_expression_with_profile(&index, src, 1, Profile::Full);

        assert!(!result.valid);
        assert_eq!(result.errors.len(), 2, "full должен вернуть обе находки");
        assert!(result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownEnumValue
                && e.confidence == Confidence::High));
        assert!(result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownTypeMember
                && e.confidence == Confidence::Low));
    }

    #[test]
    fn profile_strict_keeps_only_high_confidence() {
        let index = test_index();
        let src = "А = ЦветТест.Синий;\nБ = СтруктураТест.Опечатка;";
        let result = validate_expression_with_profile(&index, src, 2, Profile::Strict);

        assert!(!result.valid);
        assert_eq!(
            result.errors.len(),
            1,
            "strict должен оставить только high-confidence находку"
        );
        assert_eq!(result.errors[0].kind, ExprErrorKind::UnknownEnumValue);
        assert_eq!(result.errors[0].confidence, Confidence::High);
    }
}
