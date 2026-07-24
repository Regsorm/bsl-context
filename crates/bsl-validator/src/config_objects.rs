//! Проверка: код обращается к объекту конфигурации, которого нет.
//!
//! `УправлениеПрогрессом.Установить(1, 0)` и
//! `Справочники.НесуществующийСправочник.ПустаяСсылка()` — самый частый класс
//! ошибок слабой модели. Справка платформы о конфигурации ничего не знает,
//! поэтому имена сверяются с внешним источником ([`crate::symbols::SymbolSource`]).
//!
//! # Почему так много условий молчания
//!
//! Голова обращения `Имя.Член` — не обязательно имя общего модуля. Это может
//! быть реквизит объекта (`Товары.Очистить()` в модуле объекта), реквизит формы,
//! член контекста формы (`Элементы`), локальная переменная. Состава реквизитов
//! объекта у валидатора нет вовсе, состав реквизитов формы — только если его
//! передали. Поэтому правило про общий модуль работает лишь там, где неявного
//! контекста объекта заведомо нет (общий модуль, голый фрагмент, форма с
//! известными реквизитами), а во всех прочих модулях молчит.

use std::collections::HashSet;

use platform_index::{PlatformIndex, Type};

use bsl_parse::AstFacts;

use crate::context_names::FORM_TYPE;
use crate::expression::{fuzzy_confidence_for, lev, pos_at, Confidence, ExprError, ExprErrorKind};
use crate::symbols::SymbolSource;

/// Менеджер объектов конфигурации в коде → (коллекция каталога выгрузки,
/// префикс типа-менеджера ОБЪЕКТА в справке платформы).
///
/// Только русские имена: конфигурации, с которыми работает сервер, русские.
/// Таблица сверена с живым индексом; в частности, три плана в `meta_type`
/// остаются во множественном числе — соответствие держит symbol-source.
///
/// Третий столбец — префикс типа менеджера КОНКРЕТНОГО объекта
/// (`Справочники.Х` → тип `СправочникМенеджер.<Имя справочника>`). По нему
/// [`manager_object_type`] находит тип в справке и берёт его методы для проверки
/// вызова `Коллекция.Объект.Метод(...)`. Префикс в единственном числе (тип
/// одного объекта), тогда как коллекция глобального контекста — во множественном
/// (`СправочникиМенеджер`). Если префикс в справке не найден — проверка метода
/// молчит (безопасный отказ, без ложной находки).
const MANAGER_COLLECTIONS: &[(&str, &str, &str)] = &[
    ("Справочники", "Catalogs", "СправочникМенеджер"),
    ("Документы", "Documents", "ДокументМенеджер"),
    ("РегистрыСведений", "InformationRegisters", "РегистрСведенийМенеджер"),
    ("РегистрыНакопления", "AccumulationRegisters", "РегистрНакопленияМенеджер"),
    ("РегистрыБухгалтерии", "AccountingRegisters", "РегистрБухгалтерииМенеджер"),
    ("РегистрыРасчета", "CalculationRegisters", "РегистрРасчетаМенеджер"),
    ("Перечисления", "Enums", "ПеречислениеМенеджер"),
    ("ПланыВидовХарактеристик", "ChartsOfCharacteristicTypes", "ПланВидовХарактеристикМенеджер"),
    ("ПланыСчетов", "ChartsOfAccounts", "ПланСчетовМенеджер"),
    ("ПланыВидовРасчета", "ChartsOfCalculationTypes", "ПланВидовРасчетаМенеджер"),
    ("БизнесПроцессы", "BusinessProcesses", "БизнесПроцессМенеджер"),
    ("Задачи", "Tasks", "ЗадачаМенеджер"),
    ("ПланыОбмена", "ExchangePlans", "ПланОбменаМенеджер"),
    ("Константы", "Constants", "КонстантаМенеджер"),
    ("Обработки", "DataProcessors", "ОбработкаМенеджер"),
    ("Отчеты", "Reports", "ОтчетМенеджер"),
    ("ЖурналыДокументов", "DocumentJournals", "ЖурналДокументовМенеджер"),
    ("КритерииОтбора", "FilterCriteria", "КритерийОтбораМенеджер"),
    ("Последовательности", "Sequences", "ПоследовательностьМенеджер"),
];

