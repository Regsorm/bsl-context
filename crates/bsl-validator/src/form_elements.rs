//! Проверка имён элементов формы: `Элементы.<Имя>` (вторая половина issue #43).
//!
//! В модуле управляемой формы обращение к элементу идёт через `Элементы`:
//! `Элементы.Товары.Видимость = Ложь`, `Элементы.НетТакогоЭлемента123.Прочитать()`.
//! Имя элемента знает только сама форма — оно лежит в `Form.xml` выгрузки рядом с
//! модулем. Пока набор имён не передан, проверка молчит целиком: без него
//! выдуманное имя неотличимо от законного.
//!
//! Три случая молчания, кроме отсутствия набора:
//!
//! * **элемент создан в рантайме** — модуль зовёт `Элементы.Добавить(...)`, и
//!   дальше имя живёт только в коде: состав из XML про такие имена не знает;
//! * **голова связана локально** (`Элементы = …`) — это переменная, а не
//!   коллекция элементов формы;
//! * **обычная форма** (`ЭлементыФормы.<Имя>`): её состав лежит в двоичном
//!   `Form.bin`, а не в XML — данных нет, значит и находок нет.

use std::collections::HashSet;

use bsl_parse::AstFacts;

use crate::expression::{fuzzy_confidence_for, lev, pos_at, Confidence, ExprError, ExprErrorKind};

/// Голова коллекции элементов УПРАВЛЯЕМОЙ формы.
const ELEMENTS_HEAD: &str = "Элементы";

/// Голова коллекции элементов обычной формы — данных о ней у нас нет.
const ORDINARY_ELEMENTS_HEAD: &str = "ЭлементыФормы";

/// Метод, которым элемент добавляется в рантайме.
const ADD_METHOD: &str = "Добавить";

pub(crate) fn check_form_elements(
    src: &str,
    facts: &AstFacts,
    elements: Option<&HashSet<String>>,
    errors: &mut Vec<ExprError>,
) {
    let Some(elements) = elements else {
        return;
    };
    if elements.is_empty() {
        return;
    }
    // Рантайм-элементы: после `Элементы.Добавить(...)` состав из XML неполон.
    let dynamic_elements = facts.dots.iter().any(|dot| {
        dot.member_is_call
            && same_name(&dot.head, ELEMENTS_HEAD)
            && same_name(&dot.member, ADD_METHOD)
    });
    if dynamic_elements {
        return;
    }

    let bound = crate::config_objects::locally_bound_names(facts);
    let mut known: Vec<&str> = elements.iter().map(|name| name.as_str()).collect();
    known.sort_unstable();

    for dot in &facts.dots {
        if !same_name(&dot.head, ELEMENTS_HEAD) || same_name(&dot.head, ORDINARY_ELEMENTS_HEAD) {
            continue;
        }
        if bound.contains(&dot.head.to_lowercase()) {
            continue;
        }
        // `Элементы.Добавить(...)` и прочие методы самой коллекции — не имена.
        if dot.member_is_call {
            continue;
        }
        // Мусор восстановления дерева после ошибки разбора — не имя элемента.
        if !is_identifier_like(&dot.member) {
            continue;
        }
        let member_lower = dot.member.to_lowercase();
        if elements
            .iter()
            .any(|name| name.to_lowercase() == member_lower)
        {
            continue;
        }
        let suggestion = closest_element(&known, &dot.member);
        let (line, col) = pos_at(src, dot.member_byte);
        errors.push(ExprError::new_with_confidence(
            line,
            col,
            ExprErrorKind::UnknownFormElement,
            match &suggestion {
                Some(name) => format!(
                    "Элемента '{}' нет в форме. Возможно, вы имели в виду '{}'.",
                    dot.member, name
                ),
                None => format!("Элемента '{}' нет в форме.", dot.member),
            },
            // Состав приходит из файла выгрузки: он может отставать от правок
            // формы, поэтому уверенность низкая.
            Confidence::Low,
            suggestion,
            Vec::new(),
        ));
    }
}

/// Ближайшее имя элемента — только когда это правдоподобная опечатка.
fn closest_element(known: &[&str], member: &str) -> Option<String> {
    let member_lower = member.to_lowercase();
    let mut best: Option<(&str, usize)> = None;
    for name in known {
        let distance = lev(&member_lower, &name.to_lowercase());
        let better = match best {
            Some((best_name, best_distance)) => {
                distance < best_distance || (distance == best_distance && *name < best_name)
            }
            None => true,
        };
        if better {
            best = Some((name, distance));
        }
    }
    let (name, distance) = best?;
    fuzzy_confidence_for(member, name, distance).map(|_| name.to_string())
}

/// Сравнение имён без учёта регистра: `eq_ignore_ascii_case` кириллицу не сворачивает.
fn same_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// Похоже на идентификатор BSL: начинается с буквы или подчёркивания.
fn is_identifier_like(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}
