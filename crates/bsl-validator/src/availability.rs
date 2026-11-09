//! Доступность методов платформы по контексту выполнения (этап 2 плана
//! `TZ-dostupnost-metodov-kontekst.md`).
//!
//! У каждого метода, свойства и типа справка 1С содержит главу «Доступность:» —
//! список контекстов, в которых сущность существует (`Сервер`, `Тонкий клиент`,
//! `толстый клиент`, …). Данные приходят из `platform_index::Availability`; здесь
//! вторая половина правила: определить контекст МЕСТА вызова и решить, законен ли
//! вызов в нём.
//!
//! Проверяются три вида фактов AST: глобальные вызовы (`ПоказатьВопрос()`), вызовы
//! методов у однозначно разрешённого типа (`Таблица.Найти(...)`) и конструкторы
//! (`Новый Запрос`). Обращения к свойствам, `Коллекция.Объект.Метод(...)` и методы
//! конфигурации не проверяются: у первых двух нет достоверного источника
//! доступности, последние не про платформу.
//!
//! Молчание — штатный ответ. Находка появляется, только когда контекст назван
//! ОДНОЗНАЧНО (сервер либо клиент управляемого приложения) и набор меток полон
//! (нет `Unknown(_)`, нет `интеграции`, список непустой).

use bsl_parse::{AstFacts, ProcScope};
use platform_index::{Availability, Method, Type};

use crate::expression::{pos_at, Confidence, ExprError, ExprErrorKind};
use crate::module_context::{FormKind, ModuleKind};

/// Контекст выполнения в точке вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallContext {
    /// Серверный код: модуль объекта, менеджера, набора записей; `&НаСервере`
    /// и `&НаСервереБезКонтекста` в модуле управляемой формы.
    Server,
    /// Клиент управляемого приложения: `&НаКлиенте`.
    ManagedClient,
    /// Процедура компилируется и на клиенте, и на сервере
    /// (`&НаКлиентеНаСервере[БезКонтекста]`): вызов законен, если метод доступен
    /// хотя бы одной стороне.
    Both,
    /// Клиент обычного приложения: проверка выключена — там клиентская и серверная
    /// части не разделены.
    OrdinaryClient,
    /// Контекст неизвестен (общий модуль, расширение, `#Если`, процедура без
    /// директивы, тело модуля формы): проверка молчит.
    Unknown,
}

/// Контекст вызова по виду модуля, виду формы, директиве процедуры и признаку
/// «вызов внутри `#Если`».
///
/// Матрица — раздел 6.2 плана. Порядок важен: признак `#Если` и директивы
/// расширения перекрывают всё остальное, потому что контекст в этих случаях
/// приходит не из самого модуля.
pub fn call_context(
    module_kind: ModuleKind,
    form_kind: Option<FormKind>,
    directive: Option<&str>,
    in_preproc: bool,
) -> CallContext {
    // Ветки `#Если` компилируются по-разному на клиенте и сервере: какая ветка
    // попадёт в сборку, по тексту модуля не видно.
    if in_preproc {
        return CallContext::Unknown;
    }
    // Расширения (`&Перед`/`&После`/`&Вместо`/`&ИзменениеИКонтроль`)
    // компилируются в контексте расширяемой процедуры, которого у валидатора нет.
    // Случай нескольких директив (`&НаСервере` + `&Вместо(…)`) отсекается
    // раньше — в `ContextLocator::at`, по признаку `ProcScope::is_extension`:
    // здесь видно только контекстную директиву.
    if directive.is_some_and(is_extension_directive) {
        return CallContext::Unknown;
    }
    match module_kind {
        // Модули объекта, менеджера и набора записей исполняются на сервере.
        ModuleKind::Object | ModuleKind::Manager | ModuleKind::RecordSet => CallContext::Server,
        // Общий модуль, модуль команды/приложения: контексты задаются свойствами
        // общего модуля, которых в тексте нет.
        ModuleKind::Other => CallContext::Unknown,
        ModuleKind::Form => match form_kind {
            // Модуль обычной формы: клиентская и серверная части не разделены,
            // директив компиляции в нём нет — проверять нечего.
            Some(FormKind::Ordinary) => CallContext::OrdinaryClient,
            // Управляемая форма либо вид формы неизвестен: решает директива.
            // Процедура без директивы (и код уровня модуля) — `Unknown`: как
            // платформа компилирует такой код, экспериментом не проверено.
            Some(FormKind::Managed) | None => directive
                .and_then(directive_context)
                .unwrap_or(CallContext::Unknown),
        },
    }
}

