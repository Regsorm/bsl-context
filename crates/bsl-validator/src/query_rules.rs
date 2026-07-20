//! Правила оптимальности запросов.
//!
//! Проверяется не корректность запроса, а то, как платформа будет его
//! выполнять: соединение с неиндексированной временной таблицей, `ИЛИ` в
//! условии соединения, соединение с подзапросом. Такой код работает и проходит
//! любую проверку синтаксиса — он просто медленный, и на объёмах это заметно.
//!
//! Правила этого файла метаданных конфигурации не требуют: всё, что им нужно,
//! видно в самом тексте запроса.
//!
//! Общий принцип, унаследованный от `config_objects`: **непонятое молчит**.
//! Запрос, который парсер не осилил, не даёт находок вовсе — диагностика о
//! неполноте нашего парсера пользователю не нужна и вредна.

use bsl_parse::QueryText;
use sdbl_parse::{MetaTable, Package, Query, Source, Table};
use std::collections::HashSet;

use crate::expression::{pos_at, ExprError, ExprErrorKind};
use crate::symbols::{ObjectSchema, SymbolSource};

/// Проверить все запросы, записанные в модуле.
///
/// `symbols` нужен только правилам про метаданные; правила про временные
/// таблицы, `ИЛИ` и подзапросы работают и без него.
pub(crate) fn check_query_rules(
    src: &str,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    for query_text in bsl_parse::collect_query_texts(src) {
        let Ok(package) = sdbl_parse::parse(&query_text.text) else {
            continue; // парсер не понял — молчим
        };
        check_package(src, &query_text, &package, symbols, errors);
    }
}

/// Вид объекта в языке запросов → каталог выгрузки, как его ждёт `SymbolSource`.
fn collection_for_kind(kind: &str) -> Option<&'static str> {
    match kind.to_uppercase().as_str() {
        "СПРАВОЧНИК" | "CATALOG" => Some("Catalogs"),
        "ДОКУМЕНТ" | "DOCUMENT" => Some("Documents"),
        "РЕГИСТРСВЕДЕНИЙ" | "INFORMATIONREGISTER" => Some("InformationRegisters"),
        "РЕГИСТРНАКОПЛЕНИЯ" | "ACCUMULATIONREGISTER" => Some("AccumulationRegisters"),
        "РЕГИСТРБУХГАЛТЕРИИ" | "ACCOUNTINGREGISTER" => Some("AccountingRegisters"),
        "РЕГИСТРРАСЧЕТА" | "CALCULATIONREGISTER" => Some("CalculationRegisters"),
        "ПЛАНВИДОВХАРАКТЕРИСТИК" | "CHARTOFCHARACTERISTICTYPES" => {
            Some("ChartsOfCharacteristicTypes")
        }
        "ПЛАНСЧЕТОВ" | "CHARTOFACCOUNTS" => Some("ChartsOfAccounts"),
        "ПЛАНВИДОВРАСЧЕТА" | "CHARTOFCALCULATIONTYPES" => Some("ChartsOfCalculationTypes"),
        "БИЗНЕСПРОЦЕСС" | "BUSINESSPROCESS" => Some("BusinessProcesses"),
        "ЗАДАЧА" | "TASK" => Some("Tasks"),
        "ПЛАНОБМЕНА" | "EXCHANGEPLAN" => Some("ExchangePlans"),
        "ЖУРНАЛДОКУМЕНТОВ" | "DOCUMENTJOURNAL" => Some("DocumentJournals"),
        _ => None,
    }
}

/// Поля, по которым индекс есть всегда — платформа создаёт его сама.
///
/// Состав сверен по схеме СУБД (`~/.claude/reference/1c-standard-indexes.md`):
/// у объектных таблиц это ссылка, код, наименование, владелец, родитель, дата и
/// номер документа; у регистров — период, регистратор, номер строки. Измерений
/// регистра НАКОПЛЕНИЯ здесь нет намеренно: в таблице движений они в
/// кластерный индекс не входят.
fn is_standard_indexed_field(name_lower: &str) -> bool {
    matches!(
        name_lower,
        "ссылка"
            | "ref"
            | "код"
            | "code"
            | "наименование"
            | "description"
            | "владелец"
            | "owner"
            | "родитель"
            | "parent"
            | "дата"
            | "date"
            | "номер"
            | "number"
            | "период"
            | "period"
            | "регистратор"
            | "recorder"
            | "номерстроки"
            | "linenumber"
            | "счетдт"
            | "счеткт"
    )
}

