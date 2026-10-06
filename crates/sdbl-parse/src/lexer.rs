//! Разбор текста запроса на лексемы.
//!
//! Язык запросов 1С двуязычен: у каждого ключевого слова есть русская и
//! английская форма, и обе встречаются в одной конфигурации. Регистр не значим,
//! причём кириллический тоже — сворачивать его надо средствами Rust, SQLite-подобное
//! `lower()` кириллицу не берёт.

/// Вид лексемы. Ключевые слова опознаются здесь же: дальше парсер работает
/// с `Keyword`, а не сравнивает строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Имя: таблица, поле, алиас. Точка отдаётся отдельной лексемой.
    Ident,
    Keyword(Kw),
    Number,
    /// Строковый литерал внутри запроса.
    Str,
    /// Параметр запроса `&Дата`.
    Param,
    /// Одиночный знак: `. , ( ) = < > + - * / ...`
    Punct,
}

/// Ключевые слова, которые различает подмножество. Всё остальное — `Ident`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    Select,
    Allowed,
    Distinct,
    Top,
    Into,
    From,
    Where,
    GroupBy,
    Having,
    OrderBy,
    Totals,
    IndexBy,
    Union,
    All,
    Join,
    Left,
    Right,
    Full,
    Inner,
    Outer,
    On,
    As,
    And,
    Or,
    Not,
    Drop,
    Case,
    When,
    Then,
    Else,
    End,
    Hierarchy,
    In,
    Is,
    Null,
    Like,
    Between,
    ForUpdate,
    AutoOrder,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Kind,
    /// Текст лексемы как он записан в запросе.
    pub text: String,
    /// Смещение начала в БАЙТАХ текста запроса — с ним находка ложится на
    /// строку модуля через карту `QueryText`.
    pub offset: usize,
}

impl Token {
    pub fn is(&self, kw: Kw) -> bool {
        self.kind == Kind::Keyword(kw)
    }

    pub fn is_punct(&self, c: char) -> bool {
        self.kind == Kind::Punct && self.text.starts_with(c)
    }
}

/// Ошибка лексического разбора: текст даже на лексемы не распался
/// (например, строковый литерал не закрыт).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub offset: usize,
}

/// Односложные ключевые слова: русская форма, английская форма, значение.
const KEYWORDS: &[(&str, &str, Kw)] = &[
    ("ВЫБРАТЬ", "SELECT", Kw::Select),
    ("РАЗРЕШЕННЫЕ", "ALLOWED", Kw::Allowed),
    ("РАЗЛИЧНЫЕ", "DISTINCT", Kw::Distinct),
    ("ПЕРВЫЕ", "TOP", Kw::Top),
    ("ПОМЕСТИТЬ", "INTO", Kw::Into),
    ("ИЗ", "FROM", Kw::From),
    ("ГДЕ", "WHERE", Kw::Where),
    ("ИМЕЮЩИЕ", "HAVING", Kw::Having),
    ("ИТОГИ", "TOTALS", Kw::Totals),
    // Одиночное `ПО` — всегда условие соединения: `СГРУППИРОВАТЬ ПО`,
    // `УПОРЯДОЧИТЬ ПО` и `ИНДЕКСИРОВАТЬ ПО` склеены в составные лексемы выше.
    ("ПО", "ON", Kw::On),
    ("ОБЪЕДИНИТЬ", "UNION", Kw::Union),
    ("ВСЕ", "ALL", Kw::All),
    ("СОЕДИНЕНИЕ", "JOIN", Kw::Join),
    ("ЛЕВОЕ", "LEFT", Kw::Left),
    ("ПРАВОЕ", "RIGHT", Kw::Right),
    ("ПОЛНОЕ", "FULL", Kw::Full),
    ("ВНУТРЕННЕЕ", "INNER", Kw::Inner),
    ("ВНЕШНЕЕ", "OUTER", Kw::Outer),
    ("КАК", "AS", Kw::As),
    ("И", "AND", Kw::And),
    ("ИЛИ", "OR", Kw::Or),
    ("НЕ", "NOT", Kw::Not),
    ("УНИЧТОЖИТЬ", "DROP", Kw::Drop),
    ("ВЫБОР", "CASE", Kw::Case),
    ("КОГДА", "WHEN", Kw::When),
    ("ТОГДА", "THEN", Kw::Then),
    ("ИНАЧЕ", "ELSE", Kw::Else),
    ("КОНЕЦ", "END", Kw::End),
    ("ИЕРАРХИЯ", "HIERARCHY", Kw::Hierarchy),
    ("В", "IN", Kw::In),
    ("ЕСТЬ", "IS", Kw::Is),
    ("NULL", "NULL", Kw::Null),
    ("ПОДОБНО", "LIKE", Kw::Like),
    ("МЕЖДУ", "BETWEEN", Kw::Between),
    ("АВТОУПОРЯДОЧИВАНИЕ", "AUTOORDER", Kw::AutoOrder),
];