/// Имена контекста, которых нет в справке платформы, но которые реальны в коде:
/// основной реквизит формы и сам объект. Через них идёт обращение к реквизитам
/// (`Объект.Товары`), головой общего модуля они не бывают.
const CONTEXT_HEADS: &[&str] = &["Объект", "ЭтотОбъект", "ЭтаФорма", "Форма"];

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_config_objects(
    index: &PlatformIndex,
    src: &str,
    facts: &AstFacts,
    module_path: Option<&str>,
    form_module: bool,
    form_attributes: Option<&HashSet<String>>,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    let Some(symbols) = symbols else {
        return;
    };

    // Состав реквизитов формы неизвестен — в модуле формы молчим совсем: любое
    // имя может оказаться реквизитом. Тот же приём, что у правила A в
    // context_names.rs (там он снял основную массу ложных находок).
    if form_module && form_attributes.is_none() {
        return;
    }

    let bound = locally_bound_names(facts);
    let managed_form = form_module && facts.has_directives;

    // Правило про общий модуль работает, ТОЛЬКО если источник знает экспортные
    // переменные модуля приложения (`ПараметрыПриложения`). Они видны без
    // префикса отовсюду, и не зная их, правило принимает
    // `ПараметрыПриложения.Вставить(...)` за обращение к несуществующему модулю
    // — на УТ это 123 ложные находки на одном имени. `None` (источник не умеет
    // или недоступен) → правило молчит целиком. На правило про объекты
    // конфигурации это не влияет: там голова — менеджер платформы.
    let global_vars = symbols.global_variables();
    let common_module_context =
        global_vars.is_some() && has_no_object_context(module_path, form_module);

    for dot in &facts.dots {
        let head_lower = dot.head.to_lowercase();

        // Имя связано локально (`Перем Х` или `Х = ...`) — это переменная,
        // а не объект конфигурации.
        if bound.contains(&head_lower) {
            continue;
        }
        // Реквизит формы перекрывает любое имя.
        if form_attributes.is_some_and(|a| a.contains(&head_lower)) {
            continue;
        }

        // ── (б) Менеджер объектов конфигурации: `Справочники.Имя` ──
        if let Some(collection) = collection_for_manager(&dot.head) {
            // Член после менеджера — не обязательно имя объекта: у самого
            // менеджера есть методы (`ПланыОбмена.ГлавныйУзел()`,
            // `Справочники.ТипВсеСсылки()`). Замер на УТ: без этого условия
            // 967 ложных находок, все — методы менеджеров.
            if manager_type_has_member(index, &dot.head, &dot.member) {
                continue;
            }
            let member_lower = dot.member.to_lowercase();
            if symbols.object_exists(collection, &member_lower) == Some(false) {
                emit(
                    errors,
                    src,
                    dot.member_byte,
                    ExprErrorKind::UnknownMetadataObject,
                    format!("Объект '{}' не существует в '{}'.", dot.member, dot.head),
                    suggestion_for(symbols, collection, &dot.member),
                );
            }
            continue;
        }

        // ── (а) Общий модуль: `ИмяМодуля.Метод()` ──
        if !common_module_context {
            continue;
        }
        // Процедуру общего модуля можно только ВЫЗВАТЬ — свойств у него не
        // бывает. Обращение к свойству (`ТипЭлементаФорматированногоДокумента.
        // ПереводСтроки`) общим модулем быть не может: это значение
        // платформенного перечисления, и если справка о нём не знает, молчание
        // — единственный честный ответ (в справке 8.3.27 такого типа нет).
        if !dot.member_is_call {
            continue;
        }
        // Голова, не являющаяся идентификатором, — мусор восстановления дерева
        // после ошибки: в самой УТ есть строки вида `X = "1.2.643" "1.2.643"`
        // (два литерала подряд), где разбор выдаёт головой `1`.
        if !is_identifier_like(&dot.head) {
            continue;
        }
        // Экспортная переменная модуля приложения: видна без префикса отовсюду.
        if global_vars
            .as_ref()
            .is_some_and(|vars| vars.contains(&head_lower))
        {
            continue;
        }
        // Сравнение через to_lowercase(), а не eq_ignore_ascii_case: ASCII-функция
        // не сворачивает регистр кириллицы (тот же нюанс, что у
        // collection_for_manager ниже) — иначе «объект» с маленькой буквы не
        // совпал бы с константой таблицы.
        if CONTEXT_HEADS.iter().any(|n| n.to_lowercase() == head_lower) {
            continue;
        }
        if index.find_type(&dot.head).is_some() {
            continue;
        }
        if index.find_global_property(&dot.head).is_some() {
            continue;
        }
        // Член контекста управляемой формы (`Элементы`, `Параметры`, `Команды`).
        if managed_form && form_type_has_member(index, &dot.head) {
            continue;
        }
        if symbols.object_exists("CommonModules", &head_lower) == Some(false) {
            emit(
                errors,
                src,
                dot.head_byte,
                ExprErrorKind::UnknownCommonModule,
                format!("Общий модуль '{}' не существует в конфигурации.", dot.head),
                suggestion_for(symbols, "CommonModules", &dot.head),
            );
        }
    }

    // ── (в) Метод у менеджера объекта: `Справочники.Сотрудники.НайтиПоРеквизиту(...)` ──
    // Получатель метода — двухсегментная голова `Коллекция.Объект`, поэтому вызов
    // попал не в `facts.dots`, а в `facts.manager_calls` (см. `ManagerCallFact`).
    for call in &facts.manager_calls {
        let collection_lower = call.collection.to_lowercase();
        // Имя коллекции связано локально или перекрыто реквизитом формы —
        // это переменная, а не менеджер объектов конфигурации.
        if bound.contains(&collection_lower) {
            continue;
        }
        if form_attributes.is_some_and(|a| a.contains(&collection_lower)) {
            continue;
        }
        let Some((collection, prefix)) = manager_collection_with_prefix(&call.collection) else {
            continue; // голова — не менеджер объектов конфигурации
        };
        // Объект должен реально существовать. Если нет — первичная ошибка это сам
        // объект (её даёт ветка (б) как `UnknownMetadataObject`); метод не трогаем,
        // чтобы не выдать вторую находку на ту же строку. `None` (источник не знает)
        // тоже пропускаем: без подтверждённого объекта проверять метод небезопасно.
        let object_lower = call.object.to_lowercase();
        if symbols.object_exists(collection, &object_lower) != Some(true) {
            continue;
        }
        // Тип-менеджер объекта из справки платформы (`СправочникМенеджер.<Имя>`).
        let Some(manager_type) = manager_object_type(index, prefix) else {
            continue; // вид менеджера не описан в справке — молчим
        };
        let method_lower = call.method.to_lowercase();
        // Метод есть у менеджера в справке (или это его свойство) — законный вызов.
        if type_has_member(manager_type, &method_lower) {
            continue;
        }
        // Метод объявлен где-то в конфигурации — почти всегда экспорт модуля
        // менеджера этого объекта (`Справочники.Валюты.ЗагрузитьКурсы()`), которого
        // справка платформы не знает. Это НЕ опечатка — молчим. Главный отсекатель
        // ложных находок на реальном коде.
        if symbols.method_exists(&method_lower) {
            continue;
        }
        // Осталось: метода нет ни у менеджера в справке, ни в конфигурации. Находкой
        // считаем, только если имя близко к настоящему методу менеджера — тогда это
        // опечатка (`НайтиПоРеквизитам` → `НайтиПоРеквизиту`). Далёкое имя молча
        // пропускаем: возможна невидимая источнику процедура, ложная находка хуже.
        let Some((suggestion, confidence)) = closest_manager_method(manager_type, &call.method)
        else {
            continue;
        };
        let (line, col) = pos_at(src, call.method_byte);
        errors.push(ExprError::new_with_confidence(
            line,
            col,
            ExprErrorKind::UnknownManagerMethod,
            // Имя из самого кода (`Справочники.Номенклатура`), а НЕ `manager_type.name_ru`:
            // у шаблонного типа справки оно вида «СправочникМенеджер.<Имя справочника>
            // (CatalogManager.<Catalog name>)» — в сообщении это мусор.
            format!(
                "У '{}.{}' нет метода '{}'. Возможно, вы имели в виду '{}'.",
                call.collection, call.object, call.method, suggestion
            ),
            confidence,
            Some(suggestion),
            Vec::new(),
        ));
    }
}