/// Имя-заготовка под подстановку (`%1`, `#ИмяТаблицы`).
fn is_template_name(name: &str) -> bool {
    name.contains('%') || name.contains('#')
}

/// Состав объекта, на который ссылается таблица запроса.
fn schema_of(symbols: Option<&dyn SymbolSource>, meta: &MetaTable) -> Option<ObjectSchema> {
    let symbols = symbols?;
    if is_template_name(&meta.name) {
        return None;
    }
    let collection = collection_for_kind(&meta.kind)?;
    symbols.object_schema(collection, &meta.name.to_lowercase())
}

fn check_package(
    src: &str,
    text: &QueryText,
    package: &Package,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    // Временные таблицы, с которыми где-либо в пакете выполняется соединение.
    // Индекс нужен именно им: таблицу, из которой просто читают, платформа и
    // так прочитает целиком.
    let mut joined_temp_tables: HashSet<String> = HashSet::new();
    collect_joined_temp_tables(package, &mut joined_temp_tables);

    for query in &package.queries {
        check_query(src, text, query, &joined_temp_tables, symbols, errors);
    }
}

/// Обойти пакет вместе со всеми вложенными подзапросами.
fn for_each_query<'a>(package: &'a Package, visit: &mut impl FnMut(&'a Query)) {
    for query in &package.queries {
        visit(query);
        for source in query.all_sources() {
            if let Table::Subquery(inner) = &source.table {
                for_each_query(inner, visit);
            }
        }
    }
}

fn collect_joined_temp_tables(package: &Package, out: &mut HashSet<String>) {
    for_each_query(package, &mut |query| {
        for join in &query.joins {
            if let Table::Temp(named) = &join.source.table {
                out.insert(named.name.to_lowercase());
            }
        }
    });
}

fn check_query(
    src: &str,
    text: &QueryText,
    query: &Query,
    joined_temp_tables: &HashSet<String>,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    check_temp_table_index(src, text, query, joined_temp_tables, errors);
    check_register_tables(src, text, query, symbols, errors);
    check_join_fields(src, text, query, symbols, errors);

    for join in &query.joins {
        // (2) `ИЛИ`, разрывающий условие связи. Вложенный в скобки `ИЛИ` —
        // дополнительный отбор рядом с равенствами, и находкой он не является:
        // замер на УТ показал, что именно так написано большинство соединений
        // в типовых (связь по пяти полям через И плюс фильтр по ресурсам).
        if join.on.as_ref().is_some_and(|on| on.has_top_level_or) {
            emit(
                src,
                text,
                join.offset,
                ExprErrorKind::OrInJoinCondition,
                "В условии соединения (ПО) используется ИЛИ. Оптимизатор не сможет \
                 применить индекс и будет перебирать соединяемые наборы целиком. \
                 Разделите условие на несколько соединений либо объедините выборки \
                 через ОБЪЕДИНИТЬ ВСЕ."
                    .to_string(),
                errors,
            );
        }

        // (3) Соединение с подзапросом.
        if matches!(join.source.table, Table::Subquery(_)) {
            emit(
                src,
                text,
                join.offset,
                ExprErrorKind::JoinWithSubquery,
                "Соединение с подзапросом: подзапрос не индексируется и будет \
                 вычислен заново при соединении. Вынесите его во временную таблицу \
                 (ПОМЕСТИТЬ) с ИНДЕКСИРОВАТЬ ПО полю соединения."
                    .to_string(),
                errors,
            );
        }
    }

    // Подзапросы проверяются на тех же правах, что и запросы верхнего уровня.
    for source in query.all_sources() {
        if let Table::Subquery(inner) = &source.table {
            for nested in &inner.queries {
                check_query(src, text, nested, joined_temp_tables, symbols, errors);
            }
        }
    }
}

