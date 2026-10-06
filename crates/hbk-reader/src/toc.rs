//! Парсер текстового TOC.
//!
//! Формат (платформа 1С эмитирует именно так):
//!
//! ```text
//! {
//!   N    // количество корневых чанков
//!   {    // chunk
//!     id parentId childCount childId1 childId2 ...
//!     {  // PropertiesContainer
//!       n1 n2
//!       {  // NameContainer
//!         n1 n2
//!         {"langCode" "name"}   // NameObject (русский)
//!         {"langCode" "name"}   // NameObject (английский)
//!       }
//!       "htmlPath"
//!     }
//!   }
//!   ...
//! }
//! ```
//!
//! Tokenizer выделяет токены `{`, `}`, числа (без кавычек), строки в кавычках
//! (поддерживает экранирование `""` → `"`). Запятые игнорируются.
//!
//! Порт `Tokenizer.kt` + `TocParser.kt` + `Toc.kt` (alkoleft).

use std::borrow::Cow;
use std::collections::HashMap;

use crate::error::{HbkError, Result};
use crate::models::{
    Chunk, DoubleLanguageString, NameContainer, NameObject, Page, PropertiesContainer, Toc,
};

const BOM: char = '\u{FEFF}';

// ============================================================================
// Tokenizer
// ============================================================================

/// Разобрать поток токенов одним проходом по тексту.
///
/// Токены — срезы исходного текста (`Cow`: собственный `String` только для
/// строк с экранированием `""`, что редкость), а не новые `String` на каждый
/// токен: на TOC платформы это ~0,5 млн токенов, и две трети времени
/// `parse_toc` уходило именно на них. Раньше текст ещё и полностью
/// раскладывался в `Vec<char>` перед разбором, а запятые попадали в поток и
/// выбрасывались финальным фильтром — теперь этого нет.
///
/// Экранирование `""` внутри строки разворачивается в одну кавычку (как и
/// раньше); незакрытая строка в конце данных остаётся сырым срезом от
/// открывающей кавычки — `parse_string` учитывает оба случая.
fn tokenize(content: &str) -> Vec<Cow<'_, str>> {
    let mut tokens: Vec<Cow<'_, str>> = Vec::with_capacity(content.len() / 16);
    // Начало накапливаемого атома вне строки (число или имя без кавычек).
    let mut atom_start: Option<usize> = None;
    let mut string_start = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = content.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch == BOM {
            // BOM перед токеном пропускается, как и раньше. Внутри токена он
            // остаётся частью среза: данные платформы BOM в середине не содержат,
            // а прежнее вырезание из середины строки/числа делало бы вид, что
            // такой токен разобран верно.
            continue;
        }
        if in_string {
            if ch == '"' {
                if chars.peek().is_some_and(|(_, next)| *next == '"') {
                    // Экранирование: "" внутри строки → одиночная кавычка.
                    escaped = true;
                    chars.next();
                } else {
                    let end = i + ch.len_utf8();
                    tokens.push(unescape_cow(&content[string_start..end], escaped, true));
                    in_string = false;
                }
            }
            continue;
        }
        match ch {
            '"' => {
                if let Some(start) = atom_start.take() {
                    tokens.push(Cow::Borrowed(&content[start..i]));
                }
                string_start = i;
                escaped = false;
                in_string = true;
            }
            ch if ch.is_whitespace() || ch == ',' => {
                // Запятые — разделители, токенами они не становятся (раньше их
                // снимал финальный фильтр).
                if let Some(start) = atom_start.take() {
                    tokens.push(Cow::Borrowed(&content[start..i]));
                }
            }
            '{' | '}' => {
                if let Some(start) = atom_start.take() {
                    tokens.push(Cow::Borrowed(&content[start..i]));
                }
                tokens.push(Cow::Borrowed(&content[i..i + ch.len_utf8()]));
            }
            _ => {
                if atom_start.is_none() {
                    atom_start = Some(i);
                }
            }
        }
    }

    if in_string {
        // Данные оборвались внутри строки — токен от открывающей кавычки до
        // конца текста (это ожидает `parse_string`).
        tokens.push(unescape_cow(&content[string_start..], escaped, false));
    } else if let Some(start) = atom_start {
        tokens.push(Cow::Borrowed(&content[start..]));
    }
    tokens
}

