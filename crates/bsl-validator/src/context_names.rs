//! Проверка: имя переменной совпало с членом контекста модуля.
//!
//! `Параметры = Новый Структура;` в модуле управляемой формы выглядит как
//! обычное присваивание локальной переменной. На самом деле `Параметры` —
//! свойство типа `ФормаКлиентскогоПриложения` (только чтение): компилятор 1С
//! молчит, строка компилируется, но в рантайме падает «Поле объекта недоступно
//! для записи (Параметры)». То же в ЛЮБОМ модуле для свойств глобального
//! контекста (`Документы = Новый Массив;`). BSL не резервирует имена контекста
//! как ключевые слова, поэтому `tree-sitter-bsl` разбирает такую строку как
//! обычное присваивание, и без сверки с платформенным контекстом это не
//! диагностируется.
//!
//! Имена членов контекста берутся из [`PlatformIndex`], а не из хардкод-списка:
//! состав членов формы и глобального контекста зависит от версии платформы,
//! и список разошёлся бы со справкой при её обновлении.
//!
//! # Когда имя НЕ занято (проверено на типовой конфигурации, 14905 модулей)
//!
//! Имя разрешается в член контекста только тогда, когда оно нигде не связано
//! локально. Наивная проверка давала 412 находок высокой уверенности, из них
//! настоящих — единицы; остальное отсекают условия:
//!
//! 1. **Параметр процедуры** (`Процедура Обработчик(Знач Результат, Параметры)`).
//!    Параметр — локальное имя, оно перекрывает член контекста; присваивание ему
//!    законно. Так пишет сама 1С в УТ.
//! 2. **Процедура «БезКонтекста»** (`&НаКлиентеНаСервереБезКонтекста`). Контекста
//!    формы там нет: `Элементы = Форма.Элементы;` — идиома БСП (319 мест на УТ).
//! 3. **Обычная (неуправляемая) форма.** Её контекст — другой тип, с другим
//!    составом членов. Распознаётся по отсутствию директив компиляции в модуле:
//!    в обычной форме их не бывает вовсе (30 мест на УТ).
//! 4. **Свойство, доступное для записи** (`Заголовок`, `Модифицированность`).
//!    Присваивание ему компилируется и работает — это штатный способ управлять
//!    формой, а не ошибка (5294 места на УТ).
//! 5. **Объявление `Перем`** — само создаёт локальное имя и перекрывает
//!    контекст (сверено с bsl-analyzer: declared binding законен).
//! 6. **Переменная цикла** (`Для Каждого Параметры Из …`) — связывает имя так
//!    же, как параметр.
//!
//! Метод формы имя тоже НЕ занимает: реквизит формы можно назвать как метод, и
//! платформа разводит данные и вызов (`Закрыть = Ложь;` и `Закрыть();` в соседних
//! строках штатной формы учётной записи ЭДО).
//!
//! # Чего проверка не видит
//!
//! Состав РЕКВИЗИТОВ формы: он описан в метаданных формы, а валидатору дают только
//! текст модуля. Реквизит перекрывает имя контекста, поэтому:
//!
//! - правило A внутри модуля формы выключено (в УТ есть формы с реквизитами
//!   `Метаданные`, `БезопасноеХранилище`, `ПараметрЗапуска` — это законно);
//! - основной реквизит `Объект` не проверяется вовсе: его нет в справке платформы.
//!
//! Правило B остаётся: реквизит с именем свойства формы (`Параметры`, `Элементы`)
//! в 14905 модулях УТ не встретился ни разу — конфигуратор такое имя занять не даёт.

use std::collections::HashSet;

use platform_index::PlatformIndex;

use bsl_parse::{AssignFact, AstFacts, ProcScope};

use crate::expression::{pos_at, Confidence, ExprError, ExprErrorKind};

/// Тип контекста модуля управляемой формы (имя в русской справке).
pub const FORM_TYPE: &str = "ФормаКлиентскогоПриложения";