/// (4) Физическая таблица регистра остатков и (5) виртуальная таблица без отбора.
fn check_register_tables(
    src: &str,
    text: &QueryText,
    query: &Query,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    for source in query.all_sources() {
        let Table::Meta(meta) = &source.table else {
            continue;
        };
        if !sdbl_parse::is_register(&meta.kind) {
            continue;
        }

        match meta.sub_table.as_deref() {
            // Виртуальная таблица — смотрим, задан ли отбор.
            Some(sub) if sdbl_parse::is_virtual_table(sub) => {
                check_virtual_table_filter(src, text, meta, sub, symbols, errors);
            }
            // Третьего сегмента нет — читается физическая таблица движений.
            None => {
                // Утверждать что-либо можно, только зная вид регистра:
                // у регистра оборотов таблицы остатков не существует.
                let Some(schema) = schema_of(symbols, meta) else {
                    continue;
                };
                if !schema.is_balance_register() {
                    continue;
                }
                // Запросу могут быть нужны сами движения — регистратор, период,
                // номер строки. В таблице остатков этих полей нет, и советовать
                // её бессмысленно. Типовой случай: «ВЫБРАТЬ РАЗЛИЧНЫЕ
                // Движения.Регистратор ИЗ РегистрНакопления.Х КАК Движения».
                if needs_movement_fields(query, source, &schema) {
                    continue;
                }
                emit(
                    src,
                    text,
                    meta.offset,
                    ExprErrorKind::PhysicalRegisterTable,
                    format!(
                        "Чтение физической таблицы движений регистра остатков «{}». \
                         Измерения в её кластерный индекс не входят, поэтому отбор идёт \
                         полным просмотром. Используйте виртуальную таблицу {}.{}.Остатки \
                         (или .ОстаткиИОбороты) — она читает таблицу итогов, где измерения \
                         проиндексированы.",
                        meta.name, meta.kind, meta.name
                    ),
                    errors,
                );
            }
            // Табличная часть или неизвестный сегмент — не наш случай.
            Some(_) => {}
        }
    }
}

/// Нужны ли запросу поля, которые есть только в таблице движений.
///
/// Таких полей два вида, и оба делают чтение физической таблицы законным:
///
/// 1. Служебные — `Регистратор`, `Период`, `НомерСтроки`, `Активность`,
///    `ВидДвижения`: в таблице остатков их нет.
/// 2. **Реквизиты регистра**: виртуальные таблицы отдают только измерения и
///    ресурсы, реквизиты доступны исключительно в движениях. Замер на УТ:
///    типовой `РасчетыСКлиентамиПоСрокам` читается по `ДокументРегистратор` и
///    `СуммаПриемник` — это реквизиты, и совет «возьмите .Остатки» невыполним.
fn needs_movement_fields(query: &Query, source: &Source, schema: &ObjectSchema) -> bool {
    let Some(alias) = &source.alias else {
        // Без алиаса поля не сопоставить с источником — считаем, что движения
        // могут быть нужны, и молчим: ложная находка дороже пропущенной.
        return true;
    };
    let alias_lower = alias.name.to_lowercase();

    let attributes: HashSet<String> = schema
        .attributes
        .iter()
        .map(|a| a.name.to_lowercase())
        .collect();

    let movement_only = |name: &str| {
        matches!(
            name,
            "регистратор"
                | "recorder"
                | "период"
                | "period"
                | "номерстроки"
                | "linenumber"
                | "активность"
                | "active"
                | "виддвижения"
                | "recordtype"
        ) || attributes.contains(name)
    };

    query
        .select
        .iter()
        .chain(query.filter.iter())
        .chain(query.joins.iter().filter_map(|j| j.on.as_ref()))
        .flat_map(|condition| condition.fields.iter())
        .any(|field| {
            // Поле без алиаса (`ГДЕ Активность И Регистратор В (&Р)`) — тоже
            // счёт в пользу движений: так пишут, когда источник один.
            let belongs = match field.qualifier() {
                Some(qualifier) => qualifier.to_lowercase() == alias_lower,
                None => true,
            };
            belongs && movement_only(&field.name().to_lowercase())
        })
}