/// Развернуть `""` в одну кавычку; без экранирования срез отдаётся как есть.
///
/// Пары выравниваются внутри СОДЕРЖИМОГО строки, а не от начала среза:
/// `raw.replace("\"\"", …)` от нулевого байта склеивал обрамляющую кавычку с
/// первой экранированной (`""""` — это строка из одной экранированной кавычки),
/// из-за чего `parse_string` отдавал не то, что прежде. `closed` отличает
/// закрытую строку (обе кавычки — обрамление) от оборванной в конце данных
/// (обрамление только открывающее).
fn unescape_cow<'a>(raw: &'a str, escaped: bool, closed: bool) -> Cow<'a, str> {
    if !escaped {
        return Cow::Borrowed(raw);
    }
    let inner_end = if closed { raw.len() - 1 } else { raw.len() };
    let inner = &raw[1..inner_end];
    let unescaped = inner.replace("\"\"", "\"");
    let mut out = String::with_capacity(raw.len());
    out.push('"');
    out.push_str(&unescaped);
    if closed {
        out.push('"');
    }
    Cow::Owned(out)
}

// ============================================================================
// Stream-helpers
// ============================================================================

struct TokenStream<'a> {
    /// Токены — срезы текста TOC: при выдаче ничего не клонируется.
    tokens: Vec<Cow<'a, str>>,
    pos: usize,
}

impl<'a> TokenStream<'a> {
    fn new(tokens: Vec<Cow<'a, str>>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(Cow::as_ref)
    }

