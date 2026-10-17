//! Интеграционные тесты issue #43: цепочки `Метаданные.<Коллекция>.<Объект>[.<Член>]`.
//!
//! До правки ветка `Метаданные.` не проверялась вовсе — из цепочки извлекалось
//! только первое звено, и выдуманные коллекция, объект и член описания проходили
//! молча. Индекс синтетический: список коллекций и состав описания берутся из
//! «справки» теста, имена объектов — из заглушки источника.

use bsl_validator::{
    validate_module_with_symbols, Confidence, ExprError, ExprErrorKind, Profile, SymbolSource,
};
use platform_index::{Method, PlatformIndex, Property, Type};

/// Источник имён: один справочник и один общий модуль.
struct Stub {
    /// `true` — источник отвечает «не знаю» на всё (не подключён).
    silent: bool,
}

impl Stub {
    fn known() -> Self {
        Self { silent: false }
    }

    fn silent() -> Self {
        Self { silent: true }
    }
}

impl SymbolSource for Stub {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        if self.silent {
            return None;
        }
        match collection {
            "Catalogs" => Some(name_lower == "номенклатура"),
            _ => None,
        }
    }

    fn describe(&self) -> String {
        "stub-issue43".to_string()
    }
}

fn prop(name: &str) -> Property {
    Property {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        type_name: String::new(),
        readonly: false,
    }
}

fn method(name: &str) -> Method {
    Method {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        return_type: String::new(),
        signatures: Vec::new(),
        note: None,
    }
}

fn ty(name: &str, props: Vec<Property>, methods: Vec<Method>) -> Type {
    Type {
        name_ru: name.into(),
        name_en: String::new(),
        description: String::new(),
        note: None,
        methods,
        properties: props,
        constructors: Vec::new(),
        enum_values: Vec::new(),
    }
}

/// Индекс: корень метаданных с четырьмя коллекциями и описание справочника.
fn index() -> PlatformIndex {
    let mut index = PlatformIndex::new();
    index.insert_type(ty(
        "ОбъектМетаданныхКонфигурация",
        vec![
            prop("Имя"),
            prop("Синоним"),
            prop("Справочники"),
            prop("Документы"),
            prop("РегистрыСведений"),
            prop("ОбщиеМодули"),
        ],
        Vec::new(),
    ));
    index.insert_type(ty(
        "ОбъектМетаданных: Справочник",
        vec![
            prop("Имя"),
            prop("Синоним"),
            prop("Реквизиты"),
            prop("ТабличныеЧасти"),
            prop("Формы"),
        ],
        vec![method("ОбновитьПредопределенныеДанные")],
    ));
    index
}

fn findings(src: &str, symbols: Option<&dyn SymbolSource>) -> Vec<ExprError> {
    validate_module_with_symbols(&index(), src, 3, Profile::Full, None, None, symbols).errors
}

fn probe(chain: &str) -> String {
    format!("Процедура Зонд()\n\tА = {chain};\nКонецПроцедуры\n")
}

fn kinds(errors: &[ExprError]) -> Vec<ExprErrorKind> {
    errors.iter().map(|e| e.kind).collect()
}

#[test]
fn parser_extracts_metadata_chains() {
    // Разбор цепочки целиком: из обычного DotFact видно только `Метаданные.Справочники`,
    // а имя объекта и члена — третий и четвёртый сегменты.
    let facts = bsl_validator::ast::collect_facts(
        "Процедура Зонд()\n\tА = Метаданные.Справочники.Номенклатура.Имя;\nКонецПроцедуры\n",
    );
    assert_eq!(facts.chains.len(), 1, "цепочка не разобралась");
    let chain = &facts.chains[0];
    assert_eq!(chain.head, "Метаданные");
    let segments: Vec<&str> = chain.segments.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(segments, ["Справочники", "Номенклатура", "Имя"]);
    assert_eq!(facts.dots.len(), 1, "прежний DotFact должен остаться");
}

// ── Выдуманные коллекция, объект и член — находки ────────────────────────────

#[test]
fn invented_collection_is_reported() {
    let found = findings(
        &probe("Метаданные.КоллекцииТакойНет123.ЧтоТо"),
        Some(&Stub::known()),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ExprErrorKind::UnknownMetadataObject);
    assert!(
        found[0].message.contains("Коллекции"),
        "{}",
        found[0].message
    );
}

