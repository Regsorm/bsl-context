//! Правила оптимальности запросов.
//!
//! Метаданных конфигурации этим правилам не нужно, поэтому источник имён здесь
//! не участвует вовсе — достаточно пустого индекса платформы.

use bsl_validator::{
    validate_module_with_profile, validate_module_with_symbols, ExprErrorKind, ObjectField,
    ObjectSchema, Profile, SymbolSource,
};
use platform_index::PlatformIndex;
use std::collections::HashSet;

/// Источник с составом двух объектов: регистр остатков и справочник.
///
/// Режим `silent()` отвечает `None` на всё — им проверяется главный контракт:
/// «не знаю состав» обязано означать молчание, а не находку.
struct StubSource {
    silent: bool,
}

impl StubSource {
    fn new() -> Self {
        Self { silent: false }
    }

    fn silent() -> Self {
        Self { silent: true }
    }
}

impl SymbolSource for StubSource {
    fn method_exists(&self, _name_lower: &str) -> bool {
        true
    }

    fn object_exists(&self, _collection: &str, _name_lower: &str) -> Option<bool> {
        Some(true)
    }

    fn global_variables(&self) -> Option<HashSet<String>> {
        Some(HashSet::new())
    }

    fn object_schema(&self, collection: &str, name_lower: &str) -> Option<ObjectSchema> {
        if self.silent {
            return None;
        }
        let field = |name: &str, indexed: bool| ObjectField {
            name: name.to_string(),
            indexing: indexed.then(|| "Index".to_string()),
        };
        match (collection, name_lower) {
            ("AccumulationRegisters", "товарынаскладах") => Some(ObjectSchema {
                // Реквизит регистра: в виртуальных таблицах его нет.
                attributes: vec![field("Сторно", false)],
                dimensions: vec![field("Номенклатура", false), field("Склад", false)],
                resources: vec![field("ВНаличии", false)],
                register_type: Some("Balance".to_string()),
                ..Default::default()
            }),
            ("AccumulationRegisters", "продажи") => Some(ObjectSchema {
                dimensions: vec![field("Номенклатура", false)],
                register_type: Some("Turnovers".to_string()),
                ..Default::default()
            }),
            ("Catalogs", "товары") => Some(ObjectSchema {
                // «Сделка» индексирована, «Комментарий» — нет.
                attributes: vec![field("Сделка", true), field("Комментарий", false)],
                ..Default::default()
            }),
            _ => None,
        }
    }

    fn describe(&self) -> String {
        "stub".to_string()
    }
}

