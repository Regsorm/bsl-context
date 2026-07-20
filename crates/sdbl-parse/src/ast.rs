//! Дерево запроса — ровно та его часть, которой хватает правилам оптимальности.
//!
//! Полной грамматики здесь нет и не планируется: из 67 правил `SDBLParser.g4`
//! разбираются структурные (источники, соединения, временные таблицы, условия),
//! а выражения, `ВЫБОР`, агрегаты и предикаты глотаются по сбалансированным
//! скобкам. Из проглоченного сохраняются два признака — упомянутые поля и
//! наличие `ИЛИ`, больше правилам ничего не нужно.

/// Имя со смещением: без смещения находку некуда поставить.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub offset: usize,
}

/// Поле, упомянутое в условии: `Товары.Ссылка` → `["Товары", "Ссылка"]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub path: Vec<String>,
    pub offset: usize,
}

impl Field {
    /// Часть до первой точки — обычно алиас источника.
    pub fn qualifier(&self) -> Option<&str> {
        if self.path.len() > 1 {
            self.path.first().map(|s| s.as_str())
        } else {
            None
        }
    }

    /// Последняя часть пути — собственно поле.
    pub fn name(&self) -> &str {
        self.path.last().map(|s| s.as_str()).unwrap_or("")
    }
}

/// Проглоченный кусок выражения с сохранёнными признаками.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Condition {
    pub offset: usize,
    /// В условии есть `ИЛИ` на любом уровне вложенности.
    pub has_or: bool,
    /// `ИЛИ` стоит вне скобок, то есть разрывает само условие связи
    /// (`ПО А = Б ИЛИ В = Г`). `ИЛИ` внутри скобок при равенствах снаружи
    /// (`ПО А = Б И (Х > 0 ИЛИ У > 0)`) — обычный дополнительный отбор,
    /// индексу по полям связи он не мешает.
    pub has_top_level_or: bool,
    pub fields: Vec<Field>,
    /// Условие не содержит ни одной лексемы.
    pub is_empty: bool,
}

/// Таблица метаданных: `Справочник.Товары`, `РегистрНакопления.Х.Остатки(…)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaTable {
    /// Вид объекта как записан в запросе: `Справочник`, `РегистрНакопления`, …
    pub kind: String,
    pub name: String,
    /// Третий сегмент имени, если он есть. У регистров это виртуальная таблица
    /// (`Остатки`, `СрезПоследних`), у ссылочных объектов — табличная часть
    /// (`Документ.ЗаказКлиента.Товары`). Что именно, решает правило по `kind`:
    /// парсер их не различает, потому что по одному имени они неотличимы.
    pub sub_table: Option<String>,
    /// Параметры виртуальной таблицы, по одному на запятую верхнего уровня.
    pub params: Vec<Condition>,
    /// За именем стояли скобки — пусть даже пустые. Отличает `Остатки()` от
    /// `Остатки`: пустые скобки означают «параметры не заданы намеренно», а не
    /// «параметров у таблицы нет».
    pub has_parens: bool,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Table {
    Meta(MetaTable),
    /// Временная таблица — имя, не разложимое на вид метаданных.
    Temp(Named),
    /// Имя таблицы передано параметром: `ИЗ &ИмяТаблицыИзменений`. Что именно
    /// читается, из текста запроса не узнать — правила обязаны молчать про
    /// такой источник, но разбор остального запроса это ломать не должно.
    Parameter(Named),
    /// Подзапрос в скобках. Внутри может стоять не одна выборка, а несколько,
    /// объединённых через `ОБЪЕДИНИТЬ` — поэтому пакет, а не запрос.
    Subquery(Box<Package>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub table: Table,
    pub alias: Option<Named>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Left,
    Right,
    Full,
    Inner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Join {
    pub kind: JoinKind,
    pub source: Source,
    pub on: Option<Condition>,
    pub offset: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub offset: usize,
    /// `ПОМЕСТИТЬ ВТИмя` — запрос кладёт результат во временную таблицу.
    pub into: Option<Named>,
    /// Поля из `ИНДЕКСИРОВАТЬ ПО`.
    pub index_fields: Vec<Named>,
    /// Источники секции `ИЗ` (первый и перечисленные через запятую).
    pub sources: Vec<Source>,
    pub joins: Vec<Join>,
    /// Поля списка выборки. Само выражение не разбирается — нужны только
    /// имена: по ним видно, какие поля источника запросу вообще нужны
    /// (например, `Регистратор`, которого в таблице остатков не существует).
    pub select: Option<Condition>,
    /// Условие секции `ГДЕ`.
    pub filter: Option<Condition>,
    /// `УНИЧТОЖИТЬ ВТИмя` — уничтожение временной таблицы, не выборка.
    pub drop_table: Option<Named>,
}

impl Query {
    /// Все источники запроса — и корневой, и присоединённые.
    pub fn all_sources(&self) -> impl Iterator<Item = &Source> {
        self.sources.iter().chain(self.joins.iter().map(|j| &j.source))
    }
}

/// Пакет запросов: то, что записано в одном тексте через `;`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Package {
    pub queries: Vec<Query>,
}