/// Имена, связанные локально ГДЕ-ЛИБО в модуле: любое присваивание простому
/// идентификатору (`Перем Х` — `declaration=true`, и обычное `Х = ...` —
/// `declaration=false`, оно тоже связывает имя, даже без предшествующего
/// `Перем`), переменные циклов и параметры всех процедур/функций модуля.
/// Область видимости намеренно НЕ учитывается: цена несимметрична — лишнее
/// молчание (имя связано в другой процедуре модуля) безвредно, а пропуск
/// реально связанного где-то имени даёт ложную находку.
///
/// Переменные циклов — самый массовый источник: замер на УТ без них дал 39431
/// ложную находку (`Для Каждого КлючЗначение Из ... Цикл КлючЗначение.Ключ`),
/// потому что цикл связывает имя, не порождая присваивания в дереве.
fn locally_bound_names(facts: &AstFacts) -> HashSet<String> {
    let mut names: HashSet<String> = facts
        .assigns
        .iter()
        .map(|a| a.name.to_lowercase())
        .collect();
    names.extend(facts.loop_vars.iter().cloned());
    for proc in &facts.procs {
        names.extend(proc.params.iter().cloned());
    }
    names
}

/// Член принадлежит самому МЕНЕДЖЕРУ, а не является именем объекта?
///
/// `ПланыОбмена.ГлавныйУзел()` и `Справочники.ТипВсеСсылки()` синтаксически
/// неотличимы от `Справочники.Номенклатура`, но это методы типа-менеджера.
/// Тип берётся из справки: у глобального свойства `Справочники` это
/// `СправочникиМенеджер`.
///
/// Имя типа в справке обёрнуто в обратные кавычки (`` `СправочникиМенеджер` ``) —
/// без их снятия `find_type` не находит тип, гейт молча не срабатывает и все
/// методы менеджеров возвращаются ложными находками.
fn manager_type_has_member(index: &PlatformIndex, head: &str, member: &str) -> bool {
    let Some(property) = index.find_global_property(head) else {
        return false;
    };
    let Some(manager_type) = index.find_type(property.type_name.trim_matches('`')) else {
        return false;
    };
    let member_lower = member.to_lowercase();
    manager_type
        .methods
        .iter()
        .any(|m| m.name_ru.to_lowercase() == member_lower || m.name_en.to_lowercase() == member_lower)
        || manager_type
            .properties
            .iter()
            .any(|p| p.name_ru.to_lowercase() == member_lower || p.name_en.to_lowercase() == member_lower)
}