/// Модуль формы? Определяется по относительному пути модуля.
///
/// Три раскладки, все реальные:
/// - конфигурация/расширение: `.../Forms/<Имя>/Ext/Form/Module.bsl`
/// - общая форма: `.../CommonForms/<Имя>/Ext/Form/Module.bsl`
/// - внешняя обработка/отчёт (v8unpack): `.../Form/<Имя>/Form.obj.bsl`
///
/// Общая форма — такой же модуль формы со своими реквизитами: её пропуск давал
/// ложные находки на реквизите, затеняющем имя контекста (`Документы.Добавить()`
/// в `CommonForms/ДокументыОснованияЭПД`, где `Документы` — таблица формы).
///
/// Регистр и вид слэшей значения не имеют. Управляемая форма это или обычная,
/// по пути не видно — это решается по наличию директив компиляции в тексте.
pub fn is_form_module(module_path: &str) -> bool {
    let p = module_path.replace('\\', "/").to_lowercase();
    config_form_re().is_match(&p) || external_form_re().is_match(&p)
}

fn config_form_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?:^|/)(?:common)?forms/[^/]+/ext/form/module\.bsl$").unwrap()
    })
}

fn external_form_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?:^|/)form/[^/]+/form\.obj\.bsl$").unwrap())
}

/// Имя занято свойством контекста модуля, тип которого ОТЛИЧАЕТСЯ от
/// одноимённого платформенного типа?
///
/// Проверка `Идентификатор.Член` считает голову обращения именем платформенного
/// типа, если такое имя есть в справке. Для свойств контекста это верно не
/// всегда: свойство формы `УсловноеОформление` имеет тип
/// `УсловноеОформлениеКомпоновкиДанных`, а одноимённый платформенный тип
/// `УсловноеОформление` — другой состав членов (issue #11). Подстановка по имени
/// даёт тогда ложную находку и подсказку члена чужого типа.
///
/// Если тип свойства СОВПАДАЕТ с именем (`ПараметрыСеанса`,
/// `КлиентскоеПриложение`, `СредстваКриптографии` — таких имён в справке 15),
/// подстановка по имени верна, и проверка членов остаётся: молчать здесь значило
/// бы терять настоящие опечатки.
///
/// Типы берутся из справки платформы, а не из хардкод-списка: состав свойств
/// зависит от версии платформы. Тип не найден — считаем имя конфликтующим и
/// молчим: неизвестный тип хуже отсутствия проверки.
pub(crate) fn is_context_property_of_other_type(
    index: &PlatformIndex,
    form_module: bool,
    name: &str,
) -> bool {
    let name_lower = name.to_lowercase();
    let declared = match index.find_global_property(name) {
        Some(p) => Some(p.type_name.clone()),
        // Член формы приоритетнее глобального свойства с тем же именем.
        None if form_module => form_property_type(index, &name_lower),
        None => None,
    };
    let Some(raw) = declared else {
        return false; // не свойство контекста — обычный путь разрешения типа
    };
    match crate::scope::primary_type(index, &raw) {
        Some(ty) => !ty.eq_ignore_ascii_case(name),
        None => true,
    }
}

/// Тип свойства управляемой формы по имени — из справки платформы.
fn form_property_type(index: &PlatformIndex, name_lower: &str) -> Option<String> {
    index
        .find_type(FORM_TYPE)?
        .properties
        .iter()
        .find(|p| p.name_ru.to_lowercase() == name_lower)
        .map(|p| p.type_name.clone())
}