/// Составные ключевые слова: разбираются как одна лексема, иначе `ПО` в
/// `СГРУППИРОВАТЬ ПО` неотличимо от `ПО` — условия соединения.
const COMPOUND: &[(&[&str], &[&str], Kw)] = &[
    (&["СГРУППИРОВАТЬ", "ПО"], &["GROUP", "BY"], Kw::GroupBy),
    (&["УПОРЯДОЧИТЬ", "ПО"], &["ORDER", "BY"], Kw::OrderBy),
    (&["ИНДЕКСИРОВАТЬ", "ПО"], &["INDEX", "BY"], Kw::IndexBy),
    (&["ДЛЯ", "ИЗМЕНЕНИЯ"], &["FOR", "UPDATE"], Kw::ForUpdate),
];

fn upper(s: &str) -> String {
    s.to_uppercase()
}

fn keyword_of(word: &str) -> Option<Kw> {
    let up = upper(word);
    KEYWORDS
        .iter()
        .find(|(ru, en, _)| *ru == up || *en == up)
        .map(|(_, _, kw)| *kw)
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Разбить текст запроса на лексемы. Незнакомые символы становятся `Punct` —
/// парсер сам решит, мешают они ему или нет. Незакрытый строковый литерал —
/// ошибка: молча проглотить остаток запроса значит выдать мусор за разбор.
pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let rest = &src[i..];
        let Some(c) = rest.chars().next() else { break };

        // U+FEFF (BOM) в начале текста запроса — разметка, а не знак.
        if c.is_whitespace() || c == '\u{FEFF}' {
            i += c.len_utf8();
            continue;
        }

        // Комментарий внутри текста запроса. Кончается по `\r` или `\n`
        // (грамматика SDBLLexer: тело — `~[\r\n]*`), а не только по `\n`:
        // иначе CR-only текст молча съедал бы весь остаток запроса.
        if rest.starts_with("//") {
            while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            continue;
        }

        // Строковый литерал: внутри запроса кавычка удваивается.
        if c == '"' {
            let start = i;
            i += 1;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return Err(LexError {
                    message: "незакрытый строковый литерал".to_string(),
                    offset: start,
                });
            }
            tokens.push(Token {
                kind: Kind::Str,
                text: src[start..i].to_string(),
                offset: start,
            });
            continue;
        }

        // Параметр запроса.
        if c == '&' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                let Some(ch) = src[i..].chars().next() else {
                    break;
                };
                if !is_ident_char(ch) {
                    break;
                }
                i += ch.len_utf8();
            }
            tokens.push(Token {
                kind: Kind::Param,
                text: src[start..i].to_string(),
                offset: start,
            });
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            let mut seen_dot = false;
            while i < bytes.len() {
                let Some(ch) = src[i..].chars().next() else {
                    break;
                };
                if ch.is_ascii_digit() {
                    i += 1;
                    continue;
                }
                if ch == '.' && !seen_dot {
                    // Грамматика FLOAT: DIGIT+ '.' DIGIT* — ровно одна точка.
                    // Вторая точка начинает другой токен: `1.2.3` — не число.
                    seen_dot = true;
                    i += 1;
                    continue;
                }
                break;
            }
            tokens.push(Token {
                kind: Kind::Number,
                text: src[start..i].to_string(),
                offset: start,
            });
            continue;
        }

        if is_ident_char(c) {
            let start = i;
            while i < bytes.len() {
                let Some(ch) = src[i..].chars().next() else {
                    break;
                };
                if !is_ident_char(ch) {
                    break;
                }
                i += ch.len_utf8();
            }
            let word = &src[start..i];
            let kind = match keyword_of(word) {
                Some(kw) => Kind::Keyword(kw),
                None => Kind::Ident,
            };
            tokens.push(Token {
                kind,
                text: word.to_string(),
                offset: start,
            });
            continue;
        }

        tokens.push(Token {
            kind: Kind::Punct,
            text: c.to_string(),
            offset: i,
        });
        i += c.len_utf8();
    }

    Ok(merge_compound(tokens, src))
}

