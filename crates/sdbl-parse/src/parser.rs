//! Рекурсивный спуск по подмножеству языка запросов.
//!
//! Главное свойство: **непонятое молчит**. Парсер не является проверкой
//! синтаксиса — полной грамматики у него нет, и запрос, который он не осилил,
//! обязан приводить к `Err`, а не к наполовину разобранному дереву. Правило,
//! построенное на половине дерева, выдаёт находки на ровном месте.

use crate::ast::*;
use crate::lexer::{tokenize, Kind, Kw, Token};

/// Причина отказа. Текст нужен только для отладки и тестов: наружу, в находки,
/// ошибки разбора не выходят.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

type Result<T> = std::result::Result<T, ParseError>;

/// Предел вложенности подзапросов. Реальные запросы не уходят глубже
/// нескольких уровней; ограничение защищает от переполнения стека на
/// испорченном тексте.
const MAX_DEPTH: usize = 32;

/// Виды метаданных, которые могут стоять источником запроса. Список стабилен и
/// мал — тянуть его из конфигурации незачем.
const META_KINDS: &[(&str, &str)] = &[
    ("СПРАВОЧНИК", "CATALOG"),
    ("ДОКУМЕНТ", "DOCUMENT"),
    ("ЖУРНАЛДОКУМЕНТОВ", "DOCUMENTJOURNAL"),
    ("КОНСТАНТА", "CONSTANT"),
    ("ПЕРЕЧИСЛЕНИЕ", "ENUM"),
    ("ПЛАНВИДОВХАРАКТЕРИСТИК", "CHARTOFCHARACTERISTICTYPES"),
    ("ПЛАНСЧЕТОВ", "CHARTOFACCOUNTS"),
    ("ПЛАНВИДОВРАСЧЕТА", "CHARTOFCALCULATIONTYPES"),
    ("РЕГИСТРСВЕДЕНИЙ", "INFORMATIONREGISTER"),
    ("РЕГИСТРНАКОПЛЕНИЯ", "ACCUMULATIONREGISTER"),
    ("РЕГИСТРБУХГАЛТЕРИИ", "ACCOUNTINGREGISTER"),
    ("РЕГИСТРРАСЧЕТА", "CALCULATIONREGISTER"),
    ("БИЗНЕСПРОЦЕСС", "BUSINESSPROCESS"),
    ("ЗАДАЧА", "TASK"),
    ("ПЛАНОБМЕНА", "EXCHANGEPLAN"),
    ("ПОСЛЕДОВАТЕЛЬНОСТЬ", "SEQUENCE"),
    ("КРИТЕРИЙОТБОРА", "FILTERCRITERION"),
    ("ВНЕШНИЙИСТОЧНИКДАННЫХ", "EXTERNALDATASOURCE"),
];

fn is_meta_kind(word: &str) -> bool {
    let up = word.to_uppercase();
    META_KINDS.iter().any(|(ru, en)| *ru == up || *en == up)
}

/// Ключевые слова, начинающие секцию запроса. На них останавливается глотание
/// выражений: иначе список полей выборки съел бы весь запрос.
const SECTION_KEYWORDS: &[Kw] = &[
    Kw::Select,
    Kw::Into,
    Kw::From,
    Kw::Where,
    Kw::GroupBy,
    Kw::Having,
    Kw::OrderBy,
    Kw::Totals,
    Kw::IndexBy,
    Kw::Union,
    Kw::Join,
    Kw::Left,
    Kw::Right,
    Kw::Full,
    Kw::Inner,
    Kw::ForUpdate,
    Kw::AutoOrder,
];
// `Kw::On` в списке сознательно отсутствует: в секции `ИТОГИ … ПО …` он не
// начинает новую секцию, и остановка на нём обрывала бы разбор всего пакета.