#[test]
fn collection_typo_gets_suggestion() {
    // `Справочник` (единственное число) — рядом со `Справочники`.
    let found = findings(
        &probe("Метаданные.Справочник.Номенклатура"),
        Some(&Stub::known()),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].suggestion.as_deref(), Some("Справочники"));
}

#[test]
fn invented_object_is_reported() {
    let found = findings(
        &probe("Метаданные.Справочники.СправочникаТакогоНет123.Имя"),
        Some(&Stub::known()),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ExprErrorKind::UnknownMetadataObject);
    assert!(
        found[0].message.contains("СправочникаТакогоНет123"),
        "{}",
        found[0].message
    );
}

#[test]
fn invented_member_is_reported() {
    let found = findings(
        &probe("Метаданные.Справочники.Номенклатура.ПредопределенныеЗначения"),
        Some(&Stub::known()),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, ExprErrorKind::UnknownTypeMember);
    assert_eq!(found[0].confidence, Confidence::Low);
}

#[test]
fn member_typo_gets_suggestion() {
    let found = findings(
        &probe("Метаданные.Справочники.Номенклатура.Реквизити"),
        Some(&Stub::known()),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].suggestion.as_deref(), Some("Реквизиты"));
}

// ── Законные цепочки молчат ──────────────────────────────────────────────────

#[test]
fn existing_object_and_member_are_silent() {
    for chain in [
        "Метаданные.Справочники.Номенклатура.Имя",
        "Метаданные.Справочники.Номенклатура.Реквизиты",
        "Метаданные.Справочники.Номенклатура",
        "Метаданные.Документы",
        "Метаданные.Имя",
    ] {
        let found = findings(&probe(chain), Some(&Stub::known()));
        assert!(found.is_empty(), "{chain}: {found:?}");
    }
}

#[test]
fn without_source_object_check_is_silent() {
    // Источник имён не отвечает: коллекция проверяется (она из справки), а имя
    // объекта — нет, поэтому выдуманный справочник находкой не становится.
    let found = findings(
        &probe("Метаданные.Справочники.СправочникаТакогоНет123.Имя"),
        Some(&Stub::silent()),
    );
    assert!(found.is_empty(), "{found:?}");
    // ...но выдуманная КОЛЛЕКЦИЯ находится и без источника: её список есть в справке.
    let found = findings(
        &probe("Метаданные.КоллекцииТакойНет123.ЧтоТо"),
        Some(&Stub::silent()),
    );
    assert_eq!(kinds(&found), [ExprErrorKind::UnknownMetadataObject]);
}

#[test]
fn common_module_chain_is_silent() {
    // `ОбщиеМодули` — коллекция вне таблицы менеджеров: имени объекта источник
    // не знает, и находки быть не должно.
    let found = findings(
        &probe("Метаданные.ОбщиеМодули.НетТакогоМодуля123.Имя"),
        Some(&Stub::known()),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn short_chain_is_silent() {
    // Всего два сегмента — объекта в цепочке нет, проверять нечего.
    let found = findings(&probe("Метаданные.Справочники"), Some(&Stub::known()));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn locally_bound_head_is_silent() {
    // Параметр с именем `Метаданные` — так действительно пишут в УТ
    // (`CommonModules/НоменклатураСервер`: `МетаданныеРеквизитовТЧТовары =
    // Метаданные.Реквизиты`). Это описание объекта, а не метаданные конфигурации.
    // На корпусном прогоне без этой проверки появлялась ложная находка
    // «Коллекции 'Реквизиты' нет в метаданных конфигурации».
    let src = "Процедура Зонд(Метаданные)\n\tА = Метаданные.Реквизиты;\nКонецПроцедуры\n";
    let found = findings(src, Some(&Stub::known()));
    assert!(found.is_empty(), "{found:?}");

    let src =
        "Процедура Зонд()\n\tМетаданные = Описание;\n\tА = Метаданные.Реквизиты;\nКонецПроцедуры\n";
    let found = findings(src, Some(&Stub::known()));
    assert!(found.is_empty(), "{found:?}");
}