/// Контекст по имени директивы компиляции (без амперсанда, регистр не важен).
///
/// `None` — имя не из числа директив компиляции (опечатка, `&Перед`,
/// неизвестная директива): контекст неизвестен, проверка молчит.
fn directive_context(directive: &str) -> Option<CallContext> {
    let name = directive.trim_start_matches('&').to_lowercase();
    match name.as_str() {
        "наклиенте" | "atclient" => Some(CallContext::ManagedClient),
        "насервере" | "atserver" | "насерверебезконтекста" | "atservernocontext" => {
            Some(CallContext::Server)
        }
        "наклиентенасервере"
        | "atclientatserver"
        | "наклиентенасерверебезконтекста"
        | "atclientatservernocontext" => Some(CallContext::Both),
        _ => None,
    }
}

/// Директива подключает процедуру к расширяемой (расширение конфигурации).
fn is_extension_directive(directive: &str) -> bool {
    matches!(
        directive
            .trim_start_matches('&')
            .to_lowercase()
            .replace('ё', "е")
            .as_str(),
        "перед"
            | "после"
            | "вместо"
            | "изменениеиконтроль"
            | "before"
            | "after"
            | "around"
            | "changeandvalidate"
    )
}

/// Вычислитель контекста по месту вызова: вид модуля и вид формы известны
/// заранее, а директива процедуры и попадание в `#Если` зависят от байта факта.
///
/// Нужен потому, что у модуля формы процедуры бывают и клиентские, и серверные:
/// одно значение на модуль здесь неверно.
pub(crate) struct ContextLocator<'a> {
    module_kind: ModuleKind,
    form_kind: Option<FormKind>,
    facts: &'a AstFacts,
    /// Процедуры по возрастанию начала — двоичный поиск вместо перебора на
    /// каждый факт (у модулей конфигурации процедур тысячи).
    procs: Vec<&'a ProcScope>,
}

impl<'a> ContextLocator<'a> {
    pub(crate) fn new(
        module_kind: ModuleKind,
        form_kind: Option<FormKind>,
        facts: &'a AstFacts,
    ) -> Self {
        let mut procs: Vec<&ProcScope> = facts.procs.iter().collect();
        procs.sort_unstable_by_key(|proc_| proc_.byte_start);
        Self {
            module_kind,
            form_kind,
            facts,
            procs,
        }
    }

    /// Контекст вызова, начинающегося в байте `byte`.
    pub(crate) fn at(&self, byte: usize) -> CallContext {
        let (directive, is_extension) = self.directives_at(byte);
        // Расширение перекрывает любую директиву: у процедуры с `&НаСервере` и
        // `&Вместо(…)` имя директивы контекстное, и без этой проверки вызов
        // выглядел бы обычным серверным (решение приёмщика, 09.11.2026).
        if is_extension {
            return CallContext::Unknown;
        }
        let in_preproc = self
            .facts
            .preproc_regions
            .iter()
            .any(|(start, end)| byte >= *start && byte < *end);
        call_context(self.module_kind, self.form_kind, directive, in_preproc)
    }

