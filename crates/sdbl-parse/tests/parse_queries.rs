//! Разбор запросов, какими их пишут в конфигурации.
//!
//! Проверяется ровно то, на что опираются правила: вид источника, соединения,
//! временные таблицы, индексирование, наличие `ИЛИ` в условии. Всё остальное
//! парсер имеет право проглотить — но не имеет права уронить или разобрать
//! наполовину.

use sdbl_parse::{parse, JoinKind, Table};

/// Единственный запрос пакета — иначе тест бессмыслен.
fn single(src: &str) -> sdbl_parse::Query {
    let package = parse(src).unwrap_or_else(|e| panic!("не разобрано ({}): {src}", e.message));
    assert_eq!(package.queries.len(), 1, "ожидался один запрос: {src}");
    package.queries.into_iter().next().unwrap()
}

#[test]
fn plain_select_from_catalog() {
    let query = single("ВЫБРАТЬ Т.Ссылка ИЗ Справочник.Товары КАК Т");
    assert_eq!(query.sources.len(), 1);
    let Table::Meta(meta) = &query.sources[0].table else {
        panic!("источник не опознан как метаданные: {:?}", query.sources[0]);
    };
    assert_eq!(meta.kind, "Справочник");
    assert_eq!(meta.name, "Товары");
    assert_eq!(meta.sub_table, None);
    assert_eq!(query.sources[0].alias.as_ref().unwrap().name, "Т");
}

#[test]
fn english_keywords_are_understood() {
    let query = single("SELECT T.Ref FROM Catalog.Товары AS T");
    assert_eq!(query.sources.len(), 1);
    assert!(matches!(query.sources[0].table, Table::Meta(_)));
}

#[test]
fn temp_table_is_placed_and_indexed() {
    let query = single(
        "ВЫБРАТЬ Т.Ссылка КАК Ссылка ПОМЕСТИТЬ ВТТовары ИЗ Справочник.Товары КАК Т ИНДЕКСИРОВАТЬ ПО Ссылка",
    );
    assert_eq!(query.into.as_ref().unwrap().name, "ВТТовары");
    assert_eq!(query.index_fields.len(), 1);
    assert_eq!(query.index_fields[0].name, "Ссылка");
}

#[test]
fn temp_table_without_index_is_visible() {
    let query = single("ВЫБРАТЬ Т.Ссылка ПОМЕСТИТЬ ВТ ИЗ Справочник.Товары КАК Т");
    assert!(query.into.is_some());
    assert!(query.index_fields.is_empty());
}

#[test]
fn package_keeps_all_queries() {
    let package = parse(
        "ВЫБРАТЬ 1 КАК Поле ПОМЕСТИТЬ ВТ1;\nВЫБРАТЬ Т.Поле ИЗ ВТ1 КАК Т;\nУНИЧТОЖИТЬ ВТ1",
    )
    .expect("пакет не разобран");
    assert_eq!(package.queries.len(), 3);
    assert_eq!(package.queries[0].into.as_ref().unwrap().name, "ВТ1");
    assert!(matches!(package.queries[1].sources[0].table, Table::Temp(_)));
    assert_eq!(package.queries[2].drop_table.as_ref().unwrap().name, "ВТ1");
}

#[test]
fn joins_are_collected_with_kind_and_condition() {
    let query = single(
        "ВЫБРАТЬ Т.Ссылка ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК С ПО Т.Склад = С.Ссылка",
    );
    assert_eq!(query.joins.len(), 1);
    assert_eq!(query.joins[0].kind, JoinKind::Left);
    let on = query.joins[0].on.as_ref().expect("условие соединения потеряно");
    assert!(!on.has_or);
    let paths: Vec<Vec<String>> = on.fields.iter().map(|f| f.path.clone()).collect();
    assert!(paths.contains(&vec!["Т".to_string(), "Склад".to_string()]), "{paths:?}");
    assert!(paths.contains(&vec!["С".to_string(), "Ссылка".to_string()]), "{paths:?}");
}

#[test]
fn or_in_join_condition_is_flagged() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ВНУТРЕННЕЕ СОЕДИНЕНИЕ Справочник.Склады КАК С \
         ПО Т.Склад = С.Ссылка ИЛИ Т.Склад ЕСТЬ NULL",
    );
    assert!(query.joins[0].on.as_ref().unwrap().has_or);
}

