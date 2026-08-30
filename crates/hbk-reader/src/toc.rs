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

use std::collections::HashMap;

use crate::error::{HbkError, Result};
use crate::models::{
    Chunk, DoubleLanguageString, NameContainer, NameObject, Page, PropertiesContainer, Toc,
};

const BOM: char = '\u{FEFF}';

// ============================================================================
// Tokenizer
// ============================================================================

fn tokenize(content: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let chars: Vec<char> = content.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == BOM {
            i += 1;
            continue;
        }
        if ch == '"' {
            if in_string {
                // Экранирование: "" внутри строки → одиночная кавычка.
                if i + 1 < chars.len() && chars[i + 1] == '"' {
                    current.push('"');
                    i += 1;
                } else {
                    current.push(ch);
                    tokens.push(std::mem::take(&mut current));
                    in_string = false;
                }
            } else {
                if !current.is_empty() {
                    tokens.push(current.trim().to_string());
                    current.clear();
                }
                current.push(ch);
                in_string = true;
            }
        } else if in_string {
            current.push(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
        } else if ch == '{' || ch == '}' || ch == ',' {
            if !current.is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
            tokens.push(ch.to_string());
        } else {
            current.push(ch);
        }
        i += 1;
    }
    if !current.is_empty() {
        tokens.push(current.trim().to_string());
    }
    tokens
        .into_iter()
        .filter(|t| !t.is_empty() && t != ",")
        .collect()
}

// ============================================================================
// Stream-helpers
// ============================================================================

struct TokenStream {
    tokens: Vec<String>,
    pos: usize,
}

impl TokenStream {
    fn new(tokens: Vec<String>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|s| s.as_str())
    }

    fn next(&mut self) -> Option<String> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
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
    let _root_count = s.parse_number("TableOfContent: ожидалось число chunkCount")?;

    let mut chunks: Vec<Chunk> = Vec::new();
    while let Some(t) = s.peek() {
        if t == "}" {
            break;
        }
        chunks.push(parse_chunk(&mut s)?);
    }
    Ok(chunks)
}

fn parse_chunk(s: &mut TokenStream) -> Result<Chunk> {
    s.expect("{", "Chunk: ожидался '{'")?;
    let id = s.parse_number("Chunk: ожидался id")?;
    let parent_id = s.parse_number("Chunk: ожидался parentId")?;
    let child_count = s.parse_number("Chunk: ожидался childCount")?;
    let mut child_ids = Vec::with_capacity(child_count.max(0) as usize);
    for i in 0..child_count {
        child_ids.push(s.parse_number(&format!("Chunk: ожидался childId #{}", i + 1))?);
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

fn parse_properties_container(s: &mut TokenStream) -> Result<PropertiesContainer> {
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

fn parse_name_container(s: &mut TokenStream) -> Result<NameContainer> {
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

fn parse_name_object(s: &mut TokenStream) -> Result<NameObject> {
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
    Ok(build_tree(chunks))
}

fn build_tree(chunks: Vec<Chunk>) -> Toc {
    // Шаг 1: собираем плоские структуры — сама страница, parent_id, порядок появления.
    // (id == 0 зарезервирован под виртуальный корень, реальные id у chunks обычно 1+.)
    let mut pages: HashMap<i32, Page> = HashMap::with_capacity(chunks.len() + 1);
    let mut parent_of: HashMap<i32, i32> = HashMap::with_capacity(chunks.len());
    let mut order: Vec<i32> = Vec::with_capacity(chunks.len());

    pages.insert(
        0,
        Page::new(DoubleLanguageString::new("TOC", "TOC"), ""),
    );

    for chunk in chunks {
        let title = chunk_title(&chunk);
        let html_path = chunk.properties.html_path.replace('"', "");
        let id = chunk.id;
        let parent_id = chunk.parent_id;
        if id == 0 {
            // защита от хитрых данных, где id == 0 (== виртуальный корень)
            continue;
        }
        pages.insert(id, Page::new(title, html_path));
        parent_of.insert(id, parent_id);
        order.push(id);
    }

    // Шаг 2: переносим страницы под родителей. Идём в обратном порядке, чтобы дочерние
    // страницы уже содержали своих внуков к моменту вставки в родителя.
    for id in order.iter().rev() {
        let parent_id = parent_of.get(id).copied().unwrap_or(0);
        let Some(child) = pages.remove(id) else {
            continue;
        };
        if let Some(parent) = pages.get_mut(&parent_id) {
            parent.children.insert(0, child);
        } else {
            // Сирота — безопасно переподвешиваем под root (id=0).
            if let Some(root) = pages.get_mut(&0) {
                root.children.insert(0, child);
            }
        }
    }

    let root = pages
        .remove(&0)
        .unwrap_or_else(|| Page::new(DoubleLanguageString::new("TOC", "TOC"), ""));
    Toc {
        pages: root.children,
    }
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
        let name = strip_quotes(&n.name);
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

fn strip_quotes(s: &str) -> String {
    s.replace('"', "")
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
        assert_eq!(
            toks,
            vec!["{", "\"ru\"", "\"Имя\"с\"кавычками\"", "}"]
        );
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
}
