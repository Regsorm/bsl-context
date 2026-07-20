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
        self.kind == Kind::Punct && self.text.chars().next() == Some(c)
    }
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

/// Максимум слов в составном ключевом слове.
const COMPOUND_MAX_WORDS: usize = 2;

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
/// парсер сам решит, мешают они ему или нет.
pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let rest = &src[i..];
        let Some(c) = rest.chars().next() else { break };

        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }

        // Комментарий внутри текста запроса.
        if rest.starts_with("//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Строковый литерал: внутри запроса кавычка удваивается.
        if c == '"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
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
                let Some(ch) = src[i..].chars().next() else { break };
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
            while i < bytes.len() {
                let Some(ch) = src[i..].chars().next() else { break };
                if !ch.is_ascii_digit() && ch != '.' {
                    break;
                }
                i += ch.len_utf8();
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
                let Some(ch) = src[i..].chars().next() else { break };
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

    merge_compound(tokens)
}

/// Склеить составные ключевые слова в одну лексему.
fn merge_compound(tokens: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut i = 0usize;

    while i < tokens.len() {
        let mut matched = false;

        for (ru, en, kw) in COMPOUND {
            let len = ru.len().min(COMPOUND_MAX_WORDS);
            if i + len > tokens.len() {
                continue;
            }
            let window = &tokens[i..i + len];
            let same = |pattern: &[&str]| {
                window
                    .iter()
                    .zip(pattern.iter())
                    .all(|(tok, word)| upper(&tok.text) == **word)
            };
            if same(ru) || same(en) {
                out.push(Token {
                    kind: Kind::Keyword(*kw),
                    text: window
                        .iter()
                        .map(|t| t.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    offset: window[0].offset,
                });
                i += len;
                matched = true;
                break;
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
        let tokens = tokenize("выбрать SELECT Выбрать");
        assert!(tokens.iter().all(|t| t.is(Kw::Select)), "{tokens:?}");
    }

    #[test]
    fn compound_keywords_are_one_token() {
        let tokens = tokenize("СГРУППИРОВАТЬ ПО Поле");
        assert!(tokens[0].is(Kw::GroupBy));
        assert_eq!(tokens[1].kind, Kind::Ident);
    }

    #[test]
    fn standalone_by_is_not_group_by() {
        // `ПО` условия соединения не должно съедаться составным ключевым словом.
        let tokens = tokenize("ПО Т.Поле = Д.Поле");
        assert!(tokens[0].is(Kw::On));
    }

    #[test]
    fn offsets_point_at_source() {
        let src = "ВЫБРАТЬ Товар ИЗ Справочник.Товары";
        for token in tokenize(src) {
            assert!(
                src[token.offset..].starts_with(&token.text) || token.kind == Kind::Keyword(Kw::GroupBy),
                "лексема {:?} не на своём месте",
                token
            );
        }
    }

    #[test]
    fn params_and_strings_are_whole() {
        let tokens = tokenize("ГДЕ Дата >= &НачалоПериода И Имя = \"Иванов\"");
        assert!(tokens.iter().any(|t| t.kind == Kind::Param && t.text == "&НачалоПериода"));
        assert!(tokens.iter().any(|t| t.kind == Kind::Str));
    }

    #[test]
    fn cyrillic_identifier_is_one_token() {
        let tokens = tokenize("ТоварыНаСкладах");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, Kind::Ident);
    }
}