#[test]
fn subquery_in_join_is_recognized() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ (ВЫБРАТЬ П.Товар КАК Товар ИЗ Справочник.Цены КАК П) КАК Ц \
         ПО Ц.Товар = Т.Ссылка",
    );
    assert!(
        matches!(query.joins[0].source.table, Table::Subquery(_)),
        "подзапрос в соединении не распознан: {:?}",
        query.joins[0].source.table
    );
    assert_eq!(query.joins[0].source.alias.as_ref().unwrap().name, "Ц");
}

#[test]
fn virtual_table_params_are_split() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах.Остатки(&Дата, Склад = &Склад) КАК Ост",
    );
    let Table::Meta(meta) = &query.sources[0].table else {
        panic!("не метаданные");
    };
    assert_eq!(meta.sub_table.as_deref(), Some("Остатки"));
    assert!(sdbl_parse::is_virtual_table(meta.sub_table.as_ref().unwrap()));
    assert!(sdbl_parse::is_register(&meta.kind));
    assert!(meta.has_parens);
    assert_eq!(meta.params.len(), 2, "параметры: {:?}", meta.params);
    // Во втором параметре должно быть видно измерение, по которому идёт отбор.
    assert!(meta.params[1].fields.iter().any(|f| f.name() == "Склад"));
}

#[test]
fn virtual_table_without_params_keeps_parens_flag() {
    let query = single("ВЫБРАТЬ 1 ИЗ РегистрНакопления.Х.Остатки() КАК Ост");
    let Table::Meta(meta) = &query.sources[0].table else {
        panic!("не метаданные");
    };
    assert!(meta.has_parens);
    assert!(meta.params.is_empty());
}

#[test]
fn physical_register_table_has_no_sub_table() {
    let query = single("ВЫБРАТЬ 1 ИЗ РегистрНакопления.ТоварыНаСкладах КАК Р");
    let Table::Meta(meta) = &query.sources[0].table else {
        panic!("не метаданные");
    };
    assert_eq!(meta.sub_table, None);
    assert!(!meta.has_parens);
}

#[test]
fn document_tabular_section_is_not_a_virtual_table() {
    let query = single("ВЫБРАТЬ 1 ИЗ Документ.ЗаказКлиента.Товары КАК Т");
    let Table::Meta(meta) = &query.sources[0].table else {
        panic!("не метаданные");
    };
    assert_eq!(meta.sub_table.as_deref(), Some("Товары"));
    assert!(!sdbl_parse::is_virtual_table("Товары"));
    assert!(!sdbl_parse::is_register(&meta.kind));
}

#[test]
fn where_fields_are_collected() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т ГДЕ Т.Наименование = &Имя И Т.ПометкаУдаления = ЛОЖЬ",
    );
    let filter = query.filter.expect("секция ГДЕ потеряна");
    assert!(!filter.has_or);
    let names: Vec<&str> = filter.fields.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"Наименование"), "{names:?}");
    assert!(names.contains(&"ПометкаУдаления"), "{names:?}");
}

#[test]
fn group_by_does_not_swallow_join_condition() {
    // `ПО` составного `СГРУППИРОВАТЬ ПО` не должно путаться с `ПО` соединения.
    let query = single(
        "ВЫБРАТЬ Т.Склад, СУММА(Т.Количество) КАК Кол ИЗ Справочник.Товары КАК Т \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК С ПО Т.Склад = С.Ссылка \
         СГРУППИРОВАТЬ ПО Т.Склад",
    );
    assert_eq!(query.joins.len(), 1);
    assert!(query.joins[0].on.is_some());
}

#[test]
fn case_expression_is_swallowed() {
    let query = single(
        "ВЫБРАТЬ ВЫБОР КОГДА Т.Цена > 0 ТОГДА Т.Цена ИНАЧЕ 0 КОНЕЦ КАК Цена \
         ИЗ Справочник.Товары КАК Т",
    );
    assert_eq!(query.sources.len(), 1);
}

#[test]
fn nested_subquery_in_where_does_not_break_sources() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т \
         ГДЕ Т.Ссылка В (ВЫБРАТЬ Ц.Товар ИЗ Справочник.Цены КАК Ц)",
    );
    assert_eq!(query.sources.len(), 1);
    assert!(query.filter.is_some());
}

#[test]
fn several_sources_through_comma() {
    let query = single("ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК Т, Справочник.Склады КАК С");
    assert_eq!(query.sources.len(), 2);
}

#[test]
fn garbage_is_rejected_rather_than_half_parsed() {
    // Не запрос вовсе: правила обязаны промолчать, а не получить пустое дерево.
    assert!(parse("это не запрос").is_err());
    assert!(parse("").is_err());
}

// ── Случаи, вскрытые прогоном по корпусу УТ ───────────────────────────────

