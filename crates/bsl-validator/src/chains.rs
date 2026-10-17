//! Проверки ЦЕПОЧЕК обращений, у которых важен сегмент глубже первого звена.
//!
//! Две ветки, обе появились из issue:
//!
//! * `Метаданные.<Коллекция>.<Объект>[.<Член>]` (issue #43) — до правки ветка
//!   `Метаданные.` не проверялась вовсе: из цепочки извлекалось только первое
//!   звено, и выдуманные коллекция, объект и член описания проходили молча.
//!   Сверяются три уровня: коллекция — со свойствами типа справки
//!   `ОбъектМетаданныхКонфигурация`, объект — с источником имён (как
//!   `Справочники.<Имя>`), член описания — с типом `ОбъектМетаданных: <Вид>`.
//! * `Перечисления.<Имя>.<Значение>` (issue #41) — имя перечисления проверяла
//!   ветка менеджеров, а ЗНАЧЕНИЕ не проверял никто: `Перечисления.ВариантыОкругления.
//!   НетТакогоЗначения123` давало `valid: true`. Значения прикладного перечисления
//!   приходят из источника имён (`enum_values` в `attributes_json`).
//!
//! Общее правило — молчание по умолчанию: нет сегмента, нет типа в справке, нет
//! источника или он не отдал состав — находки нет. Ответ «не найдено» от
//! источника с неправдоподобно пустым составом понижается до `Low` (issue #46).

use platform_index::PlatformIndex;

use bsl_parse::{AstFacts, ChainFact};

use crate::config_objects::{collection_id_for_metadata, metadata_singular_for, suggestion_for};
use crate::expression::{fuzzy_confidence_for, lev, pos_at, Confidence, ExprError, ExprErrorKind};
use crate::symbols::SymbolSource;

/// Тип справки, описывающий сам глобальный `Метаданные`: его свойства — коллекции
/// (`Справочники`, `Документы`, …, `Языки`, `ЭлементыСтиля`).
const METADATA_ROOT_TYPE: &str = "ОбъектМетаданныхКонфигурация";

/// Префикс имени типа-описания объекта метаданных: `ОбъектМетаданных: Справочник`.
const METADATA_DESCRIPTION_PREFIX: &str = "ОбъектМетаданных: ";

/// Коллекция прикладных перечислений в терминах источника имён.
const ENUMS_COLLECTION: &str = "Enums";

/// Голова ветки про метаданные конфигурации.
const METADATA_HEAD: &str = "Метаданные";

/// Голова ветки про прикладные перечисления.
const ENUMS_HEAD: &str = "Перечисления";

pub(crate) fn check_chains(
    index: &PlatformIndex,
    src: &str,
    facts: &AstFacts,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    if facts.chains.is_empty() {
        return;
    }
    // Имя, связанное локально, — переменная, а не глобальный `Метаданные`:
    // в УТ есть процедуры с параметром `Метаданные`
    // (`CommonModules/НоменклатураСервер`), и без этой проверки обращение
    // `Метаданные.Реквизиты` становилось «коллекции 'Реквизиты' нет в метаданных».
    let bound = crate::config_objects::locally_bound_names(facts);
    for chain in &facts.chains {
        if bound.contains(&chain.head.to_lowercase()) {
            continue;
        }
        if same_name(&chain.head, METADATA_HEAD) {
            check_metadata_chain(index, src, chain, symbols, errors);
        } else if same_name(&chain.head, ENUMS_HEAD) {
            check_enum_chain(src, chain, symbols, errors);
        }
    }
}