fn kinds_with(src: &str, source: &StubSource) -> Vec<ExprErrorKind> {
    let index = PlatformIndex::new();
    validate_module_with_symbols(&index, src, 1, Profile::Full, None, None, Some(source))
        .errors
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

fn has_with(src: &str, source: &StubSource, kind: ExprErrorKind) -> bool {
    kinds_with(src, source).contains(&kind)
}

fn kinds(src: &str) -> Vec<ExprErrorKind> {
    let index = PlatformIndex::new();
    validate_module_with_profile(&index, src, None, None, 1, Profile::Full)
        .errors
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

fn has(src: &str, kind: ExprErrorKind) -> bool {
    kinds(src).contains(&kind)
}

/// Модуль с одним текстом запроса — как его пишут в реальном коде.
fn module_with(query: &str) -> String {
    format!("Процедура Выполнить()\n\tЗапрос = Новый Запрос;\n\tЗапрос.Текст = \"{query}\";\nКонецПроцедуры\n")
}

// ── Временная таблица без индекса ─────────────────────────────────────────

#[test]
fn temp_table_joined_without_index_is_reported() {
    let src = module_with(
        "ВЫБРАТЬ Т.Ссылка КАК Ссылка ПОМЕСТИТЬ ВТТовары ИЗ Справочник.Товары КАК Т \
         ;ВЫБРАТЬ 1 ИЗ Справочник.Цены КАК Ц \
         ЛЕВОЕ СОЕДИНЕНИЕ ВТТовары КАК В ПО В.Ссылка = Ц.Товар",
    );
    assert!(has(&src, ExprErrorKind::TempTableWithoutIndex), "{:?}", kinds(&src));
}

#[test]
fn indexed_temp_table_is_silent() {
    let src = module_with(
        "ВЫБРАТЬ Т.Ссылка КАК Ссылка ПОМЕСТИТЬ ВТТовары ИЗ Справочник.Товары КАК Т \
         ИНДЕКСИРОВАТЬ ПО Ссылка \
         ;ВЫБРАТЬ 1 ИЗ Справочник.Цены КАК Ц \
         ЛЕВОЕ СОЕДИНЕНИЕ ВТТовары КАК В ПО В.Ссылка = Ц.Товар",
    );
    assert!(!has(&src, ExprErrorKind::TempTableWithoutIndex), "{:?}", kinds(&src));
}

#[test]
fn temp_table_without_join_needs_no_index() {
    // Таблицу читают целиком — индекс ей ни к чему, находки быть не должно.
    let src = module_with(
        "ВЫБРАТЬ Т.Ссылка КАК Ссылка ПОМЕСТИТЬ ВТТовары ИЗ Справочник.Товары КАК Т \
         ;ВЫБРАТЬ В.Ссылка ИЗ ВТТовары КАК В",
    );
    assert!(!has(&src, ExprErrorKind::TempTableWithoutIndex), "{:?}", kinds(&src));
}

#[test]
fn temp_table_created_in_another_query_text_is_silent() {
    // Создание и соединение разнесены по разным текстам: связи не видно,
    // утверждать нечего.
    let src = module_with("ВЫБРАТЬ Т.Ссылка ПОМЕСТИТЬ ВТТовары ИЗ Справочник.Товары КАК Т");
    assert!(!has(&src, ExprErrorKind::TempTableWithoutIndex), "{:?}", kinds(&src));
}

#[test]
fn index_by_sets_counts_as_index() {
    let src = module_with(
        "ВЫБРАТЬ Т.А КАК А, Т.Б КАК Б ПОМЕСТИТЬ ВТ ИЗ Справочник.Товары КАК Т \
         ИНДЕКСИРОВАТЬ ПО НАБОРАМ ((А, Б), (Б)) \
         ;ВЫБРАТЬ 1 ИЗ Справочник.Цены КАК Ц ЛЕВОЕ СОЕДИНЕНИЕ ВТ КАК В ПО В.А = Ц.Товар",
    );
    assert!(!has(&src, ExprErrorKind::TempTableWithoutIndex), "{:?}", kinds(&src));
}

// ── ИЛИ в условии соединения ──────────────────────────────────────────────

#[test]
fn or_in_join_condition_is_reported() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК С ПО Т.Склад = С.Ссылка ИЛИ Т.Склад ЕСТЬ NULL",
    );
    assert!(has(&src, ExprErrorKind::OrInJoinCondition), "{:?}", kinds(&src));
}

#[test]
fn or_inside_parentheses_is_not_reported() {
    // Связь идёт по равенствам, а `ИЛИ` в скобках — дополнительный отбор.
    // Так написано большинство соединений в типовых; индексу это не мешает.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ВНУТРЕННЕЕ СОЕДИНЕНИЕ Справочник.Склады КАК С \
         ПО Т.Склад = С.Ссылка И (Т.Цена > 0 ИЛИ Т.Остаток > 0)",
    );
    assert!(!has(&src, ExprErrorKind::OrInJoinCondition), "{:?}", kinds(&src));
}

#[test]
fn or_in_where_is_not_a_join_finding() {
    // `ИЛИ` в отборе — обычное дело, правило про условие соединения.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т ГДЕ Т.Цена > 0 ИЛИ Т.Цена ЕСТЬ NULL",
    );
    assert!(!has(&src, ExprErrorKind::OrInJoinCondition), "{:?}", kinds(&src));
}

// ── Соединение с подзапросом ──────────────────────────────────────────────

#[test]
fn join_with_subquery_is_reported() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ (ВЫБРАТЬ Ц.Товар КАК Товар ИЗ Справочник.Цены КАК Ц) КАК П \
         ПО П.Товар = Т.Ссылка",
    );
    assert!(has(&src, ExprErrorKind::JoinWithSubquery), "{:?}", kinds(&src));
}

#[test]
fn subquery_in_where_is_not_a_join() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ГДЕ Т.Ссылка В (ВЫБРАТЬ Ц.Товар ИЗ Справочник.Цены КАК Ц)",
    );
    assert!(!has(&src, ExprErrorKind::JoinWithSubquery), "{:?}", kinds(&src));
}