/// Ключевые слова, которые не могут оказаться именем: они задают структуру
/// запроса, и принять их за алиас значит потерять секцию.
const STRUCTURAL: &[Kw] = &[
    Kw::Select,
    Kw::From,
    Kw::Into,
    Kw::Where,
    Kw::GroupBy,
    Kw::Having,
    Kw::OrderBy,
    Kw::Totals,
    Kw::IndexBy,
    Kw::Union,
    Kw::Join,
    Kw::Left,
    Kw::Right,
    Kw::Full,
    Kw::Inner,
    Kw::Outer,
    Kw::On,
    Kw::As,
];

/// Может ли лексема стоять именем.
///
/// Однобуквенные алиасы `В`, `И`, `НЕ` совпадают с ключевыми словами
/// (`IN`, `AND`, `NOT`), и без этой поблажки `СОЕДИНЕНИЕ ВТ КАК В ПО В.Поле = …`
/// обрывает разбор всего пакета.
fn is_name_like(token: &Token) -> bool {
    match token.kind {
        Kind::Ident => true,
        Kind::Keyword(kw) => !STRUCTURAL.contains(&kw),
        _ => false,
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

/// Разобрать текст запроса.
///
/// `Err` означает «я это не понял» и обязан приводить к молчанию правил, а не к
/// диагностике: непонятый запрос — вина парсера, а не автора кода.
pub fn parse(src: &str) -> Result<Package> {
    let tokens = tokenize(src);
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    parser.parse_package()
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn offset(&self) -> usize {
        self.peek().map(|t| t.offset).unwrap_or(0)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn eat_kw(&mut self, kw: Kw) -> bool {
        if self.peek().is_some_and(|t| t.is(kw)) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.peek().is_some_and(|t| t.is_punct(c)) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn at_kw(&self, kw: Kw) -> bool {
        self.peek().is_some_and(|t| t.is(kw))
    }

    fn err<T>(&self, message: &str) -> Result<T> {
        Err(ParseError {
            message: message.to_string(),
            offset: self.offset(),
        })
    }

    fn parse_package(&mut self) -> Result<Package> {
        let mut queries = Vec::new();

        while self.peek().is_some() {
            // Пустые операторы между запросами пакета.
            if self.eat_punct(';') {
                continue;
            }
            queries.push(self.parse_query()?);
        }

        if queries.is_empty() {
            return self.err("пакет не содержит запросов");
        }
        Ok(Package { queries })
    }

    fn parse_query(&mut self) -> Result<Query> {
        let offset = self.offset();

        // `УНИЧТОЖИТЬ ВТ` — отдельный вид оператора пакета.
        if self.eat_kw(Kw::Drop) {
            let name = self.parse_name()?;
            return Ok(Query {
                offset,
                drop_table: Some(name),
                ..Default::default()
            });
        }

        if !self.eat_kw(Kw::Select) {
            return self.err("ожидалось ВЫБРАТЬ");
        }

        let mut query = Query {
            offset,
            ..Default::default()
        };

        self.eat_kw(Kw::Allowed);
        self.eat_kw(Kw::Distinct);
        if self.eat_kw(Kw::Top) {
            // Число после ПЕРВЫЕ.
            self.bump();
        }

        // Список полей выборки глотается, но имена полей сохраняются: по ним
        // правила понимают, какие поля источника запросу нужны.
        query.select = Some(self.parse_condition());

        if self.eat_kw(Kw::Into) {
            query.into = Some(self.parse_name()?);
        }

        if self.eat_kw(Kw::From) {
            query.sources.push(self.parse_source()?);
            while self.eat_punct(',') {
                query.sources.push(self.parse_source()?);
            }
            self.parse_joins(&mut query)?;
        }

        if self.eat_kw(Kw::Where) {
            query.filter = Some(self.parse_condition());
        }

        // Секции, которые правилам безразличны.
        loop {
            if self.eat_kw(Kw::GroupBy)
                || self.eat_kw(Kw::Having)
                || self.eat_kw(Kw::OrderBy)
                || self.eat_kw(Kw::Totals)
                || self.eat_kw(Kw::ForUpdate)
                || self.eat_kw(Kw::AutoOrder)
            {
                self.skip_expression();
                continue;
            }
            break;
        }

        if self.eat_kw(Kw::IndexBy) {
            // `ИНДЕКСИРОВАТЬ ПО НАБОРАМ ((А, Б), (Б))` — составной индекс.
            // Правилам важно, что индекс задан; поля берём все, какие названы.
            let by_sets = self
                .peek()
                .is_some_and(|t| matches!(t.text.to_uppercase().as_str(), "НАБОРАМ" | "SETS"));
            if by_sets {
                self.pos += 1;
                for param in self.parse_call_params() {
                    for field in param.fields {
                        query.index_fields.push(Named {
                            name: field.path.join("."),
                            offset: field.offset,
                        });
                    }
                }
            } else {
                query.index_fields.push(self.parse_path_name()?);
                while self.eat_punct(',') {
                    query.index_fields.push(self.parse_path_name()?);
                }
            }
        }

        // `ОБЪЕДИНИТЬ` соединяет выборки; каждая разбирается как отдельный
        // запрос пакета — правила смотрят на источники, а не на объединение.
        if self.eat_kw(Kw::Union) {
            self.eat_kw(Kw::All);
        }

        Ok(query)
    }

    fn parse_joins(&mut self, query: &mut Query) -> Result<()> {
        loop {
            let offset = self.offset();
            let kind = if self.at_kw(Kw::Left) {
                self.pos += 1;
                JoinKind::Left
            } else if self.at_kw(Kw::Right) {
                self.pos += 1;
                JoinKind::Right
            } else if self.at_kw(Kw::Full) {
                self.pos += 1;
                JoinKind::Full
            } else if self.at_kw(Kw::Inner) {
                self.pos += 1;
                JoinKind::Inner
            } else if self.at_kw(Kw::Join) {
                JoinKind::Inner
            } else {
                return Ok(());
            };

            self.eat_kw(Kw::Outer);
            if !self.eat_kw(Kw::Join) {
                return self.err("ожидалось СОЕДИНЕНИЕ");
            }

            let source = self.parse_source()?;
            let on = if self.eat_kw(Kw::On) {
                Some(self.parse_condition())
            } else {
                None
            };

            query.joins.push(Join {
                kind,
                source,
                on,
                offset,
            });
        }
    }

    fn parse_source(&mut self) -> Result<Source> {
        // Подзапрос в источнике.
        if self.at_punct('(') {
            self.pos += 1;
            if self.depth >= MAX_DEPTH {
                return self.err("слишком глубокая вложенность подзапросов");
            }
            if !self.at_kw(Kw::Select) {
                return self.err("в источнике ожидался подзапрос");
            }
            self.depth += 1;
            let mut queries = vec![self.parse_query()?];
            // `ОБЪЕДИНИТЬ` внутри скобок: `parse_query` уже снял само слово,
            // осталось разобрать следующую выборку.
            while self.at_kw(Kw::Select) {
                queries.push(self.parse_query()?);
            }
            self.depth -= 1;
            if !self.eat_punct(')') {
                return self.err("не закрыта скобка подзапроса");
            }
            return Ok(Source {
                table: Table::Subquery(Box::new(Package { queries })),
                alias: self.parse_alias(),
            });
        }

        // Имя таблицы, переданное параметром.
        if let Some(token) = self.peek() {
            if token.kind == Kind::Param {
                let named = Named {
                    name: token.text.clone(),
                    offset: token.offset,
                };
                self.pos += 1;
                return Ok(Source {
                    table: Table::Parameter(named),
                    alias: self.parse_alias(),
                });
            }
        }

        let offset = self.offset();
        let mut parts = vec![self.parse_ident()?];
        while self.at_punct('.') {
            self.pos += 1;
            parts.push(self.parse_ident()?);
        }

        let table = if is_meta_kind(&parts[0]) && parts.len() >= 2 {
            let mut meta = MetaTable {
                kind: parts[0].clone(),
                name: parts[1].clone(),
                sub_table: parts.get(2).cloned(),
                params: Vec::new(),
                has_parens: false,
                offset,
            };
            if self.at_punct('(') {
                meta.has_parens = true;
                meta.params = self.parse_call_params();
            }
            Table::Meta(meta)
        } else {
            // Скобки после имени, не являющегося метаданными, — это уже не
            // таблица, а что-то, чего подмножество не знает.
            if self.at_punct('(') {
                return self.err("непонятный источник со скобками");
            }
            Table::Temp(Named {
                name: parts.join("."),
                offset,
            })
        };

        Ok(Source {
            table,
            alias: self.parse_alias(),
        })
    }

    /// Алиас источника: `КАК Имя` либо просто имя следом.
    fn parse_alias(&mut self) -> Option<Named> {
        if self.eat_kw(Kw::As) {
            // После `КАК` стоит имя, даже если оно совпало с ключевым словом.
            if let Some(token) = self.peek() {
                if is_name_like(token) {
                    let named = Named {
                        name: token.text.clone(),
                        offset: token.offset,
                    };
                    self.pos += 1;
                    return Some(named);
                }
            }
            return None;
        }
        if let Some(token) = self.peek() {
            if token.kind == Kind::Ident {
                let named = Named {
                    name: token.text.clone(),
                    offset: token.offset,
                };
                self.pos += 1;
                return Some(named);
            }
        }
        None
    }

    fn parse_ident(&mut self) -> Result<String> {
        // Заготовка под `СтрШаблон`: `ИЗ Документ.%1 КАК Т`. Настоящее имя
        // подставляется в рантайме, и правила про метаданные обязаны такие
        // имена пропускать — но остальной запрос разбирается как обычно.
        if self.at_punct('%') {
            if let Some(next) = self.tokens.get(self.pos + 1) {
                if next.kind == Kind::Number {
                    let text = format!("%{}", next.text);
                    self.pos += 2;
                    return Ok(text);
                }
            }
        }
        match self.peek() {
            Some(token) if token.kind == Kind::Ident => {
                let text = token.text.clone();
                self.pos += 1;
                Ok(text)
            }
            _ => self.err("ожидалось имя"),
        }
    }

    fn parse_name(&mut self) -> Result<Named> {
        let offset = self.offset();
        let name = self.parse_ident()?;
        Ok(Named { name, offset })
    }

    /// Имя, возможно квалифицированное точкой: поля `ИНДЕКСИРОВАТЬ ПО` в
    /// типовых пишут вместе с алиасом источника (`Таблица.Аналитика`).
    fn parse_path_name(&mut self) -> Result<Named> {
        let offset = self.offset();
        let mut parts = vec![self.parse_ident()?];
        while self.at_punct('.') {
            self.pos += 1;
            parts.push(self.parse_ident()?);
        }
        Ok(Named {
            name: parts.join("."),
            offset,
        })
    }

    fn at_punct(&self, c: char) -> bool {
        self.peek().is_some_and(|t| t.is_punct(c))
    }

    /// Разобрать скобки вызова, разделив содержимое по запятым верхнего уровня.
    fn parse_call_params(&mut self) -> Vec<Condition> {
        let mut params = Vec::new();
        if !self.eat_punct('(') {
            return params;
        }

        let mut depth = 1usize;
        let mut current = Condition {
            offset: self.offset(),
            is_empty: true,
            ..Default::default()
        };
        let mut path: Vec<String> = Vec::new();
        let mut path_offset = 0usize;
        let mut after_dot = false;

        while let Some(token) = self.peek().cloned() {
            if token.is_punct('(') {
                depth += 1;
            } else if token.is_punct(')') {
                depth -= 1;
                if depth == 0 {
                    self.pos += 1;
                    break;
                }
            }

            if depth == 1 && token.is_punct(',') {
                flush_path(&mut path, path_offset, &mut current);
                params.push(std::mem::take(&mut current));
                current.offset = token.offset;
                current.is_empty = true;
                after_dot = false;
                self.pos += 1;
                continue;
            }

            current.is_empty = false;
            if token.is(Kw::Or) {
                current.has_or = true;
                // Внутри скобок вызова верхним уровнем считается сам параметр.
                if depth == 1 {
                    current.has_top_level_or = true;
                }
            }
            let next_is_dot = self.tokens.get(self.pos + 1).is_some_and(|t| t.is_punct('.'));
            collect_field(
                &token,
                next_is_dot,
                &mut path,
                &mut path_offset,
                &mut after_dot,
                &mut current,
            );
            self.pos += 1;
        }

        flush_path(&mut path, path_offset, &mut current);
        if !current.is_empty || !params.is_empty() {
            params.push(current);
        }
        params
    }

    /// Проглотить выражение до ближайшего ключевого слова секции верхнего уровня.
    fn skip_expression(&mut self) {
        let mut depth = 0usize;
        while let Some(token) = self.peek() {
            if token.is_punct('(') {
                depth += 1;
            } else if token.is_punct(')') {
                if depth == 0 {
                    return; // скобка чужая — мы внутри подзапроса
                }
                depth -= 1;
            } else if token.is_punct(';') && depth == 0 {
                return;
            } else if depth == 0 {
                if let Kind::Keyword(kw) = token.kind {
                    if SECTION_KEYWORDS.contains(&kw) {
                        return;
                    }
                }
            }
            self.pos += 1;
        }
    }

    /// Проглотить условие, сохранив упомянутые поля и наличие `ИЛИ`.
    fn parse_condition(&mut self) -> Condition {
        let mut condition = Condition {
            offset: self.offset(),
            is_empty: true,
            ..Default::default()
        };
        let mut path: Vec<String> = Vec::new();
        let mut path_offset = 0usize;
        let mut after_dot = false;
        let mut depth = 0usize;

        while let Some(token) = self.peek().cloned() {
            if token.is_punct('(') {
                depth += 1;
            } else if token.is_punct(')') {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if token.is_punct(';') && depth == 0 {
                break;
            } else if depth == 0 {
                if let Kind::Keyword(kw) = token.kind {
                    if SECTION_KEYWORDS.contains(&kw) {
                        break;
                    }
                }
            }

            condition.is_empty = false;
            if token.is(Kw::Or) {
                condition.has_or = true;
                if depth == 0 {
                    condition.has_top_level_or = true;
                }
            }
            let next_is_dot = self.tokens.get(self.pos + 1).is_some_and(|t| t.is_punct('.'));
            collect_field(
                &token,
                next_is_dot,
                &mut path,
                &mut path_offset,
                &mut after_dot,
                &mut condition,
            );
            self.pos += 1;
        }

        flush_path(&mut path, path_offset, &mut condition);
        condition
    }
}

/// Накопить путь поля из лексем `Имя . Имя . Имя`.
///
/// `after_dot` обязателен: без него два имени подряд (`Товары Т` — источник и
/// его алиас) слиплись бы в один путь `Товары.Т`, которого в запросе нет.
fn collect_field(
    token: &Token,
    next_is_dot: bool,
    path: &mut Vec<String>,
    path_offset: &mut usize,
    after_dot: &mut bool,
    condition: &mut Condition,
) {
    // Ключевое слово рядом с точкой — это имя: `В.Ссылка`, `Т.В`. Само по себе
    // (`И`, `ИЛИ` между условиями) — оператор, и полем считаться не должно.
    let is_name = token.kind == Kind::Ident
        || (is_name_like(token) && (*after_dot || next_is_dot));
    if is_name {
        if !path.is_empty() && !*after_dot {
            flush_path(path, *path_offset, condition);
        }
        if path.is_empty() {
            *path_offset = token.offset;
        }
        path.push(token.text.clone());
        *after_dot = false;
        return;
    }
    if token.is_punct('.') && !path.is_empty() {
        *after_dot = true;
        return;
    }
    flush_path(path, *path_offset, condition);
    *after_dot = false;
}

fn flush_path(path: &mut Vec<String>, offset: usize, condition: &mut Condition) {
    if path.is_empty() {
        return;
    }
    condition.fields.push(Field {
        path: std::mem::take(path),
        offset,
    });
}