/// `Метаданные.<Коллекция>.<Объект>[.<Член>]` (issue #43).
fn check_metadata_chain(
    index: &PlatformIndex,
    src: &str,
    chain: &ChainFact,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    let Some((collection, collection_byte)) = chain.segments.first() else {
        return;
    };
    let Some(root) = index.find_type(METADATA_ROOT_TYPE) else {
        return;
    };
    if !root
        .properties
        .iter()
        .any(|property| same_name(&property.name_ru, collection))
    {
        let suggestion = closest_name(
            root.properties.iter().map(|p| p.name_ru.as_str()),
            collection,
        );
        let (line, col) = pos_at(src, *collection_byte);
        errors.push(ExprError::new(
            line,
            col,
            ExprErrorKind::UnknownMetadataObject,
            format!(
                "Коллекции '{}' нет в метаданных конфигурации.{}",
                collection,
                suffix(&suggestion, "коллекцию")
            ),
            suggestion,
            Vec::new(),
        ));
        return;
    }

    // Коллекции вне таблицы менеджеров (`ОбщиеМодули`, `Языки`, …) источник имён
    // не знает — проверять по нему нечего.
    let Some(collection_id) = collection_id_for_metadata(collection) else {
        return;
    };
    let Some((object, object_byte)) = chain.segments.get(1) else {
        return;
    };
    let object_lower = object.to_lowercase();
    if symbols.and_then(|source| source.object_exists(collection_id, &object_lower)) == Some(false)
    {
        let suggestion = symbols.and_then(|source| suggestion_for(source, collection_id, object));
        let (line, col) = pos_at(src, *object_byte);
        errors.push(ExprError::new_with_confidence(
            line,
            col,
            ExprErrorKind::UnknownMetadataObject,
            format!(
                "Объект '{}' не существует в '{}'.{}",
                object,
                collection,
                suffix(&suggestion, "объект")
            ),
            source_confidence(symbols),
            suggestion,
            Vec::new(),
        ));
        return;
    }

    // Член описания: у коллекции `Справочники` — тип `ОбъектМетаданных: Справочник`.
    let Some((member, member_byte)) = chain.segments.get(2) else {
        return;
    };
    let Some(singular) = metadata_singular_for(collection) else {
        return;
    };
    let type_name = format!("{METADATA_DESCRIPTION_PREFIX}{singular}");
    let Some(description) = index.find_type(&type_name) else {
        return;
    };
    let known = description
        .properties
        .iter()
        .any(|property| same_name(&property.name_ru, member))
        || description
            .methods
            .iter()
            .any(|method| same_name(&method.name_ru, member));
    if known {
        return;
    }
    let suggestion = closest_name(
        description
            .properties
            .iter()
            .map(|p| p.name_ru.as_str())
            .chain(description.methods.iter().map(|m| m.name_ru.as_str())),
        member,
    );
    let (line, col) = pos_at(src, *member_byte);
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        ExprErrorKind::UnknownTypeMember,
        format!(
            "У объекта метаданных '{}' нет свойства '{}'.{}",
            object,
            member,
            suffix(&suggestion, "свойство")
        ),
        Confidence::Low,
        suggestion,
        Vec::new(),
    ));
}

/// `Перечисления.<Имя>.<Значение>` (issue #41): значение сверяется с составом
/// прикладного перечисления из источника имён.
///
/// Имя самого перечисления проверяет ветка менеджеров (`Перечисления.<Имя>`),
/// поэтому здесь только значение. Пустой `enum_values` — источник значений не
/// отдал: молчим целиком, иначе выдуманное значение неотличимо от законного.
fn check_enum_chain(
    src: &str,
    chain: &ChainFact,
    symbols: Option<&dyn SymbolSource>,
    errors: &mut Vec<ExprError>,
) {
    let Some(symbols) = symbols else {
        return;
    };
    let (Some((enum_name, _)), Some((value, value_byte))) =
        (chain.segments.first(), chain.segments.get(1))
    else {
        return;
    };
    let Some(schema) = symbols.object_schema(ENUMS_COLLECTION, &enum_name.to_lowercase()) else {
        return;
    };
    if schema.enum_values.is_empty() || schema.has_enum_value(value) {
        return;
    }
    let suggestion = closest_name(schema.enum_values.iter().map(|v| v.as_str()), value);
    let (line, col) = pos_at(src, *value_byte);
    errors.push(ExprError::new_with_confidence(
        line,
        col,
        ExprErrorKind::UnknownEnumValue,
        format!(
            "Значения '{}' нет у перечисления '{}'.{}",
            value,
            enum_name,
            suffix(&suggestion, "значение")
        ),
        source_confidence(Some(symbols)),
        suggestion,
        Vec::new(),
    ));
}

/// Уверенность находки по ответу ИСТОЧНИКА имён: у «тонкого» источника (пустой
/// или почти пустой индекс, issue #46) ответ «не найдено» — не доказательство.
fn source_confidence(symbols: Option<&dyn SymbolSource>) -> Confidence {
    match symbols {
        Some(source) if source.is_thin() => Confidence::Low,
        _ => Confidence::High,
    }
}

/// Подсказка «Возможно, вы имели в виду …» — или пустая строка.
fn suffix(suggestion: &Option<String>, what: &str) -> String {
    match suggestion {
        Some(name) => format!(" Возможно, вы имели в виду {what} '{name}'."),
        None => String::new(),
    }
}

/// Сравнение имён без учёта регистра. `eq_ignore_ascii_case` не годится: регистр
/// кириллицы он не сворачивает.
fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// Ближайшее имя из списка — только когда это правдоподобная опечатка (та же
/// двухпороговая эвристика, что у глобальных имён и подсказок по коллекциям).
fn closest_name<'a>(names: impl Iterator<Item = &'a str>, needle: &str) -> Option<String> {
    let needle_lower = needle.to_lowercase();
    let mut best: Option<(String, usize)> = None;
    for name in names {
        let distance = lev(&needle_lower, &name.to_lowercase());
        let better = match &best {
            // Тай-брейк по имени: обход недетерминированного набора не должен
            // менять подсказку между запусками.
            Some((best_name, best_distance)) => {
                distance < *best_distance || (distance == *best_distance && name < best_name)
            }
            None => true,
        };
        if better {
            best = Some((name.to_string(), distance));
        }
    }
    let (name, distance) = best?;
    fuzzy_confidence_for(needle, &name, distance).map(|_| name)
}
