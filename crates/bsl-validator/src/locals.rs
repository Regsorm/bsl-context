//! Локальные имена модуля: параметры процедур, объявления `Перем`, присваивания
//! и тип, выведенный прямо из конструктора.
//!
//! Нужны проверке `Идентификатор.Член`: она считает голову обращения именем
//! ПЛАТФОРМЕННОГО ТИПА, если такое имя есть в справке. Но имя переменной вполне
//! может совпасть с именем типа — и тогда проверка ругается на члены чужого
//! типа. Живые примеры (issue #11; модули типовой конфигурации и код,
//! сгенерированный моделью):
//!
//! ```bsl
//! Блокировка = Новый БлокировкаДанных;
//! ЭлементБлокировки = Блокировка.Добавить("РегистрСведений.Тест"); // ложная находка
//!
//! ЭлементОтбора = Список.Отбор.Элементы.Добавить(Тип("ЭлементОтбораКомпоновкиДанных"));
//! ЭлементОтбора.ЛевоеЗначение = Новый ПолеКомпоновкиДанных("Статус"); // ложная находка
//! ```
//!
//! `ЭлементОтбора` и `Блокировка` — имена платформенных типов, но здесь это
//! ПЕРЕМЕННЫЕ других типов, и находки «у типа ЭлементОтбора нет члена
//! ЛевоеЗначение» / «у типа Блокировка нет члена Добавить» ложные. Хуже пропуска:
//! `suggestion` предлагает член чужого типа, и правка по подсказке ломает
//! рабочий код.
//!
//! Правило простое: если имя в этой процедуре связано локально (параметр,
//! `Перем`, присваивание) — это переменная, а не тип. Тип для неё берётся из
//! конструктора (`Запрос = Новый Запрос`) либо из вывода типов (`ScopeMap`,
//! уровень ≥ 2), а если типа нет — проверка молчит.

use std::collections::{HashMap, HashSet};

use bsl_parse::{AssignFact, AstFacts, ProcScope};

/// Имена модуля с индексом по имени.
///
/// Индекс строится ОДИН раз на модуль. До него каждый вопрос «это локальное имя?»
/// перебирал ВСЕ присваивания модуля и все переменные циклов, да ещё с
/// `to_lowercase()` на каждом сравнении, а `visible()` внутри дополнительно
/// сканировал список процедур. Замер фаз показал, что проверки занимают 93%
/// времени прогона, и главный вклад был именно здесь: на модуле в 40 тысяч строк
/// это миллионы операций и аллокаций на одну проверку члена.
pub(crate) struct LocalNames<'a> {
    facts: &'a AstFacts,
    /// Присваивания по имени в нижнем регистре; ближайшее выбирается по байту.
    assigns_by_name: HashMap<String, Vec<&'a AssignFact>>,
    /// Переменные циклов по имени: имена в фактах уже в нижнем регистре.
    loop_vars_by_name: HashMap<String, Vec<usize>>,
    /// Имена, объявленные `Перем` на уровне модуля: видны из любой процедуры.
    module_vars: HashSet<String>,
    /// Процедуры, отсортированные по началу, — двоичный поиск по точке вместо
    /// линейного перебора списка процедур на каждый вопрос.
    procs_sorted: Vec<&'a ProcScope>,
}