/// Похоже на идентификатор BSL: начинается с буквы или подчёркивания.
/// Дерево tree-sitter после ошибки разбора выдаёт головой что угодно, включая
/// числа, — такую «голову» проверять бессмысленно.
fn is_identifier_like(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// Найти коллекцию каталога выгрузки по имени менеджера объектов в коде.
/// Сравнение — только через `to_lowercase()` обеих строк: `eq_ignore_ascii_case`
/// не сворачивает регистр кириллицы (ASCII-функция), поэтому «справочники» с
/// маленькой буквы не совпал бы с константой таблицы.
fn collection_for_manager(head: &str) -> Option<&'static str> {
    let head_lower = head.to_lowercase();
    MANAGER_COLLECTIONS
        .iter()
        .find(|(name, _, _)| name.to_lowercase() == head_lower)
        .map(|(_, collection, _)| *collection)
}

/// Как [`collection_for_manager`], но возвращает ещё и префикс типа-менеджера
/// объекта (третий столбец таблицы) — нужен проверке вызова метода менеджера.
fn manager_collection_with_prefix(head: &str) -> Option<(&'static str, &'static str)> {
    let head_lower = head.to_lowercase();
    MANAGER_COLLECTIONS
        .iter()
        .find(|(name, _, _)| name.to_lowercase() == head_lower)
        .map(|(_, collection, prefix)| (*collection, *prefix))
}