    fn next(&mut self) -> Option<&str> {
        let t = self.tokens.get(self.pos).map(Cow::as_ref);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// Сколько токенов ещё не прочитано (для ограничения ёмкостей).
    fn remaining(&self) -> usize {
        self.tokens.len().saturating_sub(self.pos)
    }

    fn expect(&mut self, expected: &str, ctx: &str) -> Result<()> {
        let got = self
            .next()
            .ok_or_else(|| HbkError::TocParse(format!("{ctx}: не найден токен (конец данных)")))?;
        if got != expected {
            return Err(HbkError::TocParse(format!(
                "{ctx}: ожидался '{expected}', получен '{got}'"
            )));
        }
        Ok(())
    }

    fn parse_number(&mut self, ctx: &str) -> Result<i32> {
        let got = self
            .next()
            .ok_or_else(|| HbkError::TocParse(format!("{ctx}: не найден токен (конец данных)")))?;
        got.parse::<i32>()
            .map_err(|_| HbkError::TocParse(format!("{ctx}: ожидалось число, получено '{got}'")))
    }

    /// Как [`Self::parse_number`], но с номером элемента; `format!` контекста
    /// строится только при ошибке, а не на каждый разбор childId.
    fn parse_number_at(&mut self, ctx: &str, index: usize) -> Result<i32> {
        let got = self.next().ok_or_else(|| {
            HbkError::TocParse(format!("{ctx} #{index}: не найден токен (конец данных)"))
        })?;
        got.parse::<i32>().map_err(|_| {
            HbkError::TocParse(format!("{ctx} #{index}: ожидалось число, получено '{got}'"))
        })
    }

    fn parse_string(&mut self, ctx: &str) -> Result<String> {
        let got = self
            .next()
            .ok_or_else(|| HbkError::TocParse(format!("{ctx}: не найден токен (конец данных)")))?;
        // Длина проверяется первой: у токена ровно из одного символа `"` (его
        // оставляет `tokenize`, когда данные оборвались на открывающей кавычке)
        // обе проверки на кавычки истинны, и срез `[1..len-1]` паникует. Битый
        // блок должен давать TocParse, как все прочие искажения формата здесь.
        if got.len() < 2 || !got.starts_with('"') || !got.ends_with('"') {
            return Err(HbkError::TocParse(format!(
                "{ctx}: ожидалась строка в кавычках, получено '{got}'"
            )));
        }
        Ok(got[1..got.len() - 1].to_string())
    }
}

// ============================================================================
// Парсер чанков
// ============================================================================

fn parse_chunks(content: &str) -> Result<Vec<Chunk>> {
    let tokens = tokenize(content);
    let mut s = TokenStream::new(tokens);

    s.expect("{", "TableOfContent: ожидался '{'")?;
    let chunk_count = s.parse_number("TableOfContent: ожидалось число chunkCount")?;
    if chunk_count < 0 {
        return Err(HbkError::TocParse(format!(
            "TableOfContent: отрицательный chunkCount: {chunk_count}"
        )));
    }

    let mut chunks: Vec<Chunk> = Vec::new();
    while let Some(t) = s.peek() {
        if t == "}" {
            break;
        }
        chunks.push(parse_chunk(&mut s)?);
    }
    Ok(chunks)
}

fn parse_chunk(s: &mut TokenStream<'_>) -> Result<Chunk> {
    s.expect("{", "Chunk: ожидался '{'")?;
    let id = s.parse_number("Chunk: ожидался id")?;
    let parent_id = s.parse_number("Chunk: ожидался parentId")?;
    let child_count = s.parse_number("Chunk: ожидался childCount")?;
    if child_count < 0 {
        return Err(HbkError::TocParse(format!(
            "Chunk: отрицательный childCount: {child_count}"
        )));
    }
    // Ёмкость — не больше, чем осталось токенов: childCount из входных
    // данных не должен заказывать гигабайты памяти (амплификация).
    let mut child_ids = Vec::with_capacity((child_count as usize).min(s.remaining()));
    for i in 0..child_count {
        child_ids.push(s.parse_number_at("Chunk: ожидался childId", (i + 1) as usize)?);
    }
    let properties = parse_properties_container(s)?;
    s.expect("}", "Chunk: ожидался '}' в конце chunk")?;
    Ok(Chunk {
        id,
        parent_id,
        child_count,
        child_ids,
        properties,
    })
}

fn parse_properties_container(s: &mut TokenStream<'_>) -> Result<PropertiesContainer> {
    s.expect("{", "PropertiesContainer: ожидался '{'")?;
    let n1 = s.parse_number("PropertiesContainer: ожидался number1")?;
    let n2 = s.parse_number("PropertiesContainer: ожидался number2")?;
    let name_container = parse_name_container(s)?;
    let html_path = s.parse_string("PropertiesContainer: ожидался htmlPath")?;
    s.expect("}", "PropertiesContainer: ожидался '}' в конце")?;
    Ok(PropertiesContainer {
        number1: n1,
        number2: n2,
        name_container,
        html_path,
    })
}

fn parse_name_container(s: &mut TokenStream<'_>) -> Result<NameContainer> {
    s.expect("{", "NameContainer: ожидался '{'")?;
    let n1 = s.parse_number("NameContainer: ожидался number1")?;
    let n2 = s.parse_number("NameContainer: ожидался number2")?;

    let mut name_objects = Vec::new();
    if matches!(s.peek(), Some(t) if t != "}") {
        name_objects.push(parse_name_object(s)?);
        if matches!(s.peek(), Some(t) if t != "}") {
            name_objects.push(parse_name_object(s)?);
        }
    }
    s.expect("}", "NameContainer: ожидался '}' в конце")?;
    Ok(NameContainer {
        number1: n1,
        number2: n2,
        name_objects,
    })
}

fn parse_name_object(s: &mut TokenStream<'_>) -> Result<NameObject> {
    s.expect("{", "NameObject: ожидался '{'")?;
    let language_code = s.parse_string("NameObject: ожидался languageCode")?;
    let name = s.parse_string("NameObject: ожидался name")?;
    s.expect("}", "NameObject: ожидался '}' в конце")?;
    Ok(NameObject {
        language_code,
        name,
    })
}

// ============================================================================
// Сборка дерева Page из плоского списка Chunk-ов
// ============================================================================

/// Распарсить TOC из распакованного PackBlock (UTF-8 текст).
pub fn parse_toc(content: &str) -> Result<Toc> {
    let chunks = parse_chunks(content)?;
    build_tree(chunks)
}

/// Максимальная глубина дерева TOC. Реальные TOC платформы имеют глубину
/// ≤ 7 (замер на 8.3.17–8.5.1); лимит защищает от крафтовых цепочек
/// `parentId`, на которых рекурсивные `Drop`/`Clone` у `Page` переполняют стек.
const MAX_TOC_DEPTH: u32 = 256;

fn build_tree(chunks: Vec<Chunk>) -> Result<Toc> {
    // Шаг 1: собираем плоские структуры — сама страница, parent_id, порядок появления.
    // (id == 0 зарезервирован под виртуальный корень, реальные id у chunks обычно 1+.)
    let mut pages: HashMap<i32, Page> = HashMap::with_capacity(chunks.len() + 1);
    let mut parent_of: HashMap<i32, i32> = HashMap::with_capacity(chunks.len());
    let mut order: Vec<i32> = Vec::with_capacity(chunks.len());
    // Высота поддерева (в рёбрах) для уже собранных узлов.
    let mut height: HashMap<i32, u32> = HashMap::with_capacity(chunks.len());

    pages.insert(0, Page::new(DoubleLanguageString::new("TOC", "TOC"), ""));

    for chunk in chunks {
        let title = chunk_title(&chunk);
        let html_path = chunk.properties.html_path.clone();
        let id = chunk.id;
        let parent_id = chunk.parent_id;
        if id == 0 {
            // защита от хитрых данных, где id == 0 (== виртуальный корень)
            continue;
        }
        if pages.insert(id, Page::new(title, html_path)).is_some() {
            return Err(HbkError::TocParse(format!("Chunk: повторный id {id}")));
        }
        parent_of.insert(id, parent_id);
        order.push(id);
    }

    // Шаг 2: переносим страницы под родителей. Идём в обратном порядке, чтобы дочерние
    // страницы уже содержали своих внуков к моменту вставки в родителя.
    for id in order.iter().rev() {
        let mut parent_id = parent_of.get(id).copied().unwrap_or(0);
        let Some(child) = pages.remove(id) else {
            continue;
        };
        // Сирота — безопасно переподвешиваем под root (id=0).
        if !pages.contains_key(&parent_id) {
            parent_id = 0;
        }
        let child_height = height.get(id).copied().unwrap_or(0) + 1;
        if child_height > MAX_TOC_DEPTH {
            return Err(HbkError::TocParse(format!(
                "глубина TOC превышает лимит {MAX_TOC_DEPTH} (id={id})"
            )));
        }
        let h = height.entry(parent_id).or_insert(0);
        *h = (*h).max(child_height);
        if let Some(parent) = pages.get_mut(&parent_id) {
            parent.children.insert(0, child);
        }
    }

    let root = pages
        .remove(&0)
        .unwrap_or_else(|| Page::new(DoubleLanguageString::new("TOC", "TOC"), ""));
    Ok(Toc {
        pages: root.children,
    })
}

fn chunk_title(chunk: &Chunk) -> DoubleLanguageString {
    let names = &chunk.properties.name_container.name_objects;

    // Улучшение vs апстрим: апстрим при одном имени всегда клал его в `en`,
    // даже если language_code был "ru" — например для корня «Глобальный контекст»
    // в платформе 8.3.27 это давало `ru="" en="Глобальный контекст"`. Мы смотрим
    // на сам language_code, чтобы заполнить правильное поле.
    let mut ru = String::new();
    let mut en = String::new();
    for n in names {
        let name = n.name.clone();
        let lang = n.language_code.to_ascii_lowercase();
        match lang.as_str() {
            "ru" if ru.is_empty() => ru = name,
            "en" if en.is_empty() => en = name,
            // Незнакомый код языка либо повторное имя — кладём в первое
            // свободное поле, чтобы не потерять данные.
            _ => {
                if ru.is_empty() {
                    ru = name;
                } else if en.is_empty() {
                    en = name;
                }
            }
        }
    }
    DoubleLanguageString::new(en, ru)
}

// ============================================================================
// Тесты
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_simple_braces() {
        let toks = tokenize("{ 1 2 }");
        assert_eq!(toks, vec!["{", "1", "2", "}"]);
    }

