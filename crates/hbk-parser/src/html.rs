//! Общие хелперы парсинга html-страниц синтакс-помощника:
//! - `split_chapters` — режет страницу на главы по маркерам
//!   `<p|div class="V8SH_chapter">`/`<hr>` (в 8.3.17 маркер — `div`)
//! - `extract_text` — текст узла без html-тегов (блоки разделяются пробелом)
//! - `to_markdown` — html-фрагмент → Markdown (порт `MarkdownHtmlHandler.kt`)

use ego_tree::NodeRef;
use scraper::{Html, Node, Selector};
use std::sync::OnceLock;

/// Селектор `<body>` — статический: `Selector::parse` компилирует CSS-строку
/// заново, а `split_chapters` зовётся на каждую страницу справки (десятки тысяч
/// раз за сборку). Компиляция здесь не зависит от страницы, поэтому считается
/// один раз на процесс.
fn body_selector() -> &'static Selector {
    static SEL: OnceLock<Selector> = OnceLock::new();
    SEL.get_or_init(|| Selector::parse("body").expect("body selector"))
}

/// Глава html-страницы. Между двумя маркерами идёт «тело» главы — series of
/// верхнеуровневых элементов в DOM. Сохраняем их как html-фрагмент, чтобы
/// можно было пере-распарсить отдельным block-парсером.
#[derive(Debug, Clone)]
pub struct Chapter {
    /// Заголовок: текст внутри `<p class="V8SH_chapter">`. У первой главы
    /// (до любого маркера) заголовок — пустая строка; там обычно лежит
    /// `<p class="V8SH_title">` или `<p class="V8SH_heading">` с именем
    /// сущности — это «голова» страницы, обрабатывается NameBlockHandler.
    pub title: String,
    /// Сериализованный html всех узлов внутри главы (без самого маркера).
    pub body_html: String,
}

/// Разбить html-страницу синтакс-помощника на главы.
///
/// Маркеры между главами:
/// - `<p class="V8SH_chapter">Синтаксис:</p>` — заголовок становится `Chapter.title`,
///   следующее содержимое идёт в `body_html`.
/// - `<hr>` — то же, но без заголовка (используется редко, часто пустая глава).
pub fn split_chapters(html: &str) -> Vec<Chapter> {
    let doc = Html::parse_document(html);
    // Парсер scraper всегда оборачивает в <html><head>/<body>; берём body.
    let body = match doc.select(body_selector()).next() {
        Some(b) => b,
        None => return Vec::new(),
    };

    let mut chapters: Vec<Chapter> = vec![Chapter {
        title: String::new(),
        body_html: String::new(),
    }];

    for child in body.children() {
        if is_chapter_marker(child) {
            chapters.push(Chapter {
                title: chapter_title_of(child),
                body_html: String::new(),
            });
            continue;
        }
        // node serialize: scraper не имеет прямого .html() для узла-не-Element,
        // используем ego_tree NodeRef из node().
        if let Some(text) = serialize_node(child) {
            chapters
                .last_mut()
                .expect("at least one chapter")
                .body_html
                .push_str(&text);
        }
    }
    chapters
}

/// Текст узла без html-тегов и нормализованный (collapse whitespace, trim).
///
/// Тексты соседних БЛОЧНЫХ элементов разделяются пробелом — иначе
/// `<p>a</p><p>b</p>` дал бы «ab». Содержимое `script`/`style` пропускается
/// целиком (это не текст страницы).
pub fn extract_text(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let mut buf = String::new();
    let mut stack = vec![(doc.tree.root(), false)];
    while let Some((node, closing)) = stack.pop() {
        match node.value() {
            Node::Text(text) => {
                if !closing {
                    buf.push_str(text);
                }
            }
            Node::Element(el) => {
                let name = el.name().to_ascii_lowercase();
                if name == "script" || name == "style" {
                    continue;
                }
                if is_block_element(&name) {
                    buf.push(' ');
                }
            }
            _ => {}
        }
        if closing {
            continue;
        }
        if matches!(node.value(), Node::Element(_)) {
            stack.push((node, true));
        }
        let children: Vec<_> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push((child, false));
        }
    }
    collapse_whitespace(buf.trim())
}

/// Заменить последовательности whitespace на один пробел, сохранить начальные/
/// конечные пробелы как пустые строки убрать через trim снаружи.
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out
}

/// Блочный элемент html: тексты внутри отделяются пробелами от соседей.
fn is_block_element(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "hr"
            | "li"
            | "ul"
            | "ol"
            | "table"
            | "tr"
            | "td"
            | "th"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "blockquote"
            | "section"
    )
}

/// Конвертировать html-фрагмент в Markdown.
///
/// Порт `MarkdownHtmlHandler.kt` (alkoleft): обрабатывает h1-h6, p, br, strong/b,
/// em/i, code, pre, blockquote, ul/ol/li, a (с поддержкой `v8help://` ссылок —
/// они оборачиваются в `code`-кавычки, обычные ссылки — в `[text](href)`).
pub fn to_markdown(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let root = doc.tree.root();
    let mut state = MdState::default();
    walk(root, &mut state);
    state.output.trim().to_string()
}

