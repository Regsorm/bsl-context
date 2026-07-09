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

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

use platform_index::PlatformIndex;

use crate::check::{validate_method_call, SimilarValue};
use crate::scope::{extract_scope_map, extract_type_annotations, ScopeMap};

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
            ExprErrorKind::UnknownEnumValue | ExprErrorKind::WrongArgumentCount => Confidence::High,
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
    let cleaned = mask_strings_and_comments(source);
    let scope_map = if level >= 2 {
        let annotations = extract_type_annotations(source);
        Some(extract_scope_map(index, &cleaned, &annotations, level))
    } else {
        None
    };

    let mut errors = Vec::new();
    check_type_dot_members(index, &cleaned, scope_map.as_ref(), &mut errors);
    check_new_expressions(index, &cleaned, &mut errors);
    check_global_calls(index, &cleaned, None, &mut errors);

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

/// Замаскировать пробелами строковые литералы и комментарии. Длина и позиции
/// строк сохраняются — это важно для line/col, передаваемых в ошибки. Русские
/// буквы и прочие multi-byte UTF-8 символы НЕ трогаются — пробелами заменяются
/// только байты внутри строк/комментариев (ASCII содержимое).
pub fn mask_strings_and_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Однострочный комментарий //...
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' {
                if bytes[j] != b'\r' {
                    out[j] = b' ';
                }
                j += 1;
            }
            i = j;
            continue;
        }
        // Строка "..."
        if b == b'"' {
            out[i] = b' '; // открывающая кавычка
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'"' {
                    if j + 1 < bytes.len() && bytes[j + 1] == b'"' {
                        // escaped quote — затираем обе и идём дальше
                        out[j] = b' ';
                        out[j + 1] = b' ';
                        j += 2;
                        continue;
                    }
                    out[j] = b' ';
                    j += 1;
                    break;
                }
                if bytes[j] != b'\n' && bytes[j] != b'\r' {
                    out[j] = b' ';
                }
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }

    String::from_utf8(out).expect("mask_strings_and_comments сохраняет UTF-8 валидность")
}

// ── Регэксы (cached) ──────────────────────────────────────────────────────

fn type_dot_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Слева НЕ должно быть точки или идентификатора (граница), а после
        // первого Идентификатора.Идентификатора может быть ещё точка — это
        // Уровень 2/3, мы его не валидируем.
        Regex::new(r"(?P<head>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)\.(?P<member>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)").unwrap()
    })
}

fn new_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `Новый ИмяТипа` (с опциональной скобкой после, которую захватываем для
        // подсчёта аргументов в Phase 6.5+).
        Regex::new(r"(?i)(?:^|[^A-Za-zА-Яа-яЁё_0-9])(Новый|New)\s+(?P<ty>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)").unwrap()
    })
}

fn global_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `Идентификатор(` где идентификатор не предшествует точке/идентификатору.
        // Используем lookbehind через alt (regex crate не поддерживает look-around),
        // поэтому ловим контекст в группе `head` и проверяем post-hoc.
        Regex::new(r"(?P<head>[A-Za-zА-Яа-яЁё_][A-Za-zА-Яа-яЁё_0-9]*)\s*\(").unwrap()
    })
}

// ── Проверки ──────────────────────────────────────────────────────────────