/// Тип менеджера конкретного объекта вида по префиксу справки платформы:
/// `СправочникМенеджер` → тип `СправочникМенеджер.<Имя справочника>`. В справке
/// такой шаблонный тип на каждый вид ровно один — берём первый по префиксу.
/// `None` — вид в справке не описан (проверка обязана промолчать).
///
/// Ключи `index.types` — `name_ru` в нижнем регистре; ищем начинающийся на
/// `<префикс>.` (точка обязательна: `СправочникМенеджер.` не должен совпасть
/// с гипотетическим `СправочникМенеджерЧтоТо`).
fn manager_object_type<'a>(index: &'a PlatformIndex, prefix: &str) -> Option<&'a Type> {
    let needle = format!("{}.", prefix.to_lowercase());
    index
        .types
        .iter()
        .find(|(key, _)| key.starts_with(&needle))
        .map(|(_, ty)| ty)
}

/// Есть ли у типа член (метод или свойство) с таким именем (регистронезависимо,
/// оба языка)?
fn type_has_member(ty: &Type, member_lower: &str) -> bool {
    ty.methods.iter().any(|m| {
        m.name_ru.to_lowercase() == member_lower || m.name_en.to_lowercase() == member_lower
    }) || ty.properties.iter().any(|p| {
        p.name_ru.to_lowercase() == member_lower || p.name_en.to_lowercase() == member_lower
    })
}

/// Ближайший МЕТОД типа-менеджера к `name` с уверенностью по двухпороговой
/// эвристике [`fuzzy_confidence_for`] (та же, что у `UnknownGlobalMethod`).
/// `None` — ни на один метод не похоже: это не опечатка платформенного метода,
/// а, скорее всего, метод модуля менеджера, которого справка не знает, — молчим.
/// Сверяется только русское имя: методы менеджеров в справке англ. имени не имеют.
fn closest_manager_method(ty: &Type, name: &str) -> Option<(String, Confidence)> {
    let name_lower = name.to_lowercase();
    let mut best: Option<(String, usize)> = None;
    for m in &ty.methods {
        let distance = lev(&name_lower, &m.name_ru.to_lowercase());
        match &best {
            Some((_, best_distance)) if distance >= *best_distance => {}
            _ => best = Some((m.name_ru.clone(), distance)),
        }
    }
    let (candidate, distance) = best?;
    let confidence = fuzzy_confidence_for(name, &candidate, distance)?;
    Some((candidate, confidence))
}

/// Заведомо ОТСУТСТВУЕТ неявный контекст объекта — то есть правило про общий
/// модуль применимо?
///
/// true только для трёх случаев: `module_path` не передан (голый фрагмент —
/// модуль не назван, кода объектного модуля тут не предполагается), это путь
/// общего модуля (у него нет собственных реквизитов вовсе), либо это модуль
/// формы (сюда попадаем только когда реквизиты формы уже известны — см.
/// проверку в начале `check_config_objects`). Во всех прочих модулях (объекта,
/// набора записей, менеджера значения) реквизиты объекта доступны без префикса
/// (`Товары.Очистить()`), а их состава у валидатора нет вовсе — там правило
/// молчит.
fn has_no_object_context(module_path: Option<&str>, form_module: bool) -> bool {
    match module_path {
        None => true,
        Some(path) => is_common_module(path) || form_module,
    }
}

/// Путь модуля — общий модуль конфигурации/расширения
/// (`.../CommonModules/<Имя>/Ext/Module.bsl`)? Регистр и вид слэшей значения
/// не имеют — тот же приём, что у `is_form_module` в `context_names.rs`.
fn is_common_module(module_path: &str) -> bool {
    let p = module_path.replace('\\', "/").to_lowercase();
    common_module_re().is_match(&p)
}

fn common_module_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?:^|/)commonmodules/[^/]+/ext/module\.bsl$").unwrap())
}

