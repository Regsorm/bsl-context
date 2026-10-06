//! Пункт PR «div-маркеры 8.3.17»: в 8.3.17 маркер главы — `<div
//! class="V8SH_chapter">`, в остальных версиях — `<p class="V8SH_chapter">`.
//!
//! Платформы 8.3.17 под рукой нет (в эталонах 8.3.27 этот случай не
//! встречается), поэтому разметка собирается ровно так, как её описывает код,
//! и проверяется главное: **одна и та же страница в div-разметке разбирается
//! идентично** современной p-разметке.

use hbk_parser::{parse_method_page, parse_object_page};

const METHOD_CHAPTERS: &[(&str, &str)] = &[
    ("Синтаксис:", "<p>ТестовыйМетод(&lt;Параметр&gt;)</p>"),
    (
        "Параметры:",
        "<table><tr><td>Параметр</td><td>Число</td><td>Описание параметра.</td></tr></table>",
    ),
    ("Возвращаемое значение:", "<p>Число</p>"),
    ("Описание:", "<p>Тестовое описание метода.</p>"),
    ("Пример:", "<pre>Результат = ТестовыйМетод(1);</pre>"),
    (
        "См. также:",
        "<p><a href=\"v8help://ТестовыйМетод2\">ТестовыйМетод2</a></p>",
    ),
    ("Примечание:", "<p>Примечание к методу.</p>"),
];

const OBJECT_CHAPTERS: &[(&str, &str)] = &[
    ("Описание:", "<p>Описание тестового объекта.</p>"),
    ("Пример:", "<pre>Объект = Новый ТестовыйОбъект;</pre>"),
    (
        "См. также:",
        "<p><a href=\"v8help://ДругойОбъект\">ДругойОбъект</a></p>",
    ),
    ("Примечание:", "<p>Примечание к объекту.</p>"),
    // Главы, которые парсер обязан молча пропускать:
    ("Свойства:", "<p>Свойство1, Свойство2</p>"),
    ("Элементы коллекции:", "<p>Элемент1</p>"),
];

/// Страница с заголовком и главами; `tag` — `p` (современные версии) или
/// `div` (8.3.17).
fn page(tag: &str, title: &str, chapters: &[(&str, &str)]) -> String {
    let mut out = String::from("<html><body>\n");
    out.push_str(&format!("<{tag} class=\"V8SH_title\">{title}</{tag}>\n"));
    for (name, body) in chapters {
        out.push_str(&format!(
            "<{tag} class=\"V8SH_chapter\">{name}</{tag}>\n{body}\n"
        ));
    }
    out.push_str("</body></html>");
    out
}

#[test]
fn method_page_with_div_markers_matches_p_markers() {
    let modern = parse_method_page(&page("p", "ТестовыйМетод", METHOD_CHAPTERS));
    let old = parse_method_page(&page("div", "ТестовыйМетод", METHOD_CHAPTERS));

    assert!(
        format!("{old:?}").contains("ТестовыйМетод"),
        "страница 8.3.17 не разобрана: {old:?}"
    );
    assert_eq!(
        format!("{old:?}"),
        format!("{modern:?}"),
        "div-разметка 8.3.17 разобрана иначе, чем p-разметка"
    );
}

#[test]
fn object_page_with_div_markers_matches_p_markers() {
    let modern = parse_object_page(&page("p", "ТестовыйОбъект", OBJECT_CHAPTERS));
    let old = parse_object_page(&page("div", "ТестовыйОбъект", OBJECT_CHAPTERS));

    assert!(
        format!("{old:?}").contains("ТестовыйОбъект"),
        "страница 8.3.17 не разобрана: {old:?}"
    );
    assert_eq!(
        format!("{old:?}"),
        format!("{modern:?}"),
        "div-разметка 8.3.17 разобрана иначе, чем p-разметка"
    );
}