/// Порядковый номер параметра-условия у виртуальной таблицы.
///
/// Условие всегда последнее в сигнатуре, но позиция зависит от вида таблицы:
/// `Остатки(Период, Условие)`, `Обороты(Начало, Конец, Периодичность, Условие)`,
/// `ОстаткиИОбороты(Начало, Конец, Периодичность, МетодДополнения, Условие)`.
/// Без этого `ОстаткиИОбороты(&Начало, , МЕСЯЦ, Движения, )` выглядит таблицей
/// с отбором — хотя `МЕСЯЦ` и `Движения` это периодичность и метод дополнения.
///
/// `None` — сигнатура таблицы не описана, и правило обязано промолчать.
fn filter_param_index(virtual_table: &str) -> Option<usize> {
    match virtual_table.to_uppercase().as_str() {
        "ОСТАТКИ" | "BALANCE" | "СРЕЗПОСЛЕДНИХ" | "SLICELAST" | "СРЕЗПЕРВЫХ" | "SLICEFIRST" => {
            Some(1)
        }
        "ОБОРОТЫ" | "TURNOVERS" => Some(3),
        "ОСТАТКИИОБОРОТЫ" | "BALANCEANDTURNOVERS" => Some(4),
        _ => None,
    }
}

/// (5) Виртуальная таблица вызвана без отбора по измерениям.
fn check_virtual_table_filter(
    src: &str,
    text: &QueryText,
    meta: &MetaTable,
    sub: &str,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    // Условие — параметр на своём месте в сигнатуре, а не «любой непустой».
    let Some(index) = filter_param_index(sub) else {
        return; // сигнатура неизвестна — молчим
    };
    let filter = meta.params.get(index);

    if let Some(filter) = filter.filter(|p| !p.is_empty) {
        // Отбор целиком подставляется параметром запроса
        // (`Остатки(, &ОтборПоИзмерениям)` — типовой приём): полей в тексте нет,
        // и сказать про них нечего. Молчим, а не объявляем отбор отсутствующим.
        if filter.fields.is_empty() {
            return;
        }
        // Отбор есть — но по измерению ли? Ответить можно, только зная состав.
        let Some(schema) = schema_of(symbols, meta) else {
            return;
        };
        let dimensions: HashSet<String> = schema
            .dimensions
            .iter()
            .map(|d| d.name.to_lowercase())
            .collect();
        let by_dimension = filter
            .fields
            .iter()
            .any(|f| dimensions.contains(&f.name().to_lowercase()));
        if by_dimension {
            return;
        }
    }

    emit(
        src,
        text,
        meta.offset,
        ExprErrorKind::VirtualTableWithoutFilter,
        format!(
            "Виртуальная таблица {}.{}.{} вызвана без отбора по измерениям. \
             Платформа рассчитает итоги по всему регистру, а лишние строки отсеются \
             уже после. Перенесите условие в параметры виртуальной таблицы.",
            meta.kind, meta.name, sub
        ),
        errors,
    );
}