/// Есть ли у типа `ФормаКлиентскогоПриложения` свойство или метод с таким
/// именем (регистронезависимо)? Проверяем оба списка: голова обращения может
/// оказаться и свойством контекста формы (`Элементы`), и его методом
/// (`Закрыть`) — для головы обращения важно любое совпадение.
fn form_type_has_member(index: &PlatformIndex, name: &str) -> bool {
    let Some(form_type) = index.find_type(FORM_TYPE) else {
        return false;
    };
    let name_lower = name.to_lowercase();
    form_type
        .properties
        .iter()
        .any(|p| p.name_ru.to_lowercase() == name_lower)
        || form_type
            .methods
            .iter()
            .any(|m| m.name_ru.to_lowercase() == name_lower)
}

/// Ближайшее имя из коллекции по расстоянию Левенштейна — но только когда это
/// правдоподобная опечатка (двухпороговая эвристика `fuzzy_confidence_for`,
/// та же, что у `UnknownGlobalMethod`).
///
/// Выдуманное имя (`УправлениеПрогрессом`) не похоже ни на одно реальное —
/// `fuzzy_confidence_for` в этом случае вернёт `None`, и подсказки не будет.
/// Это правильно: ложная подсказка хуже её отсутствия, слабая модель воспримет
/// её как подтверждённый факт.
fn suggestion_for(symbols: &dyn SymbolSource, collection: &str, name: &str) -> Option<String> {
    let names = symbols.collection_names(collection)?;
    let name_lower = name.to_lowercase();
    let mut best: Option<(String, usize)> = None;
    for candidate in &names {
        let distance = lev(&name_lower, &candidate.to_lowercase());
        match &best {
            Some((_, best_distance)) if distance >= *best_distance => {}
            _ => best = Some((candidate.clone(), distance)),
        }
    }
    let (candidate, distance) = best?;
    fuzzy_confidence_for(name, &candidate, distance)?;
    Some(candidate)
}

/// Собрать сообщение с хвостом-подсказкой (если она есть) и добавить находку.
/// Формат хвоста — тот же, что у `check_type_dot_members` в `expression.rs`:
/// единообразный вид сообщений для потребителя. `Confidence` берётся явно из
/// `kind.confidence()` (единый источник истины), а не через `ExprError::new`:
/// конструктор модуль-приватный (виден только внутри `expression.rs`), менять
/// его видимость правкой не предусмотрено — расширять список публичных
/// точек входа сверх заказанного (`lev`) нежелательно.
fn emit(
    errors: &mut Vec<ExprError>,
    src: &str,
    byte: usize,
    kind: ExprErrorKind,
    message: String,
    suggestion: Option<String>,
) {
    let (line, col) = pos_at(src, byte);
    let tail = suggestion
        .as_ref()
        .map(|s| format!(" Возможно, вы имели в виду '{s}'."))
        .unwrap_or_default();
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        kind,
        format!("{message}{tail}"),
        kind.confidence(),
        suggestion,
        Vec::new(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Справка платформы, если она доступна в окружении.
    fn real_index() -> Option<PlatformIndex> {
        let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH").ok()?;
        let root = std::path::Path::new(&root);
        let hbk = [
            root.join("shcntx_ru.hbk"),
            root.join("bin").join("shcntx_ru.hbk"),
        ]
        .into_iter()
        .find(|p| p.exists())?;
        platform_index::load_from_hbk(&hbk).ok()
    }

    /// КАЖДЫЙ префикс типа-менеджера из `MANAGER_COLLECTIONS` разрешается в тип
    /// настоящей справки. Иначе проверка метода для этого вида молча не сработает
    /// (не ложная находка, но упущенная опечатка) — префикс сверен с 1С неверно.
    /// `#[ignore]`: нужен `BSL_CONTEXT_PLATFORM_PATH`.
    #[test]
    #[ignore]
    fn every_manager_prefix_resolves_on_real_index() {
        let Some(index) = real_index() else {
            eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
            return;
        };
        for (collection, _, prefix) in MANAGER_COLLECTIONS {
            assert!(
                manager_object_type(&index, prefix).is_some(),
                "префикс '{prefix}' (коллекция {collection}) не разрешился в тип-менеджер справки",
            );
        }
    }
}