#[derive(Default)]
struct MdState {
    output: String,
    list_level: usize,
    in_pre: bool,
    in_anchor: Option<String>, // href текущей ссылки
    anchor_text: String,
}

/// Итеративный обход дерева: глубокая вложенность (враждебная страница) не
/// должна переполнять стек — рекурсия здесь роняла процесс (abort).
fn walk(root: NodeRef<Node>, st: &mut MdState) {
    let mut stack: Vec<(NodeRef<Node>, bool)> = vec![(root, false)];
    while let Some((node, closing)) = stack.pop() {
        if closing {
            if let Node::Element(el) = node.value() {
                on_close_tag(&el.name().to_ascii_lowercase(), st);
            }
            continue;
        }
        match node.value() {
            Node::Text(text) => {
                if st.in_anchor.is_some() {
                    st.anchor_text.push_str(text);
                } else if st.in_pre || !text.trim().is_empty() {
                    // Whitespace-only узлы между блоками в Markdown не нужны
                    // (паритет с эталоном); внутри <pre> пробелы значимы.
                    st.output.push_str(text);
                }
            }
            Node::Element(el) => {
                let name = el.name().to_ascii_lowercase();
                on_open_tag(&name, el, st);
                stack.push((node, true));
            }
            _ => {}
        }
        let children: Vec<_> = node.children().collect();
        for child in children.into_iter().rev() {
            stack.push((child, false));
        }
    }
}

fn on_open_tag(name: &str, el: &scraper::node::Element, st: &mut MdState) {
    // Внутри ссылки inline-маркеры сломали бы её текст (`****[x](…)`),
    // внутри <pre> — исказили бы листинг: не выводим их.
    if st.in_anchor.is_some() && matches!(name, "strong" | "b" | "em" | "i" | "code") {
        return;
    }
    if st.in_pre && matches!(name, "strong" | "b" | "em" | "i" | "code" | "a") {
        return;
    }
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let lvl: usize = name[1..].parse().unwrap_or(1);
            st.output.push('\n');
            for _ in 0..lvl {
                st.output.push('#');
            }
            st.output.push(' ');
        }
        "p" => {
            if !st.output.is_empty() {
                st.output.push('\n');
            }
        }
        "br" => st.output.push('\n'),
        "strong" | "b" => st.output.push_str("**"),
        "em" | "i" => st.output.push('*'),
        "code" if !st.in_pre => st.output.push('`'),
        "pre" => {
            st.output.push_str("\n```\n");
            st.in_pre = true;
        }
        "blockquote" => st.output.push_str("\n> "),
        "ul" | "ol" => st.list_level += 1,
        "li" if st.list_level > 0 => {
            st.output.push('\n');
            for _ in 0..st.list_level.saturating_sub(1) {
                st.output.push_str("  ");
            }
            st.output.push_str("* ");
        }
        // Ячейки таблиц хотя бы не слипаются: границы — пробел/перевод строки.
        "tr" => st.output.push('\n'),
        "td" | "th" => st.output.push(' '),
        "a" => {
            let href = el.attr("href").map(|s| s.to_string()).unwrap_or_default();
            st.in_anchor = Some(href);
            st.anchor_text.clear();
        }
        _ => {}
    }
}

fn on_close_tag(name: &str, st: &mut MdState) {
    if st.in_anchor.is_some() && matches!(name, "strong" | "b" | "em" | "i" | "code") {
        return;
    }
    if st.in_pre && matches!(name, "strong" | "b" | "em" | "i" | "code" | "a") {
        return;
    }
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" => st.output.push('\n'),
        "strong" | "b" => st.output.push_str("**"),
        "em" | "i" => st.output.push('*'),
        "code" if !st.in_pre => st.output.push('`'),
        "pre" => {
            st.output.push_str("\n```\n");
            st.in_pre = false;
        }
        "blockquote" => st.output.push('\n'),
        "ul" | "ol" => {
            if st.list_level > 0 {
                st.list_level -= 1;
            }
        }
        "a" => {
            let href = st.in_anchor.take().unwrap_or_default();
            let text = collapse_whitespace(st.anchor_text.trim());
            if !text.is_empty() {
                if href.is_empty() {
                    // Ссылка без цели — просто текст, а не `[text]()`.
                    st.output.push_str(&text);
                } else if href.starts_with("v8help://") {
                    st.output.push('`');
                    st.output.push_str(&text);
                    st.output.push('`');
                } else if href
                    .chars()
                    .any(|c| matches!(c, '(' | ')' | ' ' | '<' | '>'))
                {
                    // Скобки/пробелы в URL ломают `[t](url)`; Markdown умеет
                    // обрамлять цель угловыми скобками, но не сами `<>`.
                    if href.chars().any(|c| c == '<' || c == '>') {
                        st.output.push_str(&text);
                    } else {
                        st.output.push('[');
                        st.output.push_str(&text);
                        st.output.push_str("](<");
                        st.output.push_str(&href);
                        st.output.push_str(">)");
                    }
                } else {
                    st.output.push('[');
                    st.output.push_str(&text);
                    st.output.push_str("](");
                    st.output.push_str(&href);
                    st.output.push(')');
                }
            }
            st.anchor_text.clear();
        }
        _ => {}
    }
}

