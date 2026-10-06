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
    let package =
        parse("ВЫБРАТЬ 1 КАК Поле ПОМЕСТИТЬ ВТ1;\nВЫБРАТЬ Т.Поле ИЗ ВТ1 КАК Т;\nУНИЧТОЖИТЬ ВТ1")
            .expect("пакет не разобран");
    assert_eq!(package.queries.len(), 3);
    assert_eq!(package.queries[0].into.as_ref().unwrap().name, "ВТ1");
    assert!(matches!(
        package.queries[1].sources[0].table,
        Table::Temp(_)
    ));
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
    let on = query.joins[0]
        .on
        .as_ref()
        .expect("условие соединения потеряно");
    assert!(!on.has_or);
    let paths: Vec<Vec<String>> = on.fields.iter().map(|f| f.path.clone()).collect();
    assert!(
        paths.contains(&vec!["Т".to_string(), "Склад".to_string()]),
        "{paths:?}"
    );
    assert!(
        paths.contains(&vec!["С".to_string(), "Ссылка".to_string()]),
        "{paths:?}"
    );
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
    assert!(sdbl_parse::is_virtual_table(
        meta.sub_table.as_ref().unwrap()
    ));
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
    assert_eq!(
        package.queries.len(),
        2,
        "объединение внутри скобок потеряно"
    );
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

    assert_eq!(
        package.queries.len(),
        2,
        "запросы пакета: {:#?}",
        package.queries
    );
    assert_eq!(
        package.queries[0].into.as_ref().map(|n| n.name.as_str()),
        Some("ВТТовары")
    );
    assert_eq!(package.queries[1].joins.len(), 1, "соединение потеряно");
    let Table::Temp(named) = &package.queries[1].joins[0].source.table else {
        panic!(
            "соединение не с временной таблицей: {:?}",
            package.queries[1].joins[0].source.table
        );
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

// ── Регрессии аудита: битый вход даёт Err, а не половину дерева ────────────

#[test]
fn unclosed_string_is_rejected() {
    assert!(parse("ВЫБРАТЬ Т.Ссылка ИЗ Справочник.Товары КАК Т ГДЕ Т.Имя = \"Иванов").is_err());
}

#[test]
fn unbalanced_paren_does_not_swallow_package() {
    // Незакрытая скобка не должна съедать `;` и следующий оператор пакета.
    assert!(parse("ВЫБРАТЬ 1 ИЗ Т1 ГДЕ (А = 1;\nВЫБРАТЬ 2 ИЗ Т2").is_err());
    assert!(parse("ВЫБРАТЬ 1 ИЗ РегистрНакопления.Р.Остатки(П;\nВЫБРАТЬ 2 ИЗ Т2").is_err());
}

#[test]
fn top_without_number_is_rejected() {
    // `ПЕРВЫЕ` без числа не должно принимать `ИЗ` за «число».
    assert!(parse("ВЫБРАТЬ ПЕРВЫЕ ИЗ Т").is_err());
}

#[test]
fn dangling_as_is_rejected() {
    assert!(parse("ВЫБРАТЬ 1 ИЗ Т КАК").is_err());
}

#[test]
fn dangling_union_is_rejected() {
    assert!(parse("ВЫБРАТЬ 1 ИЗ Т ОБЪЕДИНИТЬ").is_err());
}

#[test]
fn empty_select_and_filter_are_rejected() {
    assert!(parse("ВЫБРАТЬ").is_err());
    assert!(parse("ВЫБРАТЬ 1 ИЗ Т ГДЕ").is_err());
}

#[test]
fn four_segment_meta_name_is_rejected() {
    // Тип хранит три сегмента: четвёртый молча терять нельзя.
    assert!(parse("ВЫБРАТЬ 1 ИЗ Справочник.А.Б.В КАК Т").is_err());
}

#[test]
fn select_alias_is_not_a_field() {
    // Алиас после КАК — не поле источника: иначе `КАК Регистратор` глушит
    // правило о физической таблице регистра.
    let query = single("ВЫБРАТЬ Т.Ссылка КАК Регистратор ИЗ Справочник.Товары КАК Т");
    let select = query.select.expect("select потерян");
    assert!(
        !select.fields.iter().any(|f| f.name() == "Регистратор"),
        "алиас попал в поля: {:?}",
        select.fields
    );
    assert!(select.fields.iter().any(|f| f.name() == "Ссылка"));
}

#[test]
fn nested_subquery_fields_do_not_leak_into_condition() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК А \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК Б ПО Б.Х = А.Х И Б.У В (ВЫБРАТЬ Ц.У ИЗ Справочник.Цены КАК Ц)",
    );
    let on = query.joins[0].on.as_ref().expect("условие соединения");
    assert!(
        !on.fields.iter().any(|f| f.qualifier() == Some("Ц")),
        "поля подзапроса протекли: {:?}",
        on.fields
    );
}

#[test]
fn hash_template_names_are_tolerated() {
    let package = parse("ВЫБРАТЬ 1 ПОМЕСТИТЬ #ВТ;\nВЫБРАТЬ 1 ИЗ #ВТ КАК Т")
        .expect("шаблонные имена с # должны разбираться");
    assert_eq!(package.queries[0].into.as_ref().unwrap().name, "#ВТ");
    assert!(matches!(
        package.queries[1].sources[0].table,
        Table::Temp(_)
    ));
}