#[test]
fn table_name_passed_as_parameter() {
    // Типовые передают имя таблицы параметром — запрос от этого разбираться
    // не перестаёт, хотя про сам источник сказать нечего.
    let query = single("ВЫБРАТЬ 1 ИЗ &ИмяТаблицыИзменений КАК Таблица");
    assert!(
        matches!(query.sources[0].table, Table::Parameter(_)),
        "источник-параметр не распознан: {:?}",
        query.sources[0].table
    );
    assert_eq!(query.sources[0].alias.as_ref().unwrap().name, "Таблица");
}

#[test]
fn union_inside_subquery_is_parsed() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ (ВЫБРАТЬ А.Поле КАК Поле ИЗ Справочник.Товары КАК А \
         ОБЪЕДИНИТЬ ВСЕ \
         ВЫБРАТЬ Б.Поле ИЗ Справочник.Склады КАК Б) КАК Т",
    );
    let Table::Subquery(package) = &query.sources[0].table else {
        panic!("подзапрос не распознан: {:?}", query.sources[0].table);
    };
    assert_eq!(package.queries.len(), 2, "объединение внутри скобок потеряно");
}

#[test]
fn index_by_accepts_qualified_field() {
    let query = single(
        "ВЫБРАТЬ Т.Поле ПОМЕСТИТЬ ВТ ИЗ Справочник.Товары КАК Т \
         ИНДЕКСИРОВАТЬ ПО Т.Поле, Т.Другое",
    );
    assert_eq!(query.index_fields.len(), 2);
    assert_eq!(query.index_fields[0].name, "Т.Поле");
}

#[test]
fn string_template_placeholder_is_rejected() {
    // `ИЗ(%1)` — заготовка под СтрШаблон, а не текст запроса.
    assert!(parse("ВЫБРАТЬ 1 ИЗ(%1) КАК ВложенныйЗапрос").is_err());
}

#[test]
fn template_placeholder_in_object_name_is_tolerated() {
    // `Документ.%1` — имя подставляется в рантайме. Запрос всё равно должен
    // разобраться: правила про соединения и временные таблицы от этого не
    // зависят, а правила про метаданные сами обязаны такое имя пропустить.
    let query = single("ВЫБРАТЬ 1 ИЗ Документ.%1 КАК Т");
    let Table::Meta(meta) = &query.sources[0].table else {
        panic!("источник не разобран: {:?}", query.sources[0].table);
    };
    assert_eq!(meta.name, "%1");
}

#[test]
fn index_by_sets_is_understood() {
    let query = single(
        "ВЫБРАТЬ Т.А, Т.Б ПОМЕСТИТЬ ВТ ИЗ Справочник.Товары КАК Т \
         ИНДЕКСИРОВАТЬ ПО НАБОРАМ ((Организация, Документ), (Документ))",
    );
    assert!(
        !query.index_fields.is_empty(),
        "составной индекс потерян — правило решит, что временная таблица не индексирована"
    );
}

#[test]
fn temp_table_created_then_joined_in_same_package() {
    // Связка, на которой держится правило про неиндексированную временную
    // таблицу: `ПОМЕСТИТЬ` в первом запросе, соединение с ней — во втором.
    let package = parse(
        "ВЫБРАТЬ Т.Ссылка КАК Ссылка ПОМЕСТИТЬ ВТТовары ИЗ Справочник.Товары КАК Т \
         ;ВЫБРАТЬ 1 ИЗ Справочник.Цены КАК Ц \
         ЛЕВОЕ СОЕДИНЕНИЕ ВТТовары КАК В ПО В.Ссылка = Ц.Товар",
    )
    .expect("пакет не разобран");

    assert_eq!(package.queries.len(), 2, "запросы пакета: {:#?}", package.queries);
    assert_eq!(package.queries[0].into.as_ref().map(|n| n.name.as_str()), Some("ВТТовары"));
    assert_eq!(package.queries[1].joins.len(), 1, "соединение потеряно");
    let Table::Temp(named) = &package.queries[1].joins[0].source.table else {
        panic!("соединение не с временной таблицей: {:?}", package.queries[1].joins[0].source.table);
    };
    assert_eq!(named.name, "ВТТовары");
}

#[test]
fn union_keeps_both_selects() {
    let package = parse(
        "ВЫБРАТЬ 1 КАК Поле ИЗ Справочник.Товары КАК Т \
         ОБЪЕДИНИТЬ ВСЕ \
         ВЫБРАТЬ 2 ИЗ Справочник.Склады КАК С",
    )
    .expect("объединение не разобрано");
    assert_eq!(package.queries.len(), 2);
}