    #[test]
    fn tokenize_quoted_string() {
        let toks = tokenize(r#"{"ru" "Имя"}"#);
        assert_eq!(toks, vec!["{", "\"ru\"", "\"Имя\"", "}"]);
    }

    #[test]
    fn tokenize_escaped_quote_inside_string() {
        let toks = tokenize(r#"{"ru" "Имя""с""кавычками"}"#);
        // экранирование "" → одиночная кавычка внутри строки
        assert_eq!(toks, vec!["{", "\"ru\"", "\"Имя\"с\"кавычками\"", "}"]);
    }

    /// Строка из одних экранированных кавычек: обрамление не должно попадать в
    /// пару при разворачивании `""` (регресс: `""""` — строка из одной
    /// экранированной кавычки — читалось как пустая строка).
    #[test]
    fn tokenize_only_escaped_quotes() {
        assert_eq!(tokenize("\"\"\"\""), vec!["\"\"\""]);
        assert_eq!(tokenize("\"\"\"\"\"\""), vec!["\"\"\"\""]);
    }

    /// Обрыв данных: незакрытая строка отдаётся сырым срезом от открывающей
    /// кавычки (одиночная `"` в конце — токен из одного символа, `parse_string`
    /// такой отвергает, не паникуя); пары внутри оборванной строки развёрнуты.
    #[test]
    fn tokenize_unclosed_string_is_raw_slice() {
        assert_eq!(tokenize("\"Имя"), vec!["\"Имя"]);
        assert_eq!(tokenize("\""), vec!["\""]);
        assert_eq!(tokenize("\"\"\""), vec!["\"\""]);
    }

    #[test]
    fn tokenize_ignores_commas_and_bom() {
        let toks = tokenize("\u{FEFF}{ 1, 2 }");
        assert_eq!(toks, vec!["{", "1", "2", "}"]);
    }

    /// Минимальный синтетический TOC из одного чанка с двуязычным названием.
    #[test]
    fn parse_minimal_toc() {
        let content = r#"
        {
          1
          {
            10
            0
            0
            {
              0 0
              {
                0 0
                {"ru" "ОбщееНазваниеРу"}
                {"en" "GeneralNameEn"}
              }
              "v8help/topic.html"
            }
          }
        }"#;
        let toc = parse_toc(content).expect("toc parse ok");
        assert_eq!(toc.pages.len(), 1);
        assert_eq!(toc.pages[0].title.ru, "ОбщееНазваниеРу");
        assert_eq!(toc.pages[0].title.en, "GeneralNameEn");
        assert_eq!(toc.pages[0].html_path, "v8help/topic.html");
    }

    /// Иерархия: один родитель + два дочерних (порядок сохраняется).
    #[test]
    fn parse_toc_hierarchy() {
        let content = r#"
        {
          1
          {
            1 0 2 2 3
            { 0 0 { 0 0 {"ru" "Корень"} {"en" "Root"} } "root.html" }
          }
          {
            2 1 0
            { 0 0 { 0 0 {"ru" "Первый"} {"en" "First"} } "child1.html" }
          }
          {
            3 1 0
            { 0 0 { 0 0 {"ru" "Второй"} {"en" "Second"} } "child2.html" }
          }
        }"#;
        let toc = parse_toc(content).expect("toc parse ok");
        assert_eq!(toc.pages.len(), 1);
        let root = &toc.pages[0];
        assert_eq!(root.title.ru, "Корень");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].title.ru, "Первый");
        assert_eq!(root.children[1].title.ru, "Второй");
    }