pub(crate) fn check_type_dot_members(
    index: &PlatformIndex,
    src: &str,
    scope_map: Option<&ScopeMap>,
    errors: &mut Vec<ExprError>,
) {
    for cap in type_dot_re().captures_iter(src) {
        let head_match = cap.name("head").unwrap();
        let member_match = cap.name("member").unwrap();
        let head = head_match.as_str();
        let member = member_match.as_str();

        // Слева НЕ должна быть «.» (это значит, что head — тоже член) или другой
        // буквенный символ (значит, head попал в середину имени).
        let prev_idx = head_match.start();
        if prev_idx > 0 {
            let prev_byte = src.as_bytes()[prev_idx - 1];
            // Проверка: предыдущий символ не должен быть «.» или идентификаторным.
            if prev_byte == b'.' {
                continue;
            }
            // Многобайтные русские символы — обработаем через char-итератор.
            if !is_safe_left_boundary(src, prev_idx) {
                continue;
            }
        }

        // Уровень 1: head — это имя платформенного типа.
        // Уровень 2: head может быть локальной переменной с известным типом.
        let resolved_type_name: Option<String> = match index.find_type(head) {
            Some(_) => Some(head.to_string()),
            None => scope_map
                .and_then(|sm| sm.type_of_var(head_match.start(), head))
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
                let (line, col) = pos_at(src, member_match.start());
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
                let (line, col) = pos_at(src, member_match.start());
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

pub(crate) fn check_new_expressions(index: &PlatformIndex, src: &str, errors: &mut Vec<ExprError>) {
    for cap in new_re().captures_iter(src) {
        let ty_match = cap.name("ty").unwrap();
        let ty_name = ty_match.as_str();
        if index.find_type(ty_name).is_none() {
            let (line, col) = pos_at(src, ty_match.start());
            let all_types: Vec<String> = index.types.values().map(|t| t.name_ru.clone()).collect();
            let suggestion = closest_str(ty_name, &all_types);
            errors.push(ExprError::new(
                line,
                col,
                ExprErrorKind::UnknownNewType,
                format!(
                    "Тип '{}' не найден в платформенном контексте (Новый '{}').{}",
                    ty_name,
                    ty_name,
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

/// Проверка глобальных вызовов `Имя(args)` в тексте (уже замаскированном от
/// строк/комментариев). `user_symbols` — необязательный whitelist имён своих
/// процедур/функций (в lowercase), извлечённых из модуля вызывающим слоем
/// (см. `module::validate_module_impl`). Если вызов попадает в whitelist —
/// пропускаем без проверки. Для `validate_expression` передаётся `None`.
pub(crate) fn check_global_calls(
    index: &PlatformIndex,
    src: &str,
    user_symbols: Option<&HashSet<String>>,
    errors: &mut Vec<ExprError>,
) {
    for cap in global_call_re().captures_iter(src) {
        let head_match = cap.name("head").unwrap();
        let head = head_match.as_str();

        // Левый сосед — не должна быть «.» (тогда это вызов члена, не глобальный).
        // Также «&» слева означает, что это annotation-директива
        // (`&Перед("Foo")`, `&НаСервере` без скобок сюда не попадает вовсе),
        // а не глобальный вызов — валидация директив идёт отдельным путём через
        // `module::walk_module_ast`, здесь мы её молча пропускаем.
        let start = head_match.start();
        if start > 0 {
            let prev = src.as_bytes()[start - 1];
            if prev == b'.' || prev == b'&' {
                continue;
            }
            if !is_safe_left_boundary(src, start) {
                continue;
            }
        }
        // Игнорируем ключевые слова, не являющиеся вызовами.
        if is_bsl_keyword(head) {
            continue;
        }
        // Своя процедура/функция из этого же модуля — пропускаем.
        if let Some(whitelist) = user_symbols {
            if whitelist.contains(&head.to_lowercase()) {
                continue;
            }
        }
        // Имена, коллизирующие с приведением типа в языке запросов
        // (`ВЫРАЗИТЬ(X КАК ЧИСЛО(28,10))`): число «аргументов» в тексте запроса
        // не имеет отношения к BSL-функции Число() — массовый false-positive.
        // Проверку числа аргументов для них не делаем (baseline 2026-05-21,
        // карточка #1232). Глубокий фикс через маскировку оказался нестабильным
        // (откат в v0.3.5), поэтому точечно глушим коллизию.
        if is_query_cast_collision(head) {
            continue;
        }

        // Если head — известный глобальный метод, попытаемся посчитать аргументы.
        let Some(_method) = index.find_global_method(head) else {
            // Неизвестный глобальный вызов: пробуем fuzzy к платформенным.
            // Строгого совпадения нет — либо это опечатка платформенного метода,
            // либо процедура общего модуля/БСП (валидатор её не видит). Различаем
            // по расстоянию: сильное сходство → High (уверенно опечатка), слабое →
            // Low (возможная опечатка), далёкое → молча пропускаем.
            if let Some((suggestion, distance)) =
                closest_global_method_with_distance(index, head)
            {
                if let Some(confidence) = fuzzy_confidence_for(head, &suggestion, distance) {
                    let (line, col) = pos_at(src, head_match.start());
                    errors.push(ExprError::new_with_confidence(
                        line,
                        col,
                        ExprErrorKind::UnknownGlobalMethod,
                        format!(
                            "Глобальный метод '{}' не найден в платформенном контексте. \
                             Возможно, вы имели в виду '{}'.",
                            head, suggestion
                        ),
                        confidence,
                        Some(suggestion),
                        Vec::new(),
                    ));
                }
            }
            continue;
        };

        // Найти конец вызова (скобка после head). Группа уже захватила `(`,
        // надо отыскать парную закрывающую с учётом вложенности.
        let paren_start = head_match.end();
        // Сдвинуться к открывающей скобке (regex захватил `\s*\(`).
        let bytes = src.as_bytes();
        let mut k = paren_start;
        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= bytes.len() || bytes[k] != b'(' {
            continue;
        }
        let open = k;
        let close = match find_matching_paren(bytes, open) {
            Some(c) => c,
            None => continue,
        };
        let arg_count = count_top_level_args(&src[open + 1..close]);
        let result = validate_method_call(index, head, arg_count);
        if !result.valid {
            let (line, col) = pos_at(src, head_match.start());
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

/// Имена, коллизирующие с приведением типа в языке запросов
/// (`ВЫРАЗИТЬ(... КАК ЧИСЛО(n,m))`). Для них проверка числа аргументов как
/// BSL-функции не делается — иначе массовый false-positive из текстов запросов,
/// которые не всегда полностью замаскированы (1С-врезки `//` в многострочных
/// литералах ломают парность кавычек). Карточка #1232.
fn is_query_cast_collision(name: &str) -> bool {
    matches!(name.to_lowercase().as_str(), "число" | "number")
}

fn is_bsl_keyword(s: &str) -> bool {
    // Минимальный список ключевых слов BSL, которые могут стоять перед `(`
    // (например, `Если(...)` — нет, `Если` без скобок). На всякий случай —
    // основные операторы.
    matches!(
        s.to_lowercase().as_str(),
        "если"
            | "тогда"
            | "иначе"
            | "иначеесли"
            | "конецесли"
            | "цикл"
            | "конеццикла"
            | "процедура"
            | "конецпроцедуры"
            | "функция"
            | "конецфункции"
            | "возврат"
            | "и"
            | "или"
            | "не"
            | "истина"
            | "ложь"
            | "новый"
            | "if"
            | "then"
            | "else"
            | "elsif"
            | "endif"
            | "while"
            | "for"
            | "each"
            | "do"
            | "enddo"
            | "procedure"
            | "endprocedure"
            | "function"
            | "endfunction"
            | "return"
            | "and"
            | "or"
            | "not"
            | "true"
            | "false"
            | "new"
    )
}

/// Левый соседний символ должен быть НЕ идентификаторным
/// (буква/цифра/подчёркивание). Учитываем многобайтные русские буквы.
fn is_safe_left_boundary(src: &str, byte_idx: usize) -> bool {
    if byte_idx == 0 {
        return true;
    }
    // Найти ближайший char до byte_idx.
    let prefix = &src[..byte_idx];
    let last = prefix.chars().next_back();
    match last {
        Some(c) if c.is_alphanumeric() || c == '_' => false,
        _ => true,
    }
}

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

fn find_matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    debug_assert!(bytes[open] == b'(');
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn count_top_level_args(args_text: &str) -> usize {
    // Реально пустые скобки `Метод()` — аргументов нет.
    if args_text.is_empty() {
        return 0;
    }
    // Между скобками только пробелы. Главный случай — единственный аргумент,
    // целиком стёртый маскировкой строки/комментария: `НСтр("ru = '...'")`
    // после mask_strings_and_comments превращается в `НСтр(           )`.
    // Раньше тут возвращался 0 → массовый false-positive «не принимает 0
    // аргументов» на НСтр/Тип/ПредопределенноеЗначение и т.п. (baseline
    // 2026-05-21: ~918k срабатываний wrong_argument_count, из них ~560k — НСтр).
    // Реальный вызов без аргументов почти всегда пишут как `Метод()` без
    // пробела (его поймала проверка `is_empty` выше), поэтому «одни пробелы
    // при ненулевой длине» трактуем как 1 аргумент.
    let trimmed = args_text.trim();
    if trimmed.is_empty() {
        return 1;
    }
    let mut depth = 0i32;
    let mut commas = 0usize;
    for b in args_text.bytes() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    commas + 1
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
    fn mask_keeps_positions() {
        let src = "Если А = \"строка\" Тогда";
        let masked = mask_strings_and_comments(src);
        assert_eq!(masked.len(), src.len());
        // Текст вне строки сохранился.
        assert!(masked.contains("Если А ="));
        // Содержимое строки замаскировано.
        assert!(!masked.contains("строка"));
    }

    #[test]
    fn mask_handles_comment_to_eol() {
        let src = "А = 1; // комментарий\nБ = 2;";
        let masked = mask_strings_and_comments(src);
        assert!(masked.contains("А = 1;"));
        assert!(!masked.contains("комментарий"));
        assert!(masked.contains("Б = 2;"));
    }

    #[test]
    fn chislo_call_with_two_args_skipped() {
        // `Число(28,10)` коллизирует с приведением типа в языке запросов
        // (ВЫРАЗИТЬ(X КАК ЧИСЛО(28,10))). Проверка числа аргументов для Число
        // отключена → не должно быть wrong_argument_count даже при 2 аргументах.
        use platform_index::{Method, Parameter, PlatformIndex, Signature};
        let mut index = PlatformIndex::new();
        index.global_methods.push(Method {
            name_ru: "Число".into(),
            name_en: "Number".into(),
            description: String::new(),
            return_type: "Число".into(),
            signatures: vec![Signature {
                name: "Основная".into(),
                description: String::new(),
                parameters: vec![Parameter {
                    name: "Значение".into(),
                    type_name: String::new(),
                    required: true,
                    description: String::new(),
                }],
            }],
        });
        let res = validate_expression_at_level(&index, "X = Число(28, 10);", 1);
        assert!(res.valid, "Число(28,10) не должно ловиться: {:?}", res.errors);
    }

    #[test]
    fn count_args_simple() {
        assert_eq!(count_top_level_args(""), 0); // Метод() — без аргументов
        assert_eq!(count_top_level_args("а"), 1);
        assert_eq!(count_top_level_args("а, б, в"), 3);
        assert_eq!(count_top_level_args("а, Функция(б, в), г"), 3);
    }

    #[test]
    fn count_args_masked_string_is_one() {
        // После mask_strings_and_comments единственный аргумент-строка
        // превращается в пробелы — это 1 аргумент, не 0 (фикс false-positive
        // на НСтр/Тип/ПредопределенноеЗначение, baseline 2026-05-21).
        assert_eq!(count_top_level_args("            "), 1);
        assert_eq!(count_top_level_args("      ,    "), 2); // НСтр("...","ru")
    }

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