#[test]
fn alias_like_compound_word_keeps_join_condition() {
    // `КАК Упорядочить ПО …` — алиас и условие соединения, а не `УПОРЯДОЧИТЬ ПО`.
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК А \
         ЛЕВОЕ СОЕДИНЕНИЕ Справочник.Склады КАК Упорядочить ПО А.Х = Упорядочить.Х",
    );
    let join = &query.joins[0];
    assert_eq!(join.source.alias.as_ref().unwrap().name, "Упорядочить");
    assert!(join.on.is_some(), "условие соединения потеряно");
}

#[test]
fn comment_with_carriage_return_does_not_swallow_query() {
    let query = single("ВЫБРАТЬ Поле // c\rИЗ Справочник.Товары КАК Т");
    assert_eq!(query.sources.len(), 1);
}

#[test]
fn group_by_fields_are_collected() {
    let query = single(
        "ВЫБРАТЬ КОЛИЧЕСТВО(*) КАК К ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т \
         СГРУППИРОВАТЬ ПО Т.Регистратор",
    );
    assert!(
        query.extra_fields.iter().any(|f| f.name() == "Регистратор"),
        "{:?}",
        query.extra_fields
    );
}

#[test]
fn star_select_is_flagged() {
    let query = single("ВЫБРАТЬ * ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т");
    assert!(query.select.as_ref().unwrap().has_star);
}

#[test]
fn qualified_star_select_is_flagged() {
    let query = single("ВЫБРАТЬ Т.* ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т");
    assert!(query.select.as_ref().unwrap().has_star);
}

#[test]
fn star_after_top_is_a_select_star() {
    // `ПЕРВЫЕ 10 *` — тот же джокер: раньше его тоже нельзя было терять.
    let query = single("ВЫБРАТЬ ПЕРВЫЕ 10 * ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т");
    assert!(query.select.as_ref().unwrap().has_star);
}

#[test]
fn star_inside_function_is_not_a_select_star() {
    // `КОЛИЧЕСТВО(*)` — не «состав полей неизвестен»: правило о физической
    // таблице регистра обязано работать (аудит PR: звёздочка в скобках глушила
    // его на `ВЫБРАТЬ КОЛИЧЕСТВО(*)`).
    let query = single("ВЫБРАТЬ КОЛИЧЕСТВО(*) КАК К ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т");
    assert!(!query.select.as_ref().unwrap().has_star);
}

#[test]
fn multiplication_is_not_a_select_star() {
    let query = single("ВЫБРАТЬ Т.Количество * 2 КАК К ИЗ РегистрНакопления.ТоварыНаСкладах КАК Т");
    assert!(!query.select.as_ref().unwrap().has_star);
}

#[test]
fn external_data_source_path_is_parsed_as_unknown_source() {
    // Составной путь внешнего источника данных подмножество не разбирает — но
    // ронять из-за него разбор ВСЕГО запроса нельзя: остальные правила по этому
    // тексту обязаны работать (аудит PR).
    let query = single(
        "ВЫБРАТЬ Т.Поле КАК Поле ИЗ ВнешнийИсточникДанных.МойИсточник.Таблица.МояТаблица КАК Т",
    );
    assert_eq!(query.sources.len(), 1);
    assert!(
        matches!(query.sources[0].table, Table::Unknown(_)),
        "ожидался Unknown: {:?}",
        query.sources[0]
    );
    assert_eq!(query.sources[0].alias.as_ref().unwrap().name, "Т");
}

#[test]
fn subquery_in_condition_is_flagged() {
    let query = single(
        "ВЫБРАТЬ 1 ИЗ Справочник.Товары КАК А \
         ГДЕ А.Ссылка В (ВЫБРАТЬ Т.Ссылка ИЗ Справочник.Цены КАК Т)",
    );
    assert!(query.filter.as_ref().unwrap().has_subquery);
}

#[test]
fn union_index_is_attached_to_into_query() {
    let package = parse(
        "ВЫБРАТЬ Т.Ссылка КАК Ссылка ПОМЕСТИТЬ ВТ ИЗ Справочник.Товары КАК Т \
         ОБЪЕДИНИТЬ ВСЕ \
         ВЫБРАТЬ С.Ссылка ИЗ Справочник.Склады КАК С \
         ИНДЕКСИРОВАТЬ ПО Ссылка ;\
         ВЫБРАТЬ 1 ИЗ ВТ КАК В",
    )
    .expect("пакет с объединением и индексом");
    assert_eq!(package.queries.len(), 3);
    assert_eq!(package.queries[0].into.as_ref().unwrap().name, "ВТ");
    assert_eq!(
        package.queries[0].index_fields.len(),
        1,
        "индекс обязан переехать на запрос с ПОМЕСТИТЬ: {:?}",
        package.queries[0].index_fields
    );
    assert!(
        package.queries[1].index_fields.is_empty(),
        "индекс не должен оставаться на последней выборке объединения"
    );
}