// ── Молчание там, где сказать нечего ──────────────────────────────────────

#[test]
fn module_without_queries_is_silent() {
    let src = "Процедура П()\n\tСообщить(\"ВЫБРАТЬ не запрос\");\nКонецПроцедуры\n";
    let found = kinds(src);
    assert!(
        !found.contains(&ExprErrorKind::OrInJoinCondition)
            && !found.contains(&ExprErrorKind::JoinWithSubquery)
            && !found.contains(&ExprErrorKind::TempTableWithoutIndex),
        "{found:?}"
    );
}

#[test]
fn unparsed_query_gives_no_findings() {
    // Текст с конструкцией, которой подмножество не знает: правила молчат,
    // а не жалуются на собственное непонимание.
    let src = module_with("ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т {ГДЕ (ЛОЖЬ) КАК Признак}");
    let found = kinds(&src);
    assert!(
        !found.contains(&ExprErrorKind::OrInJoinCondition)
            && !found.contains(&ExprErrorKind::JoinWithSubquery),
        "{found:?}"
    );
}

#[test]
fn dynamic_query_text_is_skipped() {
    // Часть текста вычисляется — запрос не разбирается вовсе.
    let src = "Процедура П()\n\tЗапрос.Текст = \"ВЫБРАТЬ 1 ИЗ \" + ИмяТаблицы + \" КАК Т\";\nКонецПроцедуры\n";
    let found = kinds(src);
    assert!(!found.contains(&ExprErrorKind::TempTableWithoutIndex), "{found:?}");
}

// ── Правила, которым нужен состав объекта ─────────────────────────────────

