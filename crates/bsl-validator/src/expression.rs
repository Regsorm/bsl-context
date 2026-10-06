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

use platform_index::{PlatformIndex, Type};

use bsl_parse::{collect_facts, CallFact, DotFact, NewFact};

use crate::check::{validate_method_call, SimilarValue};
use crate::locals::LocalNames;
use crate::scope::{extract_scope_map, extract_type_annotations, ScopeMap};
use crate::symbols::SymbolSource;

/// Результат валидации выражения.
#[derive(Debug, Clone, Serialize)]
pub struct ExpressionValidation {
    pub valid: bool,
    /// Удалось ли разобрать текст в дерево. `false` — все проверки поверх
    /// дерева (типы, вызовы, обращения к объектам конфигурации) не выполнялись:
    /// двоичный файл, сбой языка, истёкший дедлайн разбора. Текстовые проверки
    /// (объявления, структура модуля, директивы, правила запросов) при этом
    /// отрабатывают, поэтому находки быть могут.
    ///
    /// Поле обязано быть в ответе: без него `valid: true` на неразобранном
    /// модуле читается как «замечаний нет», хотя проверка не проводилась.
    pub tree_parsed: bool,
    /// Были ли доступны имена конфигурации (внешний источник). `false` —
    /// проверка выполнена только против платформенного контекста: находки по
    /// платформе (`UnknownGlobalMethod`, число аргументов, члены типов,
    /// системные перечисления, синтаксис) достоверны, а всё, что опирается на
    /// имена ПРИКЛАДНЫХ объектов, проверено не было.
    ///
    /// Признак нужен машиночитаемым: без него частичная проверка неотличима от
    /// полной, и пустой список находок читается как «замечаний нет».
    pub symbols_available: bool,
    /// Почему проверка неполная. Заполняется только при `symbols_available:
    /// false` и объясняет причину человеку — разбирать текст программно не
    /// нужно, для этого есть само поле `symbols_available`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
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
    /// Имя локальной переменной совпало с членом контекста модуля: свойством
    /// глобального контекста (`PlatformIndex.global_properties`) либо — в модуле
    /// формы — методом/свойством типа `ФормаКлиентскогоПриложения`. Присваивание
    /// такому имени не создаёт переменную, а падает в рантайме («Поле объекта
    /// недоступно для записи») либо молча меняет свойство платформы. Эмиттится
    /// только из `validate_module` (`crate::context_names`). Confidence
    /// проставляется явно: метод или read-only свойство → High, свойство,
    /// доступное для записи, → Low (присваивание компилируется, но не создаёт
    /// переменную).
    ShadowedContextName,
    /// Обращение `ИмяМодуля.Метод(...)` вне явного контекста объекта, где
    /// `ИмяМодуля` не найдено среди общих модулей конфигурации (внешний
    /// источник [`crate::symbols::SymbolSource`] ответил `Some(false)`).
    /// Эмиттится только из `crate::config_objects`.
    UnknownCommonModule,
    /// Обращение `Менеджер.Имя` к менеджеру объектов конфигурации
    /// (`Справочники`, `Документы`, …), где `Имя` не найдено в
    /// соответствующей коллекции каталога выгрузки (внешний источник ответил
    /// `Some(false)`). Эмиттится только из `crate::config_objects`.
    UnknownMetadataObject,
    /// Голова обращения `Имя.Член` не объявлена в модуле и не найдена среди
    /// свойств глобального контекста платформы, но близка к одному из них по
    /// написанию. Типовой случай — имя коллекции в форме языка запросов:
    /// `Справочник.Номенклатура` вместо `Справочники.Номенклатура`. Такой код не
    /// компилируется, а неверная голова вдобавок отключает проверку имени
    /// объекта внутри цепочки. Confidence проставляется явно по двухпороговой
    /// эвристике `fuzzy_confidence_for` (как у `UnknownGlobalMethod`): без
    /// сходства находки нет вовсе — иначе её получило бы каждое обращение к
    /// общему модулю конфигурации. Эмиттится только из `crate::config_objects`.
    UnknownGlobalProperty,
    /// Вызов `Коллекция.Объект.Метод(...)` (`Справочники.Сотрудники.НайтиПоРеквизитам`),
    /// где `Метод` не найден среди методов типа-менеджера объекта в справке
    /// платформы (`СправочникМенеджер.<Имя справочника>` и т.п.), не объявлен
    /// нигде в конфигурации (экспорт модуля менеджера) и при этом близок по
    /// написанию к настоящему методу менеджера — то есть это опечатка.
    /// Confidence проставляется явно по двухпороговой эвристике
    /// `fuzzy_confidence_for` (как у `UnknownGlobalMethod`). Эмиттится только из
    /// `crate::config_objects`.
    UnknownManagerMethod,
    /// Запрос кладёт результат во временную таблицу (`ПОМЕСТИТЬ`) без
    /// `ИНДЕКСИРОВАТЬ ПО`, а дальше эта таблица участвует в соединении.
    /// Соединение с неиндексированной временной таблицей платформа выполняет
    /// перебором. Эмиттится только из `crate::query_rules`.
    TempTableWithoutIndex,
    /// В условии соединения (`ПО`) есть `ИЛИ`: оптимизатор не может
    /// воспользоваться индексом и переходит к перебору соединяемых наборов.
    /// Эмиттится только из `crate::query_rules`.
    OrInJoinCondition,
    /// Соединение с подзапросом вместо временной таблицы: подзапрос не
    /// индексируется и вычисляется заново. Эмиттится только из
    /// `crate::query_rules`.
    JoinWithSubquery,
    /// Чтение физической таблицы регистра остатков вместо виртуальной
    /// (`Остатки`, `ОстаткиИОбороты`). В таблице движений измерений в
    /// кластерном индексе нет — отбор по ним идёт полным просмотром, тогда как
    /// виртуальная таблица читает таблицу итогов, где измерения в индексе.
    /// Эмиттится только из `crate::query_rules`.
    PhysicalRegisterTable,
    /// Виртуальная таблица вызвана без отбора: платформа рассчитает итоги по
    /// всему регистру, а лишние строки отсеются уже после. Эмиттится только из
    /// `crate::query_rules`.
    VirtualTableWithoutFilter,
    /// Соединение по полю, у которого нет ни стандартного индекса, ни свойства
    /// «Индексировать». Множество индексов замкнуто (см. справочник
    /// `1c-standard-indexes.md`), поэтому вывод здесь точный, а не
    /// предположительный. Эмиттится только из `crate::query_rules`.
    JoinOnUnindexedField,
    /// Имя объявленной процедуры/функции совпало с ключевым словом языка
    /// (`Выполнить`) либо с глобальной функцией платформы (`Найти`). Модуль не
    /// компилируется: «Ожидается имя процедуры». Эмиттится только из
    /// `crate::declarations`.
    ReservedProcedureName,
    /// Процедура или функция с таким именем в модуле уже объявлена. Модуль не
    /// компилируется. Эмиттится только из `crate::declarations`.
    DuplicateDeclaration,
    /// Лишний или недостающий `КонецПроцедуры`/`КонецФункции`. Лишний конец
    /// блока платформа считает концом модуля («Обнаружено логическое завершение
    /// исходного текста модуля»), и весь код ниже теряется. Эмиттится только из
    /// `crate::declarations`.
    UnbalancedModuleBlock,
    /// Блок языка не закрыт или закрыт не тем ключевым словом: `Попытка` без
    /// `КонецПопытки`, `Если` без `КонецЕсли`, `Цикл` без `КонецЦикла`. Модуль не
    /// компилируется («Ожидается КонецПопытки»), а дерево `tree-sitter-bsl` на
    /// таком коде восстанавливается и отдаёт узлы `ERROR` — поэтому проверка
    /// текстовая (`crate::blocks`). Эмиттится только из `validate_module`.
    UnbalancedCodeBlock,
    /// Обращение к члену у ВЫРАЖЕНИЯ: у результата конструктора
    /// (`Новый Файл("x").Расширение`) или у группирующих скобок
    /// (`(Новый Файл("x")).Размер()`). Платформа разрешает обращение к члену
    /// только после ВЫЗОВА метода (`Запрос.Выполнить().Выбрать()`), остальное не
    /// компилируется. Текстовая проверка `crate::blocks`, только из
    /// `validate_module`.
    MemberAccessOnExpression,
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
    ///
    /// `ShadowedContextName` — Confidence тоже проставляется явно, через
    /// `new_with_confidence` (`crate::context_names`); здесь — Low как
    /// безопасный fallback.
    pub fn confidence(self) -> Confidence {
        match self {
            // `UnknownCommonModule`/`UnknownMetadataObject` — точная сверка со
            // списком объектов реальной конфигурации (`crate::config_objects`),
            // не эвристика: тот же уровень надёжности, что у сверки со
            // справкой платформы.
            // Правила запросов: находка следует из структуры разобранного
            // текста, а не из эвристики. `TempTableWithoutIndex` и
            // `OrInJoinCondition` — High: обе конструкции видны в тексте
            // однозначно, догадок в них нет.
            ExprErrorKind::UnknownEnumValue
            | ExprErrorKind::WrongArgumentCount
            | ExprErrorKind::UndeclaredMethod
            | ExprErrorKind::UnknownCommonModule
            | ExprErrorKind::UnknownMetadataObject
            // Из находок про запросы в `strict` попадают только эти две.
            // Они не про корректность, а про скорость, и профиль `strict`
            // у потребителя означает «блокировать сборку»: блокировать стоит
            // лишь то, что исправляется одной строкой и почти никогда не бывает
            // лишним — индекс временной таблицы и разбор `ИЛИ` в условии связи.
            | ExprErrorKind::TempTableWithoutIndex
            | ExprErrorKind::OrInJoinCondition
            // Находки `crate::declarations`. Это не эвристика и не про скорость:
            // модуль с такой находкой платформа не компилирует вовсе. Гейт обязан
            // их блокировать, поэтому High и место в `strict`.
            | ExprErrorKind::ReservedProcedureName
            | ExprErrorKind::DuplicateDeclaration
            | ExprErrorKind::UnbalancedModuleBlock
            // Находки `crate::blocks` (issue #17): баланс блоков языка и
            // обращение к члену у выражения. Тоже не эвристика — платформа такой
            // модуль не компилирует.
            | ExprErrorKind::UnbalancedCodeBlock
            | ExprErrorKind::MemberAccessOnExpression => Confidence::High,
            // Соединение с подзапросом иногда оправдано (маленький набор,
            // однократное вычисление) — оставляем на усмотрение читающего.
            ExprErrorKind::UnknownTypeMember
            | ExprErrorKind::UnknownNewType
            | ExprErrorKind::UnknownGlobalMethod
            // Confidence реально ставится явно по fuzzy (см. `crate::config_objects`);
            // здесь — безопасный fallback, как у `UnknownGlobalMethod`.
            | ExprErrorKind::UnknownManagerMethod
            | ExprErrorKind::UnknownGlobalProperty
            | ExprErrorKind::UnknownDirective
            | ExprErrorKind::ShadowedContextName
            // Остальные находки про запросы. Вывод в них точный (множество
            // индексов замкнуто, вид регистра известен), но исправление может
            // потребовать переписать половину запроса — а на малых объёмах
            // выигрыша не будет вовсе. Это решение автора кода, не гейта.
            | ExprErrorKind::JoinWithSubquery
            | ExprErrorKind::VirtualTableWithoutFilter
            | ExprErrorKind::PhysicalRegisterTable
            | ExprErrorKind::JoinOnUnindexedField => Confidence::Low,
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
///   (TypeDotMember, NewExpression, GlobalCall), включая тип переменной из
///   конструктора (`Х = Новый ТипX` → члены `Х` проверяются по `ТипX`). Дефолт.
///   Имя переменной, совпавшее с именем платформенного типа, типом НЕ считается:
///   нет конструктора — проверка членов молчит (issue #11).
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
        Some(extract_scope_map(
            index,
            &cleaned,
            &annotations,
            level,
            &facts.if_branches,
            &facts.loop_var_sites,
        ))
    } else {
        None
    };

    let mut errors = Vec::new();
    let locals = LocalNames::new(&facts);
    // Фрагмент: путь модуля неизвестен, считаем, что это не модуль формы.
    check_type_dot_members(
        index,
        source,
        &facts.dots,
        scope_map.as_ref(),
        Some(&locals),
        false,
        None,
        None,
        &mut errors,
    );
    check_new_expressions(index, source, &facts.news, &mut errors);
    check_global_calls(
        index,
        source,
        &facts.calls,
        None,
        false,
        None,
        None,
        false,
        false,
        None,
        &mut errors,
    );
    errors.sort_by_key(|e| (e.line, e.col));

    ExpressionValidation {
        valid: errors.is_empty(),
        tree_parsed: facts.parsed,
        symbols_available: true,
        degraded_reason: None,
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
        result.errors.retain(|e| e.confidence == Confidence::High);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_type_dot_members(
    index: &PlatformIndex,
    src: &str,
    dots: &[DotFact],
    scope_map: Option<&ScopeMap>,
    locals: Option<&LocalNames>,
    form_module: bool,
    symbols: Option<&dyn SymbolSource>,
    record_set: Option<(&str, &str)>,
    errors: &mut Vec<ExprError>,
) {
    for dot in dots {
        let head = dot.head.as_str();
        let member = dot.member.as_str();

        // Локальное имя ПЕРЕКРЫВАЕТ одноимённый платформенный тип: `ЭлементОтбора`,
        // `Отбор`, `Поле`, `Блокировка`, `Запрос` — и типы платформы, и ходовые
        // имена переменных. Раньше голова обращения сверялась с типом раньше, чем
        // с переменными, и члены переменной проверялись по чужому типу («у типа
        // ЭлементОтбора нет члена ЛевоеЗначение», issue #11). Тип такой переменной
        // даёт конструктор (`Запрос = Новый Запрос`) или вывод типов (уровень ≥ 2);
        // типа нет — молчим, это лучше ложной находки с подсказкой чужого члена.
        //
        // Тем же конфликтом бывает свойство контекста модуля: `УсловноеОформление`
        // — свойство формы типа `УсловноеОформлениеКомпоновкиДанных`, а не
        // одноимённый платформенный тип.
        let head_is_local = locals.is_some_and(|l| l.is_local(dot.head_byte, head))
            || crate::context_names::is_context_property_of_other_type(index, form_module, head);

        // Уровень 1: head — это имя платформенного типа.
        // Уровень 2: head может быть локальной переменной с известным типом.
        // Тип бывает составным (несколько альтернатив): член проверяется по
        // ОБЪЕДИНЕНИЮ, находка — только если его нет ни у одной альтернативы
        // (issue #15, класс 4: `ПараметрыВыполненияКоманды.Источник` — форма ИЛИ
        // окно, и проверка по одной из них давала ложную находку).
        let candidates: Vec<String> = if head_is_local {
            // Тип из ближайшего конструктора выше точки, иначе — из вывода типов.
            // Оба источника позиционны и учитывают ветви `Если/Иначе` (issue #15,
            // класс 1).
            locals
                .and_then(|l| l.constructed_type(dot.head_byte, head))
                .or_else(|| scope_map.and_then(|sm| sm.type_of_var(dot.head_byte, head)))
                .unwrap_or_default()
        } else if index.find_type(head).is_some() {
            vec![head.to_string()]
        } else {
            scope_map
                .and_then(|sm| sm.type_of_var(dot.head_byte, head))
                .unwrap_or_default()
        };
        if candidates.is_empty() {
            continue; // head — обычная переменная без выведенного типа
        }
        let types: Vec<&Type> = candidates
            .iter()
            .filter_map(|name| index.find_type(name))
            .collect();
        if types.is_empty() {
            continue;
        }
        // Открытый состав членов — молчим целиком, если такая альтернатива есть
        // хоть одна: значения открытого перечисления добавляет конфигурация
        // (issue #2), а члены выборок/структур/COM-объектов задаются в рантайме
        // (issue #15, класс 2).
        if types
            .iter()
            .any(|ty| ty.is_open_enum() || is_dynamic_member_type(&ty.name_ru))
        {
            continue;
        }
        // Имена СВОЙСТВ, которые задаются данными, а не справкой (`XBase` —
        // колонки DBF-файла, issue #31). Вызов метода при этом проверяется:
        // опечатка в имени метода должна находиться, как и раньше.
        if !dot.member_is_call && types.iter().any(|ty| is_dynamic_property_type(&ty.name_ru)) {
            continue;
        }
        // Класс 1 (issue #36): состав `ПараметрыСеанса` объявляет конфигурация,
        // в справке платформы членов у этого типа нет (`Очистить` — единственный
        // метод, он обрабатывается обычной проверкой ниже). Параметр сеанса
        // сверяется с источником имён (`SessionParameter`), а не со справкой:
        // имя есть в конфигурации — молчание; выдумано — находка; источник не
        // настроен или не знает коллекцию — молчание (прежнее поведение).
        if !dot.member_is_call
            && types
                .iter()
                .any(|ty| crate::homoglyphs::same_after_fold(&ty.name_ru, "ПараметрыСеанса"))
        {
            match symbols.and_then(|s| s.object_exists("SessionParameters", &member.to_lowercase()))
            {
                Some(true) | None => continue,
                Some(false) => {}
            }
        }
        // Класс 3 (issue #36): `Отбор` набора записей регистра. В модуле набора
        // записей голова `Отбор` — фильтр набора (свойство контекста), его поля —
        // измерения регистра плюс стандартные поля, а не члены одноимённого
        // платформенного типа `Отбор`. Состав известен — сверяем с ним; неизвестен
        // — свойства молчат, методы проверяются как обычно (приём XBase, #31).
        if !dot.member_is_call
            && types
                .iter()
                .any(|ty| crate::homoglyphs::same_after_fold(&ty.name_ru, "Отбор"))
        {
            if let Some((collection, name)) = record_set {
                match symbols.and_then(|s| s.object_schema(collection, &name.to_lowercase())) {
                    Some(schema) => {
                        let known = schema
                            .dimensions
                            .iter()
                            .any(|f| crate::homoglyphs::same_after_fold(&f.name, member))
                            || record_set_standard_fields(collection)
                                .iter()
                                .any(|f| crate::homoglyphs::same_after_fold(f, member));
                        if known {
                            continue;
                        }
                        // Не измерение и не стандартное поле — обычная проверка
                        // ниже даст находку.
                    }
                    // Состава нет: источник не настроен или не знает регистр —
                    // обращение к свойству `Отбор` не проверяем.
                    None => continue,
                }
            }
        }
        // Член есть хотя бы у одной альтернативы — находки нет. Имена сверяются
        // со сведением латинско-кириллических двойников: в справке платформы
        // встречаются неотличимые на экране опечатки (issue #18).
        if types.iter().any(|ty| type_has_member(ty, member)) {
            continue;
        }

        let (line, col) = pos_at(src, dot.member_byte);
        let allowed: Vec<String> = types.iter().flat_map(|ty| type_member_names(ty)).collect();
        let suggestion = closest_str(member, &allowed);
        let type_label = types
            .iter()
            .map(|ty| ty.name_ru.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        if types.iter().all(|ty| ty.is_enum()) {
            errors.push(ExprError::new(
                line,
                col,
                ExprErrorKind::UnknownEnumValue,
                format!(
                    "Значение '{}' не существует у типа-перечисления '{}'.{}",
                    member,
                    type_label,
                    suggestion
                        .as_ref()
                        .map(|s| format!(" Возможно, вы имели в виду '{s}'."))
                        .unwrap_or_default()
                ),
                suggestion,
                Vec::new(),
            ));
        } else {
            errors.push(ExprError::new(
                line,
                col,
                ExprErrorKind::UnknownTypeMember,
                format!(
                    "У типа '{}' нет члена '{}'.{}",
                    type_label,
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

/// Имена членов типа — для подсказки «возможно, вы имели в виду».
fn type_member_names(ty: &Type) -> Vec<String> {
    if ty.is_enum() {
        // Диапазоны из справки показываем развёрнутыми: `A...Z` в подсказке
        // бесполезен, нужны сами имена (issue #22).
        return crate::enum_values::value_names(&ty.enum_values);
    }
    let mut names: Vec<String> = ty.methods.iter().map(|m| m.name_ru.clone()).collect();
    names.extend(ty.properties.iter().map(|p| p.name_ru.clone()));
    names
}

/// Есть ли у типа такой член: метод, свойство или значение перечисления.
///
/// Имена сверяются после сведения латинско-кириллических двойников (issue #18):
/// в справке 8.3.17 значение записано как `БлокироватьВеcьИнтерфейс` с ЛАТИНСКОЙ
/// `c`, а платформа принимает кириллическую. Без сведения корректный код получал
/// `unknown_enum_value` с `confidence: high`, а подсказка предлагала имя, которое
/// платформа отвергает.
///
/// Значения-диапазоны (`A...Z`, `F1...F12` у перечисления `Клавиша`)
/// разворачиваются, а значения, которые платформа принимает ради совместимости,
/// берутся из точечного словаря — иначе `Клавиша.A` и
/// `ОтображениеОбычнойГруппы.Линия` дают ложную находку `high` (issue #22).
fn type_has_member(ty: &Type, member: &str) -> bool {
    if ty.is_enum() {
        return crate::enum_values::has_value(&ty.enum_values, member)
            || crate::enum_values::is_deprecated_value(&ty.name_ru, member);
    }
    let same = |name: &str| crate::homoglyphs::same_after_fold(name, member);
    ty.methods
        .iter()
        .any(|m| same(&m.name_ru) || same(&m.name_en))
        || ty
            .properties
            .iter()
            .any(|p| same(&p.name_ru) || same(&p.name_en))
}

/// Типы с открытым (поздним) составом членов: колонки выборки запроса / таблицы
/// значений / дерева значений, произвольные ключи структуры, реквизиты и элементы
/// формы, свойства XDTO, COM-объект и внешний объект. Для них проверка
/// `Объект.Член` бессмысленна: состав задаётся в рантайме и в `hbk` отсутствует
/// (массовый FP: `Выборка.Регистратор`, `СтрокаТЗ.ОбъектОплаты`, `Подключение.Open`
/// у COM-объекта). На уровнях 1/2 такие типы как `head` почти не встречаются;
/// проблема всплывает на level=3, где return-type tracking выводит их как тип
/// переменной (`Выб = Рез.Выбрать()`).
///
/// Написание — как в справке платформы; сравнение идёт со сведением
/// латинско-кириллических двойников, поэтому регистр и алфавит отдельной буквы
/// (`COMОбъект` — латиница плюс кириллица) значения не имеют.
const DYNAMIC_MEMBER_TYPES: &[&str] = &[
    // Колонки выборок и строк коллекций задаются текстом запроса / составом ТЗ.
    "выборкаизрезультатазапроса",
    "выборкаданных",
    "строкатаблицызначений",
    "строкадеревазначений",
    // Произвольные ключи.
    "структура",
    "фиксированнаяструктура",
    // Реквизиты и элементы конкретной формы — в метаданных формы, не в hbk.
    "форма",
    "управляемаяформа",
    "элементыформы",
    // Свойства XDTO задаются схемой/пакетом в runtime.
    "объектxdto",
    "значениеxdto",
    // COM-объект и внешний объект: члены связываются поздно (issue #15, класс 2).
    "comобъект",
    "внешнийобъект",
];

fn is_dynamic_member_type(name_ru: &str) -> bool {
    // Сведение двойников — С ОБЕИХ СТОРОН. В списке есть имена с латиницей
    // (`ОбъектXDTO`, `COMОбъект`), и приводить нужно и написание из справки, и
    // элемент списка: иначе сравнение разъезжается. Этот регресс ловил корпусный
    // замер — `ОбъектXDTO` перестал считаться динамическим типом и дал +177
    // ложных находок на 3000 модулях, при том что модульные тесты молчали.
    let folded = crate::homoglyphs::fold_lookalikes(&name_ru.to_lowercase());
    DYNAMIC_MEMBER_TYPES
        .iter()
        .any(|entry| crate::homoglyphs::fold_lookalikes(entry) == folded)
}

/// Типы, у которых имена СВОЙСТВ задаются данными в рантайме, а методы — справкой.
///
/// `XBase` (issue #31): поля — это колонки конкретного DBF-файла (`База.DAYDATE`,
/// `База.NAME`), статически их не узнать, поэтому обращение к свойству не
/// проверяется. Но вызов метода проверяется как обычно: `База.ОткрытьФайлл`
/// (опечатка) обязан остаться находкой — иначе правка «чинит» и настоящее.
const DYNAMIC_PROPERTY_TYPES: &[&str] = &["xbase"];

fn is_dynamic_property_type(name_ru: &str) -> bool {
    let folded = crate::homoglyphs::fold_lookalikes(&name_ru.to_lowercase());
    DYNAMIC_PROPERTY_TYPES
        .iter()
        .any(|entry| crate::homoglyphs::fold_lookalikes(entry) == folded)
}

/// Стандартные поля фильтра `Отбор` набора записей по виду регистра (issue #36).
///
/// Измерения добавляются из схемы источника имён; здесь — только поля, которые
/// есть у всех регистров данного вида. Признаки «активность» и «независимость»
/// в схему не входят, поэтому берём надмножество: молчание на реальном поле
/// лучше ложной находки. `collection` — папка выгрузки (регистр букв не важен).
fn record_set_standard_fields(collection: &str) -> &'static [&'static str] {
    if collection.eq_ignore_ascii_case("AccumulationRegisters") {
        &[
            "Регистратор",
            "Период",
            "Активность",
            "ВидДвижения",
            "НомерСтроки",
        ]
    } else if collection.eq_ignore_ascii_case("InformationRegisters") {
        &["Регистратор", "Период"]
    } else if collection.eq_ignore_ascii_case("AccountingRegisters")
        || collection.eq_ignore_ascii_case("CalculationRegisters")
    {
        &["Регистратор", "Период", "Активность", "НомерСтроки"]
    } else {
        &[]
    }
}

/// Пары «контекст модуля — метод», для которых справка описывает ПЕРЕАДРЕСАЦИЮ
/// неквалифицированного вызова глобальному контексту, и только они.
///
/// Справка `СправочникМенеджер.ПолучитьДанныеВыбора` (8.3.27): «Если в модуле
/// менеджера в методе указано два параметра, то будет вызван метод
/// ПолучитьДанныеВыбора глобального контекста». Для такого вызова допустимо
/// число аргументов ЛЮБОЙ из двух сигнатур. В остальных совпадениях имён
/// платформа оставляет метод контекста: `ПолучитьФорму` в модуле обычной формы
/// объекта — метод `ДокументОбъект` с тремя параметрами, и шесть аргументов
/// она не компилирует (issue #32).
///
/// Имена сверяются со сведением латинско-кириллических двойников — как и
/// остальные имена справки (issue #18).
const REDIRECTED_TO_GLOBAL: &[(&str, &str)] = &[("справочникменеджер.", "получитьданныевыбора")];

fn is_documented_redirect(context: &Type, method_name: &str) -> bool {
    let type_name = crate::homoglyphs::fold_lookalikes(&context.name_ru.to_lowercase());
    let method = crate::homoglyphs::fold_lookalikes(&method_name.to_lowercase());
    REDIRECTED_TO_GLOBAL
        .iter()
        .any(|(prefix, name)| type_name.starts_with(prefix) && method == *name)
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
            // Сортировка — детерминированный порядок кандидатов при равном
            // сходстве (обход HashMap случаен; подсказка плавала между запусками).
            let mut all_types: Vec<String> =
                index.types.values().map(|t| t.name_ru.clone()).collect();
            all_types.sort();
            let suggestion = closest_str(&n.type_name, &all_types);
            errors.push(ExprError::new(
                line,
                col,
                ExprErrorKind::UnknownNewType,
                format!(
                    "Тип '{}' не найден в платформенном контексте.{}",
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
///
/// `symbols_degraded` — источник имён был настроен, но недоступен (не поднят,
/// отвалился). Отличается от `symbols: None` при ненастроенном источнике:
/// имена конфигурации ожидались и не пришли, поэтому вывод «метод не объявлен
/// нигде» здесь недостоверен — находка остаётся (иначе теряется выдуманный
/// вызов вроде `СЕГОДНЯ()`), но с пониженной уверенностью и честным текстом.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_global_calls(
    index: &PlatformIndex,
    src: &str,
    calls: &[CallFact],
    user_symbols: Option<&HashSet<String>>,
    strict_unknown: bool,
    symbols: Option<&dyn SymbolSource>,
    owner_exports: Option<&HashSet<String>>,
    owner_unknown: bool,
    symbols_degraded: bool,
    module_context: Option<&Type>,
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
            // Внешний источник имён проверяем ДО fuzzy. Имя, законно видимое
            // отсюда (экспорт глобального общего модуля, метод модуля
            // объекта-владельца), иначе превращается в «опечатку платформенного
            // метода»: `ОткрытьЗначения()` владельца отстоит на одну правку от
            // платформенного `ОткрытьЗначение` и даёт High на законном вызове.
            // Собственный whitelist модуля так и проверяется — выше по циклу.
            let lc = call.name.to_lowercase();
            let visible_via_symbols = owner_exports.map(|s| s.contains(&lc)).unwrap_or(false)
                || symbols.map(|s| s.is_global_export(&lc)).unwrap_or(false);
            if visible_via_symbols {
                continue;
            }
            // Модуль формы есть, но модуль-владелец не попал в источник имён
            // («не знаю»): вызов может быть его методом, и любая находка здесь
            // недостоверна — молчим, как и обещает контракт `owner_exports =
            // None` (аудит PR, M1). Проверка стоит ДО fuzzy: иначе законный
            // `ОткрытьЗначения()` владельца превращался бы в «опечатку»
            // платформенного `ОткрытьЗначение`.
            if owner_unknown {
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
                // (`owner_exports`); оба уже отсеяны выше, до fuzzy.
                if symbols.map(|s| s.method_exists(&lc)).unwrap_or(false) {
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
                } else if symbols_degraded {
                    // Имена конфигурации ожидались, но источник недоступен.
                    // Выбрасывать находку нельзя — сюда попадают выдуманные
                    // глобальные вызовы (`СЕГОДНЯ()`), ради которых проверка и
                    // нужна. Оставлять High тоже нельзя: без имён конфигурации
                    // сюда же попадает каждый вызов процедуры глобального
                    // общего модуля — на УТ это 1420 находок.
                    let (line, col) = pos_at(src, call.byte);
                    errors.push(ExprError::new_with_confidence(
                        line,
                        col,
                        ExprErrorKind::UndeclaredMethod,
                        format!(
                            "Метод '{}' не объявлен в этом модуле и не найден в платформенном \
                             контексте. Имена конфигурации не проверялись — источник имён \
                             недоступен, поэтому метод может быть объявлен в другом модуле.",
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

        // Неквалифицированный вызов в модуле менеджера, объекта или обычной формы
        // разрешается методом КОНТЕКСТА модуля, а не глобальной функцией
        // (issue #19): `ПолучитьФорму` в модуле обычной формы — это
        // `ДокументОбъект.ПолучитьФорму(<Форма>, <Владелец>, <КлючУникальности>)`
        // с тремя параметрами, а не глобальная функция с диапазоном 1..6,
        // и `ПолучитьДанныеВыбора(Параметры)` в модуле менеджера — метод
        // менеджера. Если метод у контекста есть, сверяемся с НИМ и на этом
        // заканчиваем: глобальная сигнатура здесь не источник истины.
        if let Some(context) = module_context {
            if let Some(result) =
                crate::check::validate_type_method_call(context, &call.name, call.arg_count)
            {
                if !result.valid {
                    // Переадресация вызова глобальному контексту документирована
                    // НЕ для всякой пары «имя есть и у контекста модуля, и в
                    // глобальном контексте». В модуле менеджера
                    // `ПолучитьДанныеВыбора` с двумя параметрами платформа
                    // передаёт глобальной функции — так написано в справке
                    // `СправочникМенеджер.ПолучитьДанныеВыбора`. А в модуле
                    // обычной формы объекта `ПолучитьФорму` — метод ОБЪЕКТА с
                    // тремя параметрами, и шесть аргументов платформа не
                    // компилирует: «Слишком много фактических параметров»
                    // (issue #32, продолжение #19 — объединение сигнатур по
                    // любому совпавшему имени гасило эту находку).
                    if is_documented_redirect(context, &call.name)
                        && validate_method_call(index, &call.name, call.arg_count).valid
                    {
                        continue;
                    }
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
                continue;
            }
        }

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

pub(crate) fn pos_at(src: &str, byte_idx: usize) -> (u32, u32) {
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
        // Имя из справки со смешанными алфавитами (`БлокироватьВеcьИнтерфейс`)
        // в подсказку не отдаём: в коде его повторят дословно, а платформа такое
        // имя отвергнет — подсказка сломала бы рабочий код (issue #18).
        .filter(|c| !crate::homoglyphs::is_mixed_alphabet(c))
        .map(|c| (similarity(&target_l, &c.to_lowercase()), c.clone()))
        // Тай-брейк по имени: при равном сходстве порядок кандидатов не должен
        // влиять на подсказку (недетерминизм ловился запуском).
        .max_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.cmp(&a.1))
        })
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

pub(crate) fn lev(a: &str, b: &str) -> usize {
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
                syntax: String::new(),
                description: String::new(),
                parameters: vec![
                    Parameter {
                        name: "ИсходнаяСтрока".into(),
                        type_name: String::new(),
                        required: true,
                        description: String::new(),
                    },
                    Parameter {
                        name: "КодЯзыка".into(),
                        type_name: String::new(),
                        required: false,
                        description: String::new(),
                    },
                ],
            }],
            note: None,
        });
        let src = "Текст = НСтр(\"ru = 'Неверный тип запроса.'\");";
        let res = validate_expression_at_level(&index, src, 1);
        assert!(
            res.valid,
            "НСтр с одним строковым аргументом ложно помечен: {:?}",
            res.errors
        );
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
        assert_eq!(
            ExprErrorKind::UnknownEnumValue.confidence(),
            Confidence::High
        );
        assert_eq!(
            ExprErrorKind::WrongArgumentCount.confidence(),
            Confidence::High
        );
        assert_eq!(
            ExprErrorKind::UnknownTypeMember.confidence(),
            Confidence::Low
        );
        assert_eq!(ExprErrorKind::UnknownNewType.confidence(), Confidence::Low);
        assert_eq!(
            ExprErrorKind::UnknownGlobalMethod.confidence(),
            Confidence::Low
        );
    }

    #[test]
    fn profile_parse_or_default() {
        assert_eq!(Profile::parse_or_default(Some("strict")), Profile::Strict);
        assert_eq!(
            Profile::parse_or_default(Some("  STRICT ")),
            Profile::Strict
        );
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
                note: None,
            }],
            note: None,
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
                note: None,
            }],
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: Vec::new(),
            note: None,
        });

        // Открытая коллекция: первым значением — псевдо-элемент `<...>`.
        index.insert_type(Type {
            name_ru: "КартинкиТест".into(),
            name_en: "PicturesTest".into(),
            description: String::new(),
            methods: Vec::new(),
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: vec![
                EnumValue {
                    name_ru: "<Имя картинки>".into(),
                    name_en: String::new(),
                    description: String::new(),
                    note: None,
                },
                EnumValue {
                    name_ru: "Лупа".into(),
                    name_en: "Magnifier".into(),
                    description: String::new(),
                    note: None,
                },
            ],
            note: None,
        });

        // Issue #31: `XBase` — свойства это поля конкретного DBF-файла (динамические),
        // а методы берутся из справки и проверяются.
        index.insert_type(Type {
            name_ru: "XBase".into(),
            name_en: "XBase".into(),
            description: String::new(),
            methods: vec![Method {
                name_ru: "ОткрытьФайл".into(),
                name_en: "OpenFile".into(),
                description: String::new(),
                return_type: String::new(),
                signatures: Vec::new(),
                note: None,
            }],
            properties: Vec::new(),
            constructors: Vec::new(),
            enum_values: Vec::new(),
            note: None,
        });

        index
    }

    #[test]
    fn open_collection_value_is_not_reported() {
        let index = test_index();
        // Значение из конфигурации: в справке его нет, но тип открытый.
        let src = "А = КартинкиТест.МояКартинка;";
        let result = validate_expression_with_profile(&index, src, 1, Profile::Full);
        assert!(
            result.valid,
            "открытая коллекция не даёт находок: {:?}",
            result.errors
        );
    }

    #[test]
    fn profile_full_returns_all_findings() {
        let index = test_index();
        // Первая строка — high (значение перечисления), вторая — low (член типа).
        let src = "А = ЦветТест.Синий;\nБ = СтруктураТест.Опечатка;";
        let result = validate_expression_with_profile(&index, src, 1, Profile::Full);

        assert!(!result.valid);
        assert_eq!(result.errors.len(), 2, "full должен вернуть обе находки");
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.kind == ExprErrorKind::UnknownEnumValue
                    && e.confidence == Confidence::High)
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.kind == ExprErrorKind::UnknownTypeMember
                    && e.confidence == Confidence::Low)
        );
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

    /// Issue #31: свойства `XBase` — поля конкретного DBF-файла, статически их не
    /// узнать, поэтому обращение к свойству не проверяется. Вызов метода — да:
    /// иначе правка «чинит» и настоящую опечатку в имени метода.
    #[test]
    fn xbase_properties_are_dynamic_but_methods_are_checked() {
        let index = test_index();

        // Свойства (поля DBF) — молчание.
        let src = "База = Новый XBase;\nД = База.DAYDATE;\nБаза.NAME = \"x\";\n";
        let result = validate_expression_with_profile(&index, src, 3, Profile::Full);
        assert!(
            result.valid,
            "поля DBF не проверяются, находок быть не должно: {:?}",
            result.errors
        );

        // Контроль: опечатка в имени МЕТОДА остаётся находкой.
        let src2 = "База = Новый XBase;\nБаза.ОткрытьФайлл(\"f\");\n";
        let result2 = validate_expression_with_profile(&index, src2, 3, Profile::Full);
        assert!(!result2.valid, "опечатка в методе должна находиться");
        assert!(
            result2
                .errors
                .iter()
                .any(|e| e.kind == ExprErrorKind::UnknownTypeMember),
            "ожидалась находка unknown_type_member: {:?}",
            result2.errors
        );
    }

    /// Issue #15, класс 2: члены `COMОбъект`/`ВнешнийОбъект` не проверяются.
    /// Отдельно держим имена с латиницей в составе (`COMОбъект`, `ОбъектXDTO`):
    /// список динамических типов сравнивается со сведением двойников С ОБЕИХ
    /// сторон, и регресс на `ОбъектXDTO` уже проскакивал — его поймал корпусный
    /// замер, а не модульные тесты.
    #[test]
    fn com_and_external_objects_are_dynamic_member_types() {
        for name in [
            "COMОбъект",
            "ВнешнийОбъект",
            "ОбъектXDTO",
            "ЗначениеXDTO",
            "ВыборкаИзРезультатаЗапроса",
            "Структура",
        ] {
            assert!(
                is_dynamic_member_type(name),
                "{name} должен быть динамическим"
            );
        }
        for name in ["Массив", "ТаблицаЗначений", "Строка"] {
            assert!(!is_dynamic_member_type(name), "{name} — обычный тип");
        }
    }
}