/// (6) Соединение, при котором у ПРИСОЕДИНЯЕМОЙ таблицы нет индекса по полю связи.
///
/// Ключевая тонкость, из-за которой первая редакция правила дала 16360 находок
/// на УТ: соединение идёт поиском по индексу ПРИСОЕДИНЯЕМОЙ таблицы, и индекс
/// нужен только ей. В типовом
/// `… СОЕДИНЕНИЕ Справочник.Номенклатура КАК С ПО А.Номенклатура = С.Ссылка`
/// поле `А.Номенклатура` не индексировано и это совершенно нормально: поиск
/// идёт по `С.Ссылка`, то есть по кластерному индексу.
///
/// Регистры пропускаются: у виртуальных таблиц измерения лежат в кластерном
/// индексе таблицы итогов, а про физические таблицы говорит правило (4).
fn check_join_fields(
    src: &str,
    text: &QueryText,
    query: &Query,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    if symbols.is_none() {
        return;
    }

    for join in &query.joins {
        let Some(on) = &join.on else { continue };
        let Table::Meta(meta) = &join.source.table else {
            continue;
        };
        if sdbl_parse::is_register(&meta.kind) {
            continue;
        }
        let Some(alias) = &join.source.alias else {
            continue; // без алиаса поля не сопоставить с таблицей
        };
        let Some(schema) = schema_of(symbols, meta) else {
            continue;
        };

        let alias_lower = alias.name.to_lowercase();
        // Поля присоединяемой таблицы, по которым идёт связь.
        let mut unindexed: Option<&sdbl_parse::Field> = None;
        let mut indexed_found = false;

        for field in &on.fields {
            // Путь длиннее двух сегментов — обращение через точку к реквизиту
            // ссылки; это уже другая таблица.
            if field.path.len() != 2 {
                continue;
            }
            if field.qualifier().map(|q| q.to_lowercase()).as_deref() != Some(alias_lower.as_str())
            {
                continue;
            }
            let name_lower = field.name().to_lowercase();
            if is_standard_indexed_field(&name_lower) {
                indexed_found = true;
                break;
            }
            match schema.field(&name_lower) {
                Some(object_field) if object_field.is_indexed() => {
                    indexed_found = true;
                    break;
                }
                Some(_) => {
                    if unindexed.is_none() {
                        unindexed = Some(field);
                    }
                }
                // Поля в составе нет — молчим, а не гадаем.
                None => {}
            }
        }

        if indexed_found {
            continue;
        }
        let Some(field) = unindexed else { continue };

        emit(
            src,
            text,
            field.offset,
            ExprErrorKind::JoinOnUnindexedField,
            format!(
                "Соединение с «{}.{}» идёт по полю «{}», у которого нет ни стандартного \
                 индекса, ни свойства «Индексировать». Поиск в присоединяемой таблице \
                 пойдёт полным просмотром. Установите «Индексировать» либо соединяйте \
                 по ссылке.",
                meta.kind,
                meta.name,
                field.name()
            ),
            errors,
        );
    }
}

/// (1) Временная таблица без индекса, участвующая в соединении.
fn check_temp_table_index(
    src: &str,
    text: &QueryText,
    query: &Query,
    joined_temp_tables: &HashSet<String>,
    errors: &mut Vec<ExprError>,
) {
    let Some(into) = &query.into else {
        return;
    };
    if !query.index_fields.is_empty() {
        return;
    }
    // Имя подставляется в рантайме (`ПОМЕСТИТЬ #ИмяТаблицы`) — сверить не с чем.
    if is_template_name(&into.name) {
        return;
    }
    // Соединения с этой таблицей в пакете нет: индекс ей не нужен. Если таблица
    // создаётся в одном тексте запроса, а соединяется в другом, мы этого не
    // видим — и молчим, потому что утверждать нечего.
    if !joined_temp_tables.contains(&into.name.to_lowercase()) {
        return;
    }

    emit(
        src,
        text,
        into.offset,
        ExprErrorKind::TempTableWithoutIndex,
        format!(
            "Временная таблица «{}» участвует в соединении, но не индексирована. \
             Платформа выполнит соединение перебором. Добавьте ИНДЕКСИРОВАТЬ ПО \
             полям, по которым идёт соединение.",
            into.name
        ),
        errors,
    );
}

/// Перевести смещение внутри текста запроса в координаты модуля и добавить находку.
fn emit(
    src: &str,
    text: &QueryText,
    query_offset: usize,
    kind: ExprErrorKind,
    message: String,
    errors: &mut Vec<ExprError>,
) {
    let module_byte = text.map_offset(query_offset);
    let (line, col) = pos_at(src, module_byte);
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        kind,
        message,
        kind.confidence(),
        None,
        Vec::new(),
    ));
}