/// Найти имена, занятые членом контекста модуля, и эмиттить `ShadowedContextName`.
///
/// Правило A (любой модуль): имя совпало со свойством глобального контекста,
/// доступным только для чтения (`Документы`, `Метаданные`, `Справочники`, …).
///
/// Правило B (только модуль УПРАВЛЯЕМОЙ формы): имя совпало со свойством
/// `ФормаКлиентскогоПриложения`, доступным только для чтения (`Параметры`,
/// `Элементы`, `Команды`, …). Проверяется первым — член формы приоритетнее
/// глобального свойства с тем же именем.
///
/// Оба правила молчат, если имя связано локально: параметр процедуры,
/// переменная `Перем` (включая само объявление) либо переменная цикла.
/// Правило B, кроме того, не применяется в процедурах «БезКонтекста» и в
/// обычных формах — подробности в описании модуля.
///
/// `form_attributes` — имена реквизитов формы (нижний регистр), если вызывающий
/// их знает. Реквизит перекрывает имя контекста, поэтому такие имена из проверки
/// исключаются. Зато становится безопасным правило A внутри модуля формы: когда
/// состав реквизитов известен, молчать «на всякий случай» уже не нужно. `None` —
/// состав неизвестен, правило A внутри форм не применяется (консервативно).
pub(crate) fn check_shadowed_context_names(
    index: &PlatformIndex,
    src: &str,
    facts: &AstFacts,
    form_module: bool,
    form_attributes: Option<&HashSet<String>>,
    errors: &mut Vec<ExprError>,
) {
    // Директив нет — модуль обычной формы; её контекст другого типа, и члены
    // `ФормаКлиентскогоПриложения` там имён не занимают.
    let managed_form = form_module && facts.has_directives;

    // Имена, объявленные через `Перем` вне процедур: видны всему модулю.
    let module_vars: HashSet<&str> = facts
        .assigns
        .iter()
        .filter(|a| a.declaration && scope_of(facts, a.byte).is_none())
        .map(|a| a.name.as_str())
        .collect();

    for fact in &facts.assigns {
        let name_lower = fact.name.to_lowercase();
        let scope = scope_of(facts, fact.byte);

        // Локально связанное имя законно: параметр, `Перем` (само объявление
        // тоже создаёт локальное имя и перекрывает контекст — сверено с
        // bsl-analyzer), переменная цикла.
        if is_bound_locally(facts, scope, &module_vars, &name_lower) {
            continue;
        }

        // Реквизит формы тоже перекрывает имя контекста — и свойство, и метод
        // (в УТ есть формы с реквизитами `Метаданные`, `БезопасноеХранилище`,
        // `Закрыть`). Знаем состав реквизитов — имя из него законно. В
        // процедуре «БезКонтекста» реквизитов формы не существует — там
        // реквизит ничего не перекрывает.
        let in_no_context = scope.is_some_and(|s| s.no_context);
        if !in_no_context && form_attributes.is_some_and(|attrs| attrs.contains(&name_lower)) {
            continue;
        }

        if managed_form && !in_no_context {
            if let Some(form_type) = index.find_type(FORM_TYPE) {
                if form_type.properties.iter().any(|p| {
                    p.readonly
                        && (p.name_ru.to_lowercase() == name_lower
                            || (!p.name_en.is_empty() && p.name_en.to_lowercase() == name_lower))
                }) {
                    emit(
                        errors,
                        src,
                        fact,
                        "свойством ФормаКлиентскогоПриложения (только чтение)",
                    );
                    continue;
                }
            }
        }

        // В модуле формы имя может занимать РЕКВИЗИТ формы, а его состав виден
        // только в метаданных формы, не в тексте модуля. Реквизит перекрывает
        // свойство глобального контекста (в УТ есть формы с реквизитами
        // `Метаданные`, `БезопасноеХранилище`, `ПараметрЗапуска`), поэтому без
        // списка реквизитов правило A внутри формы молчит. Оно включается, если
        // состав реквизитов передан (имена из него отсеяны выше) либо если
        // процедура помечена «БезКонтекста»: реквизитов формы там нет, а
        // глобальный контекст есть.
        let global_visible = !form_module || form_attributes.is_some() || in_no_context;
        if global_visible
            && index
                .find_global_property(&fact.name)
                .is_some_and(|p| p.readonly)
        {
            emit(
                errors,
                src,
                fact,
                "свойством глобального контекста (только чтение)",
            );
        }
    }
}

/// Процедура, в которую попадает байт (объявления уровня модуля — вне всех).
fn scope_of(facts: &AstFacts, byte: usize) -> Option<&ProcScope> {
    facts.procs.iter().find(|p| p.contains(byte))
}

/// Имя уже связано локально: параметр своей процедуры, `Перем` в ней же либо
/// `Перем` на уровне модуля?
fn is_bound_locally(
    facts: &AstFacts,
    scope: Option<&ProcScope>,
    module_vars: &HashSet<&str>,
    name_lower: &str,
) -> bool {
    if module_vars.iter().any(|n| n.to_lowercase() == name_lower) {
        return true;
    }
    // Переменная цикла (`Для Каждого X Из …`, `Для X = …`) тоже связывает
    // имя; в facts у неё нет позиции, поэтому проверяем по имени (как в
    // config_objects::locally_bound_names).
    if facts.loop_vars.contains(name_lower) {
        return true;
    }
    let Some(scope) = scope else {
        return false;
    };
    if scope.params.contains(name_lower) {
        return true;
    }
    facts
        .assigns
        .iter()
        .any(|a| a.declaration && scope.contains(a.byte) && a.name.to_lowercase() == name_lower)
}