/// Склеить составные ключевые слова (`СГРУППИРОВАТЬ ПО`, `ДЛЯ ИЗМЕНЕНИЯ` …)
/// в одну лексему.
///
/// `text` — точный срез источника от первого слова до конца второго: внутренние
/// пробелы и переводы строк сохраняются, поэтому `src[offset .. offset +
/// text.len()]` всегда валиден (раньше пробел синтезировался и при нескольких
/// пробелах срез уезжал в середину следующего слова).
///
/// После `КАК` или точки первое слово — имя (алиас/поле), а не начало
/// составного ключевого слова: `КАК Упорядочить ПО …` не склеивается.
fn merge_compound(tokens: Vec<Token>, src: &str) -> Vec<Token> {
    let uppers: Vec<String> = tokens.iter().map(|t| t.text.to_uppercase()).collect();
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut i = 0usize;

    while i < tokens.len() {
        let name_context = i > 0 && (tokens[i - 1].is(Kw::As) || tokens[i - 1].is_punct('.'));
        let mut matched = false;

        if !name_context {
            for (ru, en, kw) in COMPOUND {
                debug_assert_eq!(
                    ru.len(),
                    en.len(),
                    "пары COMPOUND должны совпадать по числу слов"
                );
                let len = ru.len();
                if len == 0 || i + len > tokens.len() {
                    continue;
                }
                let same = |pattern: &[&str]| {
                    pattern
                        .iter()
                        .enumerate()
                        .all(|(k, word)| uppers[i + k] == **word)
                };
                if same(ru) || same(en) {
                    let first = &tokens[i];
                    let last = &tokens[i + len - 1];
                    out.push(Token {
                        kind: Kind::Keyword(*kw),
                        text: src[first.offset..last.offset + last.text.len()].to_string(),
                        offset: first.offset,
                    });
                    i += len;
                    matched = true;
                    break;
                }
            }
        }

        if !matched {
            out.push(tokens[i].clone());
            i += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_case_and_language_insensitive() {
        let tokens = tokenize("выбрать SELECT Выбрать").unwrap();
        assert!(tokens.iter().all(|t| t.is(Kw::Select)), "{tokens:?}");
    }

    #[test]
    fn compound_keywords_are_one_token() {
        let tokens = tokenize("СГРУППИРОВАТЬ ПО Поле").unwrap();
        assert!(tokens[0].is(Kw::GroupBy));
        assert_eq!(tokens[1].kind, Kind::Ident);
    }

    #[test]
    fn standalone_by_is_not_group_by() {
        // `ПО` условия соединения не должно съедаться составным ключевым словом.
        let tokens = tokenize("ПО Т.Поле = Д.Поле").unwrap();
        assert!(tokens[0].is(Kw::On));
    }

    #[test]
    fn offsets_point_at_source() {
        let src = "ВЫБРАТЬ Товар ИЗ Справочник.Товары СГРУППИРОВАТЬ    ПО Товар";
        for token in tokenize(src).unwrap() {
            assert!(
                src[token.offset..].starts_with(&token.text),
                "лексема {:?} не на своём месте",
                token
            );
        }
    }

    /// Несколько пробелов внутри составного ключевого слова: `text` — точный
    /// срез источника, а не синтезированная склейка через один пробел.
    #[test]
    fn compound_text_is_source_slice() {
        let src = "СГРУППИРОВАТЬ \t ПО Т.Поле";
        let tokens = tokenize(src).unwrap();
        assert!(tokens[0].is(Kw::GroupBy));
        assert_eq!(tokens[0].text, "СГРУППИРОВАТЬ \t ПО");
        assert_eq!(
            &src[tokens[0].offset..tokens[0].offset + tokens[0].text.len()],
            "СГРУППИРОВАТЬ \t ПО"
        );
    }

    /// Алиас, совпавший со словом составного ключевого слова, не склеивается:
    /// `КАК Упорядочить ПО …` — это `КАК` + имя + условие соединения.
    #[test]
    fn alias_like_compound_word_is_not_merged() {
        let tokens = tokenize("КАК Упорядочить ПО А.Поле").unwrap();
        assert_eq!(tokens[0].kind, Kind::Keyword(Kw::As));
        assert_eq!(tokens[1].kind, Kind::Ident);
        assert!(tokens[2].is(Kw::On), "{tokens:?}");
    }

    #[test]
    fn params_and_strings_are_whole() {
        let tokens = tokenize("ГДЕ Дата >= &НачалоПериода И Имя = \"Иванов\"").unwrap();
        assert!(tokens
            .iter()
            .any(|t| t.kind == Kind::Param && t.text == "&НачалоПериода"));
        assert!(tokens.iter().any(|t| t.kind == Kind::Str));
    }

    #[test]
    fn cyrillic_identifier_is_one_token() {
        let tokens = tokenize("ТоварыНаСкладах").unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, Kind::Ident);
    }

    #[test]
    fn unclosed_string_is_error() {
        assert!(tokenize("ВЫБРАТЬ \"abc").is_err());
        // Экранированная кавычка в конце — тоже незакрытая строка.
        assert!(tokenize("ВЫБРАТЬ \"abc\"\"").is_err());
    }

    /// Комментарий кончается и по `\r`: CR-only текст не должен проглатывать
    /// остаток запроса.
    #[test]
    fn comment_ends_at_carriage_return() {
        let tokens = tokenize("ВЫБРАТЬ Поле // c\rИЗ Т").unwrap();
        assert!(tokens.iter().any(|t| t.is(Kw::From)), "{tokens:?}");
    }

    /// Число — не более одной точки: `1.2.3` бьётся на `1.2`, `.` и `3`.
    #[test]
    fn number_has_single_dot() {
        let tokens = tokenize("1.2.3").unwrap();
        assert_eq!(tokens.len(), 3, "{tokens:?}");
        assert_eq!(tokens[0].kind, Kind::Number);
        assert_eq!(tokens[0].text, "1.2");
        assert_eq!(tokens[1].kind, Kind::Punct);
        assert_eq!(tokens[2].kind, Kind::Number);
    }

    /// BOM в начале текста запроса не мешает разбору.
    #[test]
    fn bom_is_whitespace() {
        assert!(tokenize("\u{FEFF}ВЫБРАТЬ 1").unwrap()[0].is(Kw::Select));
    }
}