impl<'a> LocalNames<'a> {
    pub(crate) fn new(facts: &'a AstFacts) -> Self {
        let mut procs_sorted: Vec<&ProcScope> = facts.procs.iter().collect();
        procs_sorted.sort_unstable_by_key(|p| p.byte_start);
        let in_proc = |byte: usize| {
            procs_sorted
                .binary_search_by(|p| {
                    if byte < p.byte_start {
                        std::cmp::Ordering::Greater
                    } else if byte >= p.byte_end {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .is_ok()
        };

        let mut assigns_by_name: HashMap<String, Vec<&AssignFact>> = HashMap::new();
        let mut module_vars: HashSet<String> = HashSet::new();
        for assign in &facts.assigns {
            let key = assign.name.to_lowercase();
            if assign.declaration && !in_proc(assign.byte) {
                module_vars.insert(key.clone());
            }
            assigns_by_name.entry(key).or_default().push(assign);
        }

        let mut loop_vars_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for site in &facts.loop_var_sites {
            loop_vars_by_name
                .entry(site.name.clone())
                .or_default()
                .push(site.byte);
        }

        Self {
            facts,
            assigns_by_name,
            loop_vars_by_name,
            module_vars,
            procs_sorted,
        }
    }

    /// Номер процедуры, содержащей точку; `None` — код в теле модуля.
    fn proc_index(&self, byte: usize) -> Option<usize> {
        let idx = self.procs_sorted.partition_point(|p| p.byte_start <= byte);
        if idx == 0 {
            return None;
        }
        (byte < self.procs_sorted[idx - 1].byte_end).then_some(idx - 1)
    }

    /// Точка `site` — в той же области видимости, что и `query`: та же процедура
    /// либо (для обеих) тело модуля. Заменяет прежние замыкания `in_scope` и
    /// `visible`, которые на каждый вопрос перебирали список процедур.
    fn same_scope(&self, query: usize, site: usize) -> bool {
        self.proc_index(query) == self.proc_index(site)
    }

    /// Тип локальной переменной, если он задан конструктором: `Запрос = Новый Запрос;`.
    ///
    /// Это не вывод типов, а факт из текста: справа стоит `Новый ТипX`. Нужно,
    /// чтобы не потерять проверку членов у переменных, названных по своему типу
    /// (`Запрос`, `Массив`, `Структура` — обычное дело в 1С): их члены проверяются
    /// по типу из конструктора, а не по совпадению имени с типом.
    ///
    /// Берётся БЛИЖАЙШЕЕ присваивание ВЫШЕ точки, а не первое в процедуре
    /// (issue #15, класс 1: после переприсваивания тип не менялся). Если ближайшее
    /// присваивание — не конструктор, тип неизвестен (`None`), и решение принимает
    /// вызывающий код. Если присваивание сделано внутри ветви условного оператора,
    /// а спрашиваем мы после него, возвращаются типы ВСЕХ ветвей: переменная может
    /// иметь любой из них. Если в соседней ветви присваивание без конструктора, тип
    /// неизвестен — `None`, потому что молчание лучше ложной находки.
    ///
    /// Учитывается только присваивание в ТОЙ ЖЕ процедуре (для кода вне процедур —
    /// на уровне модуля): переменные BSL локальны для процедуры, и тип из чужой
    /// процедуры к этой точке отношения не имеет.
    pub(crate) fn constructed_type(&self, byte: usize, name: &str) -> Option<Vec<String>> {
        let name_lower = name.to_lowercase();
        let candidates = self.assigns_by_name.get(&name_lower)?;
        let nearest = candidates
            .iter()
            .filter(|a| a.byte <= byte && self.same_scope(byte, a.byte))
            .max_by_key(|a| a.byte)?;

        // Переменная цикла переопределяет имя с НАЧАЛА тела цикла (issue #36):
        // `Х = Новый Массив; … Для Каждого Х Из С Цикл Х.Значение` — здесь `Х`
        // уже элемент коллекции, а не прежний массив. Тип элемента выводится не
        // всегда (`Соответствие` → `КлючИЗначение`, `Массив` → неизвестен),
        // поэтому после такого переопределения тип считаем НЕИЗВЕСТНЫМ, а не
        // прежним: молчание лучше находки про чужой тип — тот же выбор, что в #29.
        if self
            .loop_vars_by_name
            .get(&name_lower)
            .is_some_and(|sites| {
                sites.iter().any(|site| {
                    nearest.byte < *site && *site <= byte && self.same_scope(byte, *site)
                })
            })
        {
            return None;
        }

        let mut types: Vec<String> = vec![nearest.new_type.as_deref()?.to_string()];

        // Ветви того же условного оператора, уже закрытого к этой точке.
        let block = self
            .facts
            .if_branches
            .iter()
            .filter(|b| b.span.1 <= byte)
            .filter(|b| {
                b.branches
                    .iter()
                    .any(|(s, e)| *s <= nearest.byte && nearest.byte < *e)
            })
            .min_by_key(|b| b.span.1.saturating_sub(b.span.0));
        if let Some(block) = block {
            for a in candidates
                .iter()
                .filter(|a| a.byte <= byte && self.same_scope(byte, a.byte))
            {
                if !block
                    .branches
                    .iter()
                    .any(|(s, e)| *s <= a.byte && a.byte < *e)
                {
                    continue;
                }
                // В соседней ветви тип не из конструктора — не угадываем.
                let t = a.new_type.as_deref()?;
                if !types.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                    types.push(t.to_string());
                }
            }
        }
        Some(types)
    }

    pub(crate) fn has_form_context(&self, byte: usize) -> bool {
        self.proc_index(byte)
            .is_none_or(|index| !self.procs_sorted[index].no_context)
    }

    /// Имя в этой точке — локальная переменная, а не имя типа?
    ///
    /// Да, если это параметр объемлющей процедуры, либо имя ей присваивается
    /// (или объявлено `Перем`) в той же процедуре, либо объявлено `Перем` на
    /// уровне модуля — такие переменные видны во всех процедурах. В теле модуля
    /// (код вне процедур) имя связывает и обычное присваивание: `Перем` там для
    /// этого не обязателен, а местная переменная тела модуля процедурам не видна.
    pub(crate) fn is_local(&self, byte: usize, name: &str) -> bool {
        let name_lower = name.to_lowercase();

        // Переменная уровня модуля видна везде.
        if self.module_vars.contains(&name_lower) {
            return true;
        }

        // Переменная цикла: цикл связывает имя, не порождая присваивания в дереве,
        // поэтому в `assigns` таких имён нет. Область видимости учитываем: имя,
        // связанное циклом в ОДНОЙ процедуре, не должно глушить проверки в другой
        // — иначе модуль с `Для Каждого Поле Из СписокПолей Цикл` теряет проверку
        // члена у ТИПА `Поле` во всех остальных процедурах (замер по выгрузке:
        // 2633 находки на 14943 модулях). Сам случай, ради которого правило
        // нужно (issue #22): `Для Каждого ГруппировкаКолонок Из Список Цикл` —
        // имя совпало с типом-перечислением, и `ГруппировкаКолонок.Значение`
        // получило `unknown_enum_value` с `confidence: high`.
        if let Some(sites) = self.loop_vars_by_name.get(&name_lower) {
            if sites.iter().any(|site| self.same_scope(byte, *site)) {
                return true;
            }
        }

        // Параметр объемлющей процедуры связывает имя так же, как присваивание,
        // и присваиваний у него может не быть вовсе — проверяем ДО раннего выхода
        // по отсутствию одноимённых присваиваний.
        if let Some(index) = self.proc_index(byte) {
            if self.procs_sorted[index].params.contains(&name_lower) {
                return true;
            }
        }

        let Some(candidates) = self.assigns_by_name.get(&name_lower) else {
            return false;
        };
        // Присваивание в той же области видимости: в теле модуля (код вне процедур)
        // `Перем` для этого не обязателен, а местная переменная тела модуля
        // процедурам не видна — это и различает `same_scope`.
        candidates
            .iter()
            .any(|assign| self.same_scope(byte, assign.byte))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bsl_parse::collect_facts;

    /// Байт первого вхождения подстроки — точка, в которой спрашиваем про имя.
    fn byte_of(src: &str, needle: &str) -> usize {
        src.find(needle).expect("подстрока есть в тексте")
    }

    #[test]
    fn assigned_name_is_local() {
        let src = "\
&НаСервере
Процедура Т()
ЭлементОтбора = Новый Структура;
ЭлементОтбора.Поле = 1;
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(locals.is_local(byte_of(src, "ЭлементОтбора.Поле"), "ЭлементОтбора"));
    }

    #[test]
    fn parameter_is_local() {
        let src = "\
&НаСервере
Процедура Т(Отбор)
Отбор.Вставить(\"А\", 1);
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(locals.is_local(byte_of(src, "Отбор.Вставить"), "Отбор"));
    }

    #[test]
    fn module_var_is_local_inside_any_procedure() {
        let src = "\
Перем Отбор;

Процедура Т()
Отбор.Вставить(\"А\", 1);
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(locals.is_local(byte_of(src, "Отбор.Вставить"), "Отбор"));
    }

    #[test]
    fn name_from_other_procedure_is_not_local() {
        let src = "\
Процедура Первая()
Отбор = Новый Структура;
КонецПроцедуры

Процедура Вторая()
Отбор.Вставить(\"А\", 1);
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(!locals.is_local(byte_of(src, "Отбор.Вставить"), "Отбор"));
    }

    #[test]
    fn assignment_in_module_body_is_local_for_module_body() {
        // Код вне процедур: `Перем` не обязателен, присваивание связывает имя
        // здесь же. Воспроизведение 1 из issue #11 записано именно так.
        let src = "\
Блокировка = Новый БлокировкаДанных;
Блокировка.Заблокировать();
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        let byte = byte_of(src, "Блокировка.Заблокировать");
        assert!(locals.is_local(byte, "Блокировка"));
        assert_eq!(
            locals.constructed_type(byte, "Блокировка"),
            Some(vec!["БлокировкаДанных".to_string()])
        );
    }

    #[test]
    fn module_body_assignment_does_not_leak_into_procedure() {
        // Присваивание в теле модуля процедуре не видно: там своя область.
        let src = "\
Блокировка = Новый БлокировкаДанных;

Процедура Т()
Блокировка.Заблокировать();
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        let byte = byte_of(src, "Блокировка.Заблокировать");
        assert!(!locals.is_local(byte, "Блокировка"));
    }

    #[test]
    fn constructed_type_is_taken_from_new_expression() {
        // Переменная названа по своему типу — обычное дело в 1С. Тип берём из
        // конструктора, поэтому её члены по-прежнему проверяются.
        let src = "\
&НаСервере
Процедура Т()
Запрос = Новый Запрос;
Запрос.Текст = \"ВЫБРАТЬ 1\";
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        let byte = byte_of(src, "Запрос.Текст");
        assert!(locals.is_local(byte, "Запрос"));
        assert_eq!(
            locals.constructed_type(byte, "Запрос"),
            Some(vec!["Запрос".to_string()])
        );
    }

    /// Issue #36, класс 2: переменная цикла переопределяет имя с НАЧАЛА тела цикла,
    /// поэтому тип из присваивания ВЫШЕ цикла к телу уже не относится. До правки
    /// `Х = Новый Массив; … Для Каждого Х Из С Цикл Х.Значение` давало находку про
    /// тип `Массив` — ложную (в реальности `Х` — элемент соответствия).
    #[test]
    fn loop_variable_resets_constructed_type() {
        let src = "Процедура Т()\n\tХ = Новый Массив;\n\tС = Новый Соответствие;\n\tДля Каждого Х Из С Цикл\n\t\tЗ = Х.Значение;\n\tКонецЦикла;\nКонецПроцедуры\n";
        let facts = bsl_parse::collect_facts(src);
        let locals = LocalNames::new(&facts);

        let inside = byte_of(src, "Х.Значение");
        assert_eq!(
            locals.constructed_type(inside, "Х"),
            None,
            "после заголовка цикла прежний тип не действует"
        );

        // Контроль: ВЫШЕ цикла прежний тип ещё действует — правило не выключает
        // проверку вообще, а только с начала тела цикла.
        let before = byte_of(src, "С = Новый Соответствие");
        assert_eq!(
            locals.constructed_type(before, "Х"),
            Some(vec!["Массив".to_string()])
        );
    }

    #[test]
    fn constructor_type_of_other_procedure_is_not_used() {
        // Тип из чужой процедуры к этой точке отношения не имеет.
        let src = "\
Процедура Первая()
Запрос = Новый Запрос;
КонецПроцедуры

Процедура Вторая()
Запрос.Текст = \"ВЫБРАТЬ 1\";
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        let byte = byte_of(src, "Запрос.Текст");
        assert!(!locals.is_local(byte, "Запрос"));
        assert_eq!(locals.constructed_type(byte, "Запрос"), None);
    }

    #[test]
    fn no_constructor_means_unknown_type() {
        // Справа не конструктор — тип неизвестен, проверять члены не по чему.
        let src = "\
&НаСервере
Процедура Т()
ЭлементОтбора = Список.Отбор.Элементы.Добавить();
ЭлементОтбора.ЛевоеЗначение = 1;
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        let byte = byte_of(src, "ЭлементОтбора.ЛевоеЗначение");
        assert!(locals.is_local(byte, "ЭлементОтбора"));
        assert_eq!(locals.constructed_type(byte, "ЭлементОтбора"), None);
    }

    #[test]
    fn platform_type_name_without_assignment_is_not_local() {
        // `Справочники.Контрагенты` — обращение к глобальному свойству, не к переменной.
        let src = "\
Процедура Т()
Ссылка = Справочники.Контрагенты.ПустаяСсылка();
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(!locals.is_local(byte_of(src, "Справочники.Контрагенты"), "Справочники"));
    }

    /// Issue #22: переменная цикла, названная как системное перечисление.
    ///
    /// `ГруппировкаКолонок` есть в справке как тип-перечисление, но здесь это
    /// переменная цикла — проверку значений перечисления вести нельзя, иначе
    /// корректный код получает `unknown_enum_value` с `confidence: high`.
    #[test]
    fn loop_variable_is_local() {
        let src = "\
&НаСервере
Процедура Т(Список)
Для Каждого ГруппировкаКолонок Из Список Цикл
Х = ГруппировкаКолонок.Значение;
КонецЦикла;
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(
            locals.is_local(
                byte_of(src, "ГруппировкаКолонок.Значение"),
                "ГруппировкаКолонок"
            ),
            "переменная цикла обязана считаться локальным именем"
        );
    }

    /// Переменная цикла видна и в теле модуля (код вне процедур).
    #[test]
    fn loop_variable_in_module_body_is_local() {
        let src = "\
Для Каждого Значение Из Список Цикл
Значение.Свойство = 1;
КонецЦикла;
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(locals.is_local(byte_of(src, "Значение.Свойство"), "Значение"));
    }

    /// Область видимости переменной цикла учитывается: имя, связанное циклом в
    /// ОДНОЙ процедуре, не глушит проверки в другой.
    ///
    /// На замере по выгрузке (14943 модуля) модуль-широкое правило стоило 2633
    /// находок: модуль с `Для Каждого Поле Из СписокПолей Цикл` терял проверку
    /// члена у ТИПА `Поле` во всех остальных процедурах, хотя внутри цикла
    /// `Поле.Ключ` — законное обращение к элементу коллекции.
    #[test]
    fn loop_variable_does_not_leak_into_another_procedure() {
        let src = "\
Процедура Первая(Список)
Для Каждого Поле Из Список Цикл
КонецЦикла;
КонецПроцедуры

Процедура Вторая()
Поле.Ключ = 1;
КонецПроцедуры
";
        let facts = collect_facts(src);
        let locals = LocalNames::new(&facts);
        assert!(
            !locals.is_local(byte_of(src, "Поле.Ключ"), "Поле"),
            "имя, связанное циклом в чужой процедуре, локальным не считается"
        );
    }
}