    /// Директива процедуры, содержащей байт, и признак расширения; `(None, false)`
    /// — код уровня модуля.
    fn directives_at(&self, byte: usize) -> (Option<&str>, bool) {
        let idx = self.procs.partition_point(|proc_| proc_.byte_start <= byte);
        let Some(proc_) = (idx > 0).then(|| self.procs[idx - 1]) else {
            return (None, false);
        };
        if byte >= proc_.byte_end {
            return (None, false);
        }
        (proc_.directive.as_deref(), proc_.is_extension)
    }
}

/// Разбор решения по доступности: `Some((формулировка, уверенность))` — находка,
/// `None` — молчание.
///
/// Формулировка подставляется в сообщение после слова «недоступен»: `на сервере`,
/// `в клиенте управляемого приложения`, …
fn verdict(
    context: CallContext,
    availability: &Availability,
) -> Option<(&'static str, Confidence)> {
    // Неполный список — молчание, а не догадка: пустые метки и `интеграция`
    // смысла для правила не имеют, а незнакомое написание означает, что справка
    // новее словаря индекса.
    if availability.marks.is_empty() || availability.has_unknown() {
        return None;
    }
    let available = match context {
        CallContext::Server => availability.server_side(),
        CallContext::ManagedClient => availability.managed_client_side(),
        CallContext::Both => availability.server_side() || availability.managed_client_side(),
        CallContext::OrdinaryClient | CallContext::Unknown => return None,
    };
    if available {
        return None;
    }
    let formulation = match context {
        CallContext::Server => "на сервере",
        CallContext::ManagedClient => "в клиенте управляемого приложения",
        CallContext::Both => "ни на сервере, ни в клиенте управляемого приложения",
        CallContext::OrdinaryClient | CallContext::Unknown => return None,
    };
    // Уверенность. На сервере «толстый клиент» в списке меток ничего не решает:
    // вызов ищется в СЕРВЕРНОЙ части, а её метки от трактовки толстого клиента
    // не зависят. В клиенте управляемого приложения — решает: если «толстый
    // клиент» на самом деле означает управляемый толстый клиент, метод в клиенте
    // доступен, и находка ложная. Поэтому такие находки — `Medium` (раздел 6.4
    // плана), остальные — `High`.
    let thick_client_questionable =
        matches!(context, CallContext::ManagedClient | CallContext::Both)
            && availability.ordinary_client_side();
    let confidence = if thick_client_questionable {
        Confidence::Medium
    } else {
        Confidence::High
    };
    Some((formulation, confidence))
}

/// Метод глобального контекста вызван в контексте, где справка его не описывает.
pub(crate) fn check_global_method(
    context: CallContext,
    src: &str,
    byte: usize,
    method: &Method,
    errors: &mut Vec<ExprError>,
) {
    let Some(availability) = method.availability.as_ref() else {
        return;
    };
    let Some((formulation, confidence)) = verdict(context, availability) else {
        return;
    };
    let (line, col) = pos_at(src, byte);
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        ExprErrorKind::UnavailableInContext,
        format!(
            "Метод `{}` недоступен {formulation}. По справке доступен: {}.",
            method.name_ru,
            availability.labels()
        ),
        confidence,
        None,
        Vec::new(),
    ));
}

/// Метод типа вызван в контексте, где справка его не описывает.
///
/// Вызывается только там, где тип разрешён ОДНОЗНАЧНО и член уже найден как
/// метод: при объединении альтернатив доступность могла бы прийти из чужой ветви.
pub(crate) fn check_member(
    context: CallContext,
    src: &str,
    byte: usize,
    ty: &Type,
    member: &str,
    errors: &mut Vec<ExprError>,
) {
    // Значения перечислений — не методы; их доступность в справке не описана.
    if ty.is_enum() {
        return;
    }
    let Some(method) = ty.methods.iter().find(|method| {
        crate::homoglyphs::same_after_fold(&method.name_ru, member)
            || (!method.name_en.is_empty()
                && crate::homoglyphs::same_after_fold(&method.name_en, member))
    }) else {
        return;
    };
    let Some(availability) = method.availability.as_ref() else {
        return;
    };
    let Some((formulation, confidence)) = verdict(context, availability) else {
        return;
    };
    let (line, col) = pos_at(src, byte);
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        ExprErrorKind::UnavailableInContext,
        format!(
            "Метод `{}` типа `{}` недоступен {formulation}. По справке доступен: {}.",
            method.name_ru,
            ty.name_ru,
            availability.labels()
        ),
        confidence,
        None,
        Vec::new(),
    ));
}