    /// Кавычки внутри имён и путей — часть данных (после `""`-экранирования),
    /// а не обрамление: парсер не должен их вырезать.
    #[test]
    fn parse_toc_preserves_inner_quotes() {
        let content = r#"
        {
          1
          {
            1 0 0
            { 0 0 { 0 0 {"ru" "Имя""с""кавычками"} {"en" "Name ""q"" here"} } "a""b.html" }
          }
        }"#;
        let toc = parse_toc(content).expect("toc parse ok");
        assert_eq!(toc.pages[0].title.ru, "Имя\"с\"кавычками");
        assert_eq!(toc.pages[0].title.en, "Name \"q\" here");
        assert_eq!(toc.pages[0].html_path, "a\"b.html");
    }

    #[test]
    fn parse_toc_rejects_duplicate_id() {
        let content = r#"
        {
          2
          { 1 0 0 { 0 0 { 0 0 {"ru" "Первый"} } "a.html" } }
          { 1 0 0 { 0 0 { 0 0 {"ru" "Второй"} } "b.html" } }
        }"#;
        let err = parse_toc(content).expect_err("дубликат id должен быть ошибкой");
        assert!(matches!(err, HbkError::TocParse(_)));
    }

    #[test]
    fn parse_toc_rejects_negative_child_count() {
        let content = r#"
        {
          1
          { 1 0 -3 { 0 0 { 0 0 {"ru" "Имя"} } "a.html" } }
        }"#;
        let err = parse_toc(content).expect_err("отрицательный childCount должен быть ошибкой");
        assert!(matches!(err, HbkError::TocParse(_)));
    }

    /// Крафтовая цепочка `parentId` не должна приводить к переполнению стека
    /// при рекурсивном `Drop` дерева: глубина ограничивается на парсинге.
    #[test]
    fn parse_toc_depth_limit() {
        let n = MAX_TOC_DEPTH as usize + 1;
        let mut content = String::from("{\n");
        content.push_str(&format!("{n}\n"));
        for id in 1..=n {
            let parent = id - 1;
            content.push_str(&format!(
                "{{ {id} {parent} 0 {{ 0 0 {{ 0 0 {{\"ru\" \"P{id}\"}} }} \"p{id}.html\" }} }}\n"
            ));
        }
        content.push('}');
        let err = parse_toc(&content).expect_err("глубина выше лимита должна быть ошибкой");
        assert!(matches!(err, HbkError::TocParse(_)));
    }
}