/// Собрать сообщение по виду присваивания/объявления и добавить находку.
///
/// Уверенность всегда `High`: имя сверено с реальной справкой платформы,
/// присвоить свойству «только чтение» нельзя, а все известные случаи законного
/// совпадения имён отсечены выше.
fn emit(errors: &mut Vec<ExprError>, src: &str, fact: &AssignFact, member_kind: &str) {
    let (line, col) = pos_at(src, fact.byte);
    let message = format!(
        "Имя '{}' занято {}: локальная переменная не создастся, присваивание упадёт в \
         рантайме («Поле объекта недоступно для записи»). Переименуйте переменную.",
        fact.name, member_kind
    );
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        ExprErrorKind::ShadowedContextName,
        message,
        Confidence::High,
        None,
        Vec::new(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_index::{Method, Property, Type};

    /// Синтетический индекс: глобальные свойства `Документы` (только чтение) и
    /// `РабочаяДата` (запись), тип `ФормаКлиентскогоПриложения` со свойствами
    /// `Параметры` (только чтение) и `Заголовок` (запись) и методом `Закрыть`.
    fn test_index() -> PlatformIndex {
        let mut index = PlatformIndex::new();

        index.global_properties.push(Property {
            name_ru: "Документы".into(),
            name_en: "Documents".into(),
            description: String::new(),
            type_name: String::new(),
            readonly: true,
        });
        index.global_properties.push(Property {
            name_ru: "РабочаяДата".into(),
            name_en: "WorkingDate".into(),
            description: String::new(),
            type_name: String::new(),
            readonly: false,
        });

        index.insert_type(Type {
            name_ru: FORM_TYPE.into(),
            name_en: "ManagedClientApplicationForm".into(),
            description: String::new(),
            methods: vec![Method {
                name_ru: "Закрыть".into(),
                name_en: "Close".into(),
                description: String::new(),
                return_type: String::new(),
                signatures: Vec::new(),
            }],
            properties: vec![
                Property {
                    name_ru: "Параметры".into(),
                    name_en: "Parameters".into(),
                    description: String::new(),
                    type_name: String::new(),
                    readonly: true,
                },
                Property {
                    name_ru: "Заголовок".into(),
                    name_en: "Title".into(),
                    description: String::new(),
                    type_name: String::new(),
                    readonly: false,
                },
            ],
            constructors: Vec::new(),
            enum_values: Vec::new(),
        });

        index
    }

    fn shadowed(src: &str, form_module: bool) -> Vec<ExprError> {
        shadowed_with_attrs(src, form_module, None)
    }

    fn shadowed_with_attrs(
        src: &str,
        form_module: bool,
        form_attributes: Option<&HashSet<String>>,
    ) -> Vec<ExprError> {
        let index = test_index();
        let facts = bsl_parse::collect_facts(src);
        let mut errors = Vec::new();
        check_shadowed_context_names(
            &index,
            src,
            &facts,
            form_module,
            form_attributes,
            &mut errors,
        );
        errors
    }

    fn attrs(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    #[test]
    fn is_form_module_recognizes_both_layouts() {
        assert!(is_form_module(
            "base/Catalogs/Х/Forms/ФормаЭлемента/Ext/Form/Module.bsl"
        ));
        assert!(is_form_module("External/Обработка/Form/Форма/Form.obj.bsl"));
        assert!(!is_form_module("base/Documents/Заказ/Ext/ObjectModule.bsl"));
        assert!(!is_form_module("base/CommonModules/Х/Ext/Module.bsl"));
    }

    #[test]
    fn form_readonly_property_assignment_is_high() {
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n",
            true,
        );
        assert_eq!(errors.len(), 1, "{:?}", errors);
        assert_eq!(errors[0].kind, ExprErrorKind::ShadowedContextName);
        assert_eq!(errors[0].confidence, Confidence::High);
    }

    #[test]
    fn form_property_match_is_case_insensitive() {
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nпараметры = Новый Структура;\nКонецПроцедуры\n",
            true,
        );
        assert_eq!(errors.len(), 1, "{:?}", errors);
    }

    #[test]
    fn form_writable_property_assignment_is_silent() {
        // Заголовок доступен для записи — штатный способ задать заголовок формы.
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nЗаголовок = \"Тест\";\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn form_method_name_assignment_is_silent() {
        // Реквизит формы можно назвать как метод: `Закрыть = Ложь;` и `Закрыть();`
        // сосуществуют в штатных формах УТ.
        let errors = shadowed(
            "&НаКлиенте\nПроцедура Т()\nЗакрыть = Ложь;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn procedure_parameter_frees_the_name() {
        let errors = shadowed(
            "&НаКлиенте\nПроцедура Т(Знач Результат, Параметры)\nПараметры = Новый Структура;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn no_context_procedure_frees_form_member_name() {
        let errors = shadowed(
            "&НаКлиентеНаСервереБезКонтекста\nПроцедура Т(Форма)\nПараметры = Форма.Параметры;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn no_context_procedure_does_not_free_global_property() {
        // Глобальный контекст доступен и в процедуре «БезКонтекста».
        let errors = shadowed(
            "&НаСервереБезКонтекста\nПроцедура Т()\nДокументы = Новый Массив;\nКонецПроцедуры\n",
            true,
        );
        assert_eq!(errors.len(), 1, "{:?}", errors);
    }

    #[test]
    fn ordinary_form_module_is_silent() {
        // Директив компиляции нет — обычная форма, у неё другой контекст.
        let errors = shadowed(
            "Процедура КнопкаНажатие(Элемент)\nПараметры = Новый Структура;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn non_form_module_ignores_form_member() {
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n",
            false,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn global_readonly_property_assignment_is_high_in_any_module() {
        let errors = shadowed(
            "Процедура Т()\nДокументы = Новый Массив;\nКонецПроцедуры\n",
            false,
        );
        assert_eq!(errors.len(), 1, "{:?}", errors);
        assert_eq!(errors[0].confidence, Confidence::High);
    }

    #[test]
    fn known_form_attributes_enable_global_rule_inside_form() {
        // Состав реквизитов передан, `Документы` среди них нет — имя занято
        // глобальным контекстом, присваивание упадёт.
        let errors = shadowed_with_attrs(
            "&НаСервере\nПроцедура Т()\nДокументы = Новый Массив;\nКонецПроцедуры\n",
            true,
            Some(&attrs(&["Объект", "СписокЗаказов"])),
        );
        assert_eq!(errors.len(), 1, "{:?}", errors);
        assert_eq!(errors[0].confidence, Confidence::High);
    }

    #[test]
    fn form_attribute_overrides_context_name() {
        // Реквизит формы назван как свойство глобального контекста — законно.
        let errors = shadowed_with_attrs(
            "&НаСервере\nПроцедура Т(Знач Значение)\nДокументы = Значение;\nКонецПроцедуры\n",
            true,
            Some(&attrs(&["Объект", "Документы"])),
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn form_attribute_overrides_form_property_name() {
        // Гипотетический реквизит с именем свойства формы тоже перекрывает его.
        let errors = shadowed_with_attrs(
            "&НаСервере\nПроцедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n",
            true,
            Some(&attrs(&["Параметры"])),
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn form_module_does_not_check_global_property() {
        // Реквизит формы может называться как свойство глобального контекста
        // (в УТ — `Метаданные`, `БезопасноеХранилище`), а состава реквизитов
        // валидатор не видит. Поэтому в модуле формы правило A молчит.
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nДокументы = Новый Массив;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn global_writable_property_assignment_is_silent() {
        let errors = shadowed(
            "Процедура Т()\nРабочаяДата = ТекущаяДата();\nКонецПроцедуры\n",
            false,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn var_declaration_of_context_name_is_silent() {
        // `Перем` сам создаёт локальное имя и перекрывает контекст
        // (сверено с bsl-analyzer: declared binding — законно), поэтому
        // ни объявление, ни присваивание после него находок не дают.
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nПерем Параметры;\nПараметры = Новый Структура;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn loop_variable_frees_the_name() {
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nДля Каждого Параметры Из Массив Цикл\nПараметры = Новый Структура;\nКонецЦикла;\nКонецПроцедуры\n",
            true,
        );
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn form_property_name_matches_english_too() {
        let errors = shadowed(
            "&НаСервере\nПроцедура Т()\nParameters = Новый Структура;\nКонецПроцедуры\n",
            true,
        );
        assert_eq!(errors.len(), 1, "{:?}", errors);
    }
}