/// Конструктор `Новый Тип` в контексте, где недоступен сам ТИП.
///
/// Доступность конструктора определяется доступностью типа: у страницы
/// конструктора своей главы «Доступность:» нет (раздел 2 плана).
pub(crate) fn check_type(
    context: CallContext,
    src: &str,
    byte: usize,
    ty: &Type,
    errors: &mut Vec<ExprError>,
) {
    // Серверное направление не проверяем: доступность ТИПА в справке не описывает
    // доступность КОНСТРУКТОРА на сервере. Вендорский код ставит `Новый
    // СочетаниеКлавиш` в `&НаСервере` (35 мест в УТ), хотя у типа только клиентские
    // метки — платформа это допускает. Клиентское направление (`Новый Запрос` в
    // `&НаКлиенте`) проверяем: там расхождение с справкой подтверждается.
    if context != CallContext::ManagedClient {
        return;
    }
    let Some(availability) = ty.availability.as_ref() else {
        return;
    };
    let Some((formulation, confidence)) = verdict(context, availability) else {
        return;
    };
    let (line, col) = pos_at(src, byte);
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        ExprErrorKind::UnavailableInContext,
        format!(
            "Тип `{}` недоступен {formulation}. По справке доступен: {}.",
            ty.name_ru,
            availability.labels()
        ),
        confidence,
        None,
        Vec::new(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_index::AvailabilityMark;

    fn marks(names: &[&str]) -> Availability {
        Availability::new(
            names
                .iter()
                .map(|name| AvailabilityMark::from_label(name))
                .collect(),
        )
    }

    fn server_only() -> Availability {
        marks(&[
            "Сервер",
            "толстый клиент",
            "внешнее соединение",
            "мобильное приложение (сервер)",
            "мобильный автономный сервер",
        ])
    }

    fn client_only() -> Availability {
        marks(&[
            "Тонкий клиент",
            "веб-клиент",
            "мобильный клиент",
            "толстый клиент",
            "мобильное приложение (клиент)",
        ])
    }

    /// Матрица контекста по виду модуля (раздел 6.2 плана).
    #[test]
    fn context_by_module_kind() {
        assert_eq!(
            call_context(ModuleKind::Object, None, None, false),
            CallContext::Server
        );
        assert_eq!(
            call_context(ModuleKind::Manager, None, None, false),
            CallContext::Server
        );
        assert_eq!(
            call_context(ModuleKind::RecordSet, None, None, false),
            CallContext::Server
        );
        assert_eq!(
            call_context(ModuleKind::Other, None, None, false),
            CallContext::Unknown
        );
    }

    /// Директивы управляемой формы; `&Вместо` — расширение, контекст неизвестен.
    #[test]
    fn context_by_directive() {
        let form = ModuleKind::Form;
        let managed = Some(FormKind::Managed);
        assert_eq!(
            call_context(form, managed, Some("НаКлиенте"), false),
            CallContext::ManagedClient
        );
        assert_eq!(
            call_context(form, managed, Some("НаСервере"), false),
            CallContext::Server
        );
        assert_eq!(
            call_context(form, managed, Some("НаСервереБезКонтекста"), false),
            CallContext::Server
        );
        assert_eq!(
            call_context(form, managed, Some("НаКлиентеНаСервере"), false),
            CallContext::Both
        );
        assert_eq!(
            call_context(form, managed, Some("НаКлиентеНаСервереБезКонтекста"), false),
            CallContext::Both
        );
        assert_eq!(
            call_context(form, managed, None, false),
            CallContext::Unknown,
            "процедура без директивы в управляемой форме"
        );
        assert_eq!(
            call_context(form, managed, Some("Вместо"), false),
            CallContext::Unknown,
            "процедура расширения"
        );
        assert_eq!(
            call_context(form, managed, Some("Клентее"), false),
            CallContext::Unknown,
            "опечатка в директиве — не повод судить о контексте"
        );
    }

    /// Обычная форма — проверка выключена целиком; тело модуля — неизвестно.
    #[test]
    fn context_ordinary_form_and_module_level() {
        assert_eq!(
            call_context(
                ModuleKind::Form,
                Some(FormKind::Ordinary),
                Some("НаКлиенте"),
                false
            ),
            CallContext::OrdinaryClient
        );
        assert_eq!(
            call_context(ModuleKind::Form, None, None, false),
            CallContext::Unknown
        );
    }

    /// `#Если` перекрывает любой вид модуля и любую директиву.
    #[test]
    fn preproc_region_beats_module_kind() {
        assert_eq!(
            call_context(ModuleKind::Object, None, None, true),
            CallContext::Unknown
        );
        assert_eq!(
            call_context(
                ModuleKind::Form,
                Some(FormKind::Managed),
                Some("НаСервере"),
                true
            ),
            CallContext::Unknown
        );
    }

    /// Решения по меткам: сервер, клиент, обе стороны, толстый клиент, неполный
    /// список.
    #[test]
    fn verdict_by_marks() {
        assert_eq!(
            verdict(CallContext::Server, &client_only()),
            Some(("на сервере", Confidence::High)),
            "клиентский метод на сервере: метки серверной части от «толстого клиента» не зависят"
        );
        assert_eq!(
            verdict(CallContext::ManagedClient, &server_only()),
            Some(("в клиенте управляемого приложения", Confidence::Medium)),
            "серверный метод в клиенте; в метках есть «толстый клиент»"
        );
        assert_eq!(verdict(CallContext::Server, &server_only()), None);
        assert_eq!(verdict(CallContext::ManagedClient, &client_only()), None);
        // `Both`: достаточно одной стороны.
        let both = marks(&["Сервер", "Тонкий клиент"]);
        assert_eq!(verdict(CallContext::Both, &both), None);
        // `Both`: ни одной из сторон метод не доступен — находка. Метка только
        // «толстый клиент», поэтому уверенность Medium.
        let thick = marks(&["толстый клиент"]);
        assert_eq!(
            verdict(CallContext::Both, &thick),
            Some((
                "ни на сервере, ни в клиенте управляемого приложения",
                Confidence::Medium
            ))
        );
        // Неполный список — молчание.
        let unknown = marks(&["Сервер", "интеграция"]);
        assert_eq!(verdict(CallContext::Server, &unknown), None);
        assert_eq!(
            verdict(CallContext::Server, &Availability::new(Vec::new())),
            None
        );
        // Обычная форма и неизвестный контекст не проверяются.
        assert_eq!(verdict(CallContext::OrdinaryClient, &server_only()), None);
        assert_eq!(verdict(CallContext::Unknown, &server_only()), None);
    }

    /// Клиентский метод без «толстого клиента» — уверенность высокая; серверный
    /// метод без «толстого клиента» в клиенте — тоже высокая.
    #[test]
    fn verdict_without_thick_client_is_high() {
        let thin_only = marks(&["Тонкий клиент", "веб-клиент"]);
        assert_eq!(
            verdict(CallContext::Server, &thin_only),
            Some(("на сервере", Confidence::High))
        );
        let server_no_thick = marks(&["Сервер", "внешнее соединение"]);
        assert_eq!(
            verdict(CallContext::ManagedClient, &server_no_thick),
            Some(("в клиенте управляемого приложения", Confidence::High)),
            "«толстого клиента» в метках нет — трактовка метки ни при чём"
        );
    }
}