// ---- helpers --------------------------------------------------------------

fn is_chapter_marker(child: NodeRef<Node>) -> bool {
    if let Node::Element(el) = child.value() {
        let name = el.name().to_ascii_lowercase();
        if name == "hr" {
            return true;
        }
        if matches!(name.as_str(), "p" | "div") {
            // 8.3.17 отдаёт маркер главой как `<div class="V8SH_chapter">`,
            // остальные версии — `<p class="V8SH_chapter">`.
            if el
                .attr("class")
                .is_some_and(|c| c.split_whitespace().any(|t| t == "V8SH_chapter"))
            {
                return true;
            }
        }
    }
    false
}

fn chapter_title_of(child: NodeRef<Node>) -> String {
    let mut buf = String::new();
    collect_text(child, &mut buf);
    collapse_whitespace(buf.trim())
}

fn collect_text(node: NodeRef<Node>, buf: &mut String) {
    // Итеративно: тот же мотив, что и в `walk` — глубина не должна ронять стек.
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if let Node::Text(text) = n.value() {
            buf.push_str(text);
        }
        let children: Vec<_> = n.children().collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
}

pub(crate) fn serialize_node(node: NodeRef<Node>) -> Option<String> {
    match node.value() {
        Node::Element(_) => {
            // scraper::ElementRef::html() — используем через попытку вернуться
            // к ElementRef. Для не-Element узлов (Text, Comment) — собираем сами.
            let element_ref = scraper::ElementRef::wrap(node)?;
            Some(element_ref.html())
        }
        Node::Text(text) => Some(escape_text(text)),
        _ => None,
    }
}

/// Экранировать текст для повторной сборки html: литеральные `&`, `<`, `>`
/// должны остаться текстом, а не превратиться в теги при повторном разборе
/// (`&lt;b&gt;` в исходной странице).
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_only_text_nodes_are_dropped() {
        assert_eq!(to_markdown("<p>a</p>\n<p>b</p>"), "a\n\nb");
    }

    #[test]
    fn deep_nesting_does_not_overflow_stack() {
        // 5000 вложенных <div> раньше роняли процесс (рекурсивный walk).
        let html = format!("{}x{}", "<div>".repeat(5000), "</div>".repeat(5000));
        let md = to_markdown(&html);
        assert!(md.contains('x'), "{md}");
    }

    #[test]
    fn extract_text_separates_blocks_and_skips_scripts() {
        assert_eq!(extract_text("<p>a</p><p>b</p>"), "a b");
        assert_eq!(
            extract_text("<script>var x=1;</script>Только чтение"),
            "Только чтение"
        );
    }

    #[test]
    fn inline_markup_inside_anchor_does_not_leak() {
        assert_eq!(
            to_markdown(r#"<a href="http://e"><strong>x</strong></a>"#),
            "[x](http://e)"
        );
        assert_eq!(to_markdown("<a>text</a>"), "text");
        assert_eq!(
            to_markdown(r#"<a href="http://x/a)b">t</a>"#),
            "[t](<http://x/a)b>)"
        );
    }

    #[test]
    fn inline_markup_inside_pre_is_suppressed() {
        assert_eq!(to_markdown("<pre><b>a</b></pre>"), "```\na\n```");
    }

    #[test]
    fn div_chapter_marker_is_recognized() {
        // 8.3.17: маркер главы — div, а не p.
        let html = r#"<body><div class="V8SH_chapter">Синтаксис:</div><p>x</p></body>"#;
        let chapters = split_chapters(html);
        assert_eq!(chapters.len(), 2, "{chapters:?}");
        assert_eq!(chapters[1].title, "Синтаксис:");
    }

    #[test]
    fn escaped_text_in_body_stays_literal() {
        let html =
            "<body><p class=\"V8SH_chapter\">Описание:</p>&lt;b&gt;жирный&lt;/b&gt; и текст</body>";
        let chapters = split_chapters(html);
        let body = &chapters.last().unwrap().body_html;
        assert!(body.contains("&lt;b&gt;"), "{body}");
        let md = to_markdown(body);
        assert!(md.contains("<b>жирный</b>"), "{md}");
        assert!(!md.contains("**"), "{md}");
    }

    #[test]
    fn table_cells_do_not_glue() {
        let md = to_markdown("<table><tr><td>a</td><td>b</td></tr></table>");
        assert_eq!(md, "a b");
    }
}
