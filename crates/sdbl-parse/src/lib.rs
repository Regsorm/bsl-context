//! Разбор подмножества языка запросов 1С.
//!
//! Крейт отвечает на структурные вопросы, которые нужны правилам оптимальности:
//! откуда запрос читает данные, как соединяет источники, кладёт ли результат во
//! временную таблицу и индексирует ли её. Выражения, агрегаты и предикаты он
//! глотает целиком, сохраняя из них только упомянутые поля, наличие `ИЛИ`
//! (в том числе на верхнем уровне условия) и признак пустого условия.
//!
//! Готовой интеграции грамматики SDBL с Rust нет: эталонная грамматика
//! `1c-syntax` написана под ANTLR, и тащить ANTLR-рантайм ради структурного
//! подмножества не стали. Отсюда разбор вручную — и сознательно неполный.
//!
//! Зависимостей у крейта нет: разбор текста не должен тянуть за собой ни
//! платформенный контекст, ни доступ к конфигурации.
//!
//! ```
//! let package = sdbl_parse::parse("ВЫБРАТЬ Т.Ссылка ИЗ Справочник.Товары КАК Т").unwrap();
//! assert_eq!(package.queries.len(), 1);
//! ```

mod ast;
mod lexer;
mod parser;

pub use ast::{Condition, Field, Join, JoinKind, MetaTable, Named, Package, Query, Source, Table};
pub use parser::{parse, ParseError};

/// Имена виртуальных таблиц регистров: пары «русское / английское».
///
/// Нужны, чтобы отличить `РегистрНакопления.Х.Остатки` (виртуальная таблица) от
/// `Документ.Х.Товары` (табличная часть): по третьему сегменту имени они
/// неразличимы, а правила про физические таблицы и отборы опираются именно на
/// это различие. Языки равноправны — запрос может быть написан на любом,
/// поэтому пара не должна разъезжаться.
const VIRTUAL_TABLES: &[(&str, &str)] = &[
    ("ОСТАТКИ", "BALANCE"),
    ("ОБОРОТЫ", "TURNOVERS"),
    ("ОСТАТКИИОБОРОТЫ", "BALANCEANDTURNOVERS"),
    ("ОБОРОТЫДТКТ", "DRCRTURNOVERS"),
    ("СРЕЗПОСЛЕДНИХ", "SLICELAST"),
    ("СРЕЗПЕРВЫХ", "SLICEFIRST"),
    ("ДВИЖЕНИЯССУБКОНТО", "RECORDSWITHEXTDIMENSIONS"),
    ("ОСТАТКИИОБОРОТЫДТКТ", "BALANCEANDTURNOVERSDRCR"),
    ("СУБКОНТО", "EXTDIMENSIONS"),
    ("ФАКТИЧЕСКИЙПЕРИОДДЕЙСТВИЯ", "ACTUALACTIONPERIOD"),
    ("ДАННЫЕГРАФИКА", "SCHEDULEDATA"),
    ("БАЗАНАЧИСЛЕНО", "BASE"),
];

/// Является ли третий сегмент имени таблицы виртуальной таблицей регистра.
pub fn is_virtual_table(name: &str) -> bool {
    let up = name.to_uppercase();
    VIRTUAL_TABLES.iter().any(|(ru, en)| *ru == up || *en == up)
}

/// Читает ли таблица регистр — физически или через виртуальную таблицу.
pub fn is_register(kind: &str) -> bool {
    let up = kind.to_uppercase();
    matches!(
        up.as_str(),
        "РЕГИСТРНАКОПЛЕНИЯ"
            | "РЕГИСТРСВЕДЕНИЙ"
            | "РЕГИСТРБУХГАЛТЕРИИ"
            | "РЕГИСТРРАСЧЕТА"
            | "ACCUMULATIONREGISTER"
            | "INFORMATIONREGISTER"
            | "ACCOUNTINGREGISTER"
            | "CALCULATIONREGISTER"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каждая пара виртуальных таблиц опознаётся в обеих языковых формах,
    /// пары уникальны — иначе одно из правил молча теряет ветку.
    #[test]
    fn virtual_tables_are_bilingual_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (ru, en) in VIRTUAL_TABLES {
            assert!(is_virtual_table(ru), "{ru}");
            assert!(is_virtual_table(&en.to_lowercase()), "{en}");
            assert!(seen.insert(*ru), "дубль {ru}");
            assert!(seen.insert(*en), "дубль {en}");
        }
        assert!(is_virtual_table("ОстаткиИОборотыДтКт"));
        assert!(is_virtual_table("BalanceAndTurnoversDrCr"));
        assert!(!is_virtual_table("Товары"));
    }
}