#[test]
fn physical_balance_register_table_is_reported() {
    let src = module_with("ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах КАК Р");
    assert!(
        has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn movement_fields_make_physical_table_legitimate() {
    // Регистратора в таблице остатков нет — читать движения тут правильно.
    let src = module_with(
        "ВЫБРАТЬ РАЗЛИЧНЫЕ Движения.Регистратор КАК Регистратор \
         ИЗ РегистрНакопления.ТоварыНаСкладах КАК Движения",
    );
    assert!(
        !has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn virtual_table_filter_passed_as_parameter_is_silent() {
    // `Остатки(, &ОтборПоИзмерениям)` — отбор подставляется целиком; полей в
    // тексте нет, и объявлять отбор отсутствующим нельзя.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.Остатки(, &ОтборПоИзмерениям) КАК Т",
    );
    assert!(
        !has_with(&src, &StubSource::new(), ExprErrorKind::VirtualTableWithoutFilter),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn movement_fields_without_alias_also_count() {
    // Типовые пишут условие без алиаса, когда источник один.
    let src = module_with(
        "ВЫБРАТЬ Т.Номенклатура ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т \
         ГДЕ Активность И Регистратор В (&Регистратор)",
    );
    assert!(
        !has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn register_attribute_makes_physical_table_legitimate() {
    // Реквизиты регистра доступны только в движениях — виртуальная таблица их
    // не отдаёт, и советовать её нельзя.
    let src = module_with(
        "ВЫБРАТЬ Т.Номенклатура, Т.Сторно ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т",
    );
    assert!(
        !has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn physical_read_by_dimensions_only_is_still_reported() {
    // Читаются только измерения и ресурс — это ровно то, что отдаёт таблица
    // остатков, значит физическое чтение здесь напрасно.
    let src = module_with(
        "ВЫБРАТЬ Т.Номенклатура, Т.ВНаличии ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т \
         ГДЕ Т.Склад = &Склад",
    );
    assert!(
        has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn periodicity_argument_is_not_a_filter() {
    // `ОстаткиИОбороты(Начало, Конец, Периодичность, МетодДополнения, Условие)`:
    // МЕСЯЦ и Движения — периодичность и метод дополнения, а условие пусто.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.ОстаткиИОбороты(&Нач, , МЕСЯЦ, Движения, ) КАК Т",
    );
    assert!(
        has_with(&src, &StubSource::new(), ExprErrorKind::VirtualTableWithoutFilter),
        "отбор пуст, находка обязана быть: {:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn balance_and_turnovers_with_filter_is_silent() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.ОстаткиИОбороты(&Нач, &Кон, МЕСЯЦ, Движения, Склад = &Склад) КАК Т",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::VirtualTableWithoutFilter));
}

#[test]
fn turnovers_register_has_no_balance_table() {
    // У регистра оборотов таблицы остатков не существует — советовать нечего.
    let src = module_with("ВЫБРАТЬ 1 ИЗ РегистрНакопления.Продажи КАК Р");
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable));
}

#[test]
fn virtual_table_is_not_a_physical_read() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.Остатки(&Дата, Склад = &Склад) КАК Ост",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::PhysicalRegisterTable));
}

#[test]
fn silent_source_means_silence_not_findings() {
    // Главный контракт слоя: «состава не знаю» → правило молчит целиком.
    let src = module_with("ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах КАК Р");
    assert!(!has_with(&src, &StubSource::silent(), ExprErrorKind::PhysicalRegisterTable));
}

#[test]
fn virtual_table_without_filter_is_reported() {
    let src = module_with("ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.Остатки() КАК Ост");
    assert!(
        has_with(&src, &StubSource::new(), ExprErrorKind::VirtualTableWithoutFilter),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn virtual_table_with_dimension_filter_is_silent() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.Остатки(&Дата, Склад = &Склад) КАК Ост",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::VirtualTableWithoutFilter));
}

#[test]
fn join_on_unindexed_field_is_reported() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Товары КАК Д ПО Т.Комментарий = Д.Комментарий",
    );
    assert!(
        has_with(&src, &StubSource::new(), ExprErrorKind::JoinOnUnindexedField),
        "{:?}",
        kinds_with(&src, &StubSource::new())
    );
}

#[test]
fn unindexed_field_on_the_other_side_is_fine() {
    // Поиск идёт по индексу ПРИСОЕДИНЯЕМОЙ таблицы: `Д.Ссылка` — кластерный
    // индекс, и то, что слева стоит неиндексированное поле, роли не играет.
    // На этом случае первая редакция правила дала 16360 находок на УТ.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Товары КАК Д ПО Т.Комментарий = Д.Ссылка",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::JoinOnUnindexedField));
}

#[test]
fn join_with_register_is_left_to_the_register_rule() {
    // У виртуальной таблицы измерения лежат в кластерном индексе итогов,
    // про физическую говорит правило о физической таблице.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ РегистрНакопления.ТоварыНаСкладах.Остатки(&Дата, Склад = &Склад) КАК О \
         ПО О.Номенклатура = Т.Сделка",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::JoinOnUnindexedField));
}

#[test]
fn join_on_indexed_field_is_silent() {
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Товары КАК Д ПО Т.Сделка = Д.Сделка",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::JoinOnUnindexedField));
}

#[test]
fn join_on_standard_field_is_silent() {
    // Ссылка входит в кластерный индекс — платформа индексирует её сама.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Товары КАК Д ПО Т.Ссылка = Д.Ссылка",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::JoinOnUnindexedField));
}

#[test]
fn join_on_unknown_field_is_silent() {
    // Поля нет в составе — возможно, это реквизит табличной части или
    // вычисляемое поле подзапроса. Гадать нельзя.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Товары КАК Д ПО Т.НеизвестноеПоле = Д.НеизвестноеПоле",
    );
    assert!(!has_with(&src, &StubSource::new(), ExprErrorKind::JoinOnUnindexedField));
}

#[test]
fn rules_without_metadata_still_work_without_source() {
    // Правила 1-3 не зависят от состава: без источника они обязаны работать.
    let src = module_with(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК С ПО Т.Склад = С.Ссылка ИЛИ Т.Склад ЕСТЬ NULL",
    );
    assert!(has(&src, ExprErrorKind::OrInJoinCondition));
}

// ── Координаты ────────────────────────────────────────────────────────────

#[test]
fn finding_points_at_the_line_inside_module() {
    let src = "Процедура П()\n\tЗапрос.Текст = \"ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т\n\t|ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК С\n\t|ПО Т.Склад = С.Ссылка ИЛИ Т.Склад ЕСТЬ NULL\";\nКонецПроцедуры\n";
    let index = PlatformIndex::new();
    let result = validate_module_with_profile(&index, src, None, None, 1, Profile::Full);
    let finding = result
        .errors
        .iter()
        .find(|e| e.kind == ExprErrorKind::OrInJoinCondition)
        .expect("находка потеряна");
    // Соединение записано на третьей строке модуля.
    assert_eq!(finding.line, 3, "находка встала не на ту строку: {finding:?}");
}
