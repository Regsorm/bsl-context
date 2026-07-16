//! Интеграционные тесты `crate::config_objects` — проверка существования
//! объектов конфигурации (`crate::config_objects::check_config_objects`),
//! подключённой через `validate_module_with_symbols`.
//!
//! Источник — стаб с фиксированным набором имён (тот же приём, что у
//! `StubSource` в `crate::module::tests`, но здесь ещё умеет отвечать на
//! `object_exists`/`collection_names`):
//! - `CommonModules` = {"ОбщегоНазначения", "УправлениеДоступом"};
//! - `Catalogs` = {"Номенклатура"};
//! - `Documents` = {} (пусто, но коллекция ИЗВЕСТНА — не то же самое, что
//!   источник не умеет ответить).

use std::collections::HashSet;

use bsl_validator::{validate_module_with_symbols, Confidence, ExprErrorKind, Profile, SymbolSource};
use platform_index::{Method, PlatformIndex, Property, Type};

/// Источник-заглушка: фиксированный набор имён по трём коллекциям.
/// `silent` — имитация недоступного/не умеющего отвечать источника: тогда
/// `object_exists`/`collection_names` возвращают `None`, что валидатор обязан
/// принять за «не знаю» и промолчать.
struct StubSource {
    common_modules: HashSet<String>,
    catalogs: HashSet<String>,
    documents: HashSet<String>,
    /// Экспортные переменные модуля приложения (нижний регистр).
    global_vars: HashSet<String>,
    /// Умеет ли источник вообще отвечать про эти переменные (`false` → `None`).
    knows_global_vars: bool,
    silent: bool,
}

impl StubSource {
    fn new() -> Self {
        Self {
            common_modules: ["ОбщегоНазначения", "УправлениеДоступом"]
                .into_iter()
                .map(String::from)
                .collect(),
            catalogs: ["Номенклатура"].into_iter().map(String::from).collect(),
            documents: HashSet::new(),
            global_vars: ["параметрыприложения"].into_iter().map(String::from).collect(),
            knows_global_vars: true,
            silent: false,
        }
    }

    fn silent() -> Self {
        Self {
            silent: true,
            ..Self::new()
        }
    }

    /// Источник, умеющий всё, КРОМЕ экспортных переменных модуля приложения:
    /// так ведёт себя реализация, которая не научена их отдавать. Именно
    /// «не знаю» (`None`), а НЕ «знаю, их нет» (пустой набор) — разница
    /// принципиальная: пустой набор означал бы, что правило можно применять.
    fn without_global_vars() -> Self {
        Self {
            knows_global_vars: false,
            ..Self::new()
        }
    }
}

impl SymbolSource for StubSource {
    fn method_exists(&self, _name_lower: &str) -> bool {
        false
    }

    fn object_exists(&self, collection: &str, name_lower: &str) -> Option<bool> {
        if self.silent {
            return None;
        }
        let names: &HashSet<String> = match collection {
            "CommonModules" => &self.common_modules,
            "Catalogs" => &self.catalogs,
            "Documents" => &self.documents,
            _ => return None,
        };
        Some(names.iter().any(|n| n.to_lowercase() == name_lower))
    }

    fn collection_names(&self, collection: &str) -> Option<HashSet<String>> {
        if self.silent {
            return None;
        }
        match collection {
            "CommonModules" => Some(self.common_modules.clone()),
            "Catalogs" => Some(self.catalogs.clone()),
            "Documents" => Some(self.documents.clone()),
            _ => None,
        }
    }

    fn global_variables(&self) -> Option<HashSet<String>> {
        if self.silent || !self.knows_global_vars {
            return None;
        }
        Some(self.global_vars.clone())
    }

    fn describe(&self) -> String {
        "stub".to_string()
    }
}

fn empty_index() -> PlatformIndex {
    PlatformIndex::new()
}

/// Синтетический индекс платформы для проверки гейтов молчания. На ПУСТОМ
/// индексе три условия «это платформа, а не общий модуль» не работают вовсе
/// (`find_type`/`find_global_property` всегда отвечают «нет»), то есть тесты
/// поверх `empty_index()` их не проверяют. Здесь заведено ровно то, что нужно
/// каждому из них: глобальное свойство `Метаданные`, тип-перечисление
/// `ВидДвиженияНакопления` и контекст управляемой формы со свойством `Элементы`.
fn index_with_platform_context() -> PlatformIndex {
    let mut index = PlatformIndex::new();

    index.global_properties.push(Property {
        name_ru: "Метаданные".into(),
        name_en: "Metadata".into(),
        description: String::new(),
        type_name: String::new(),
        readonly: true,
    });

    index.insert_type(Type {
        name_ru: "ВидДвиженияНакопления".into(),
        name_en: "AccumulationRecordType".into(),
        description: String::new(),
        methods: Vec::new(),
        properties: Vec::new(),
        constructors: Vec::new(),
        enum_values: Vec::new(),
    });

    index.insert_type(Type {
        name_ru: "ФормаКлиентскогоПриложения".into(),
        name_en: "ManagedClientApplicationForm".into(),
        description: String::new(),
        methods: vec![Method {
            name_ru: "Закрыть".into(),
            name_en: "Close".into(),
            description: String::new(),
            return_type: String::new(),
            signatures: Vec::new(),
        }],
        properties: vec![Property {
            name_ru: "Элементы".into(),
            name_en: "Items".into(),
            description: String::new(),
            type_name: String::new(),
            readonly: true,
        }],
        constructors: Vec::new(),
        enum_values: Vec::new(),
    });

    index
}

fn attrs(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// 1. Выдуманный общий модуль без `module_path` (голый фрагмент) — ровно одна
/// находка `UnknownCommonModule`, `confidence = High`.
#[test]
fn invented_common_module_without_module_path_is_reported() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nУправлениеПрогрессом.Установить(1, 0);\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert_eq!(
        result.errors.len(),
        1,
        "ожидалась ровно одна находка: {:?}",
        result.errors
    );
    let finding = &result.errors[0];
    assert_eq!(finding.kind, ExprErrorKind::UnknownCommonModule);
    assert_eq!(finding.confidence, Confidence::High);
}

/// 2. Существующий общий модуль (`ОбщегоНазначения` есть в стабе) — находок нет.
#[test]
fn known_common_module_is_silent() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nОбщегоНазначения.МодульВыполненияЗапросов();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 3. Выдуманный справочник — `UnknownMetadataObject`.
#[test]
fn invented_catalog_is_reported() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nСправочники.НесуществующийСправочник.ПустаяСсылка();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownMetadataObject),
        "{:?}",
        result.errors
    );
}

/// 4. Существующий справочник (`Номенклатура` есть в стабе) — находок нет.
#[test]
fn known_catalog_is_silent() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nСправочники.Номенклатура.ПустаяСсылка();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 5. Затенение присваиванием: имя, которому только что присвоили значение,
/// не проверяется как общий модуль — ни когда оно реально есть в стабе
/// (`УправлениеДоступом`), ни когда оно выдумано (`УправлениеПрогрессом`).
#[test]
fn assignment_shadows_known_common_module_name() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nУправлениеДоступом = 5;\nУправлениеДоступом.Метод();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn assignment_shadows_invented_common_module_name() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nУправлениеПрогрессом = 5;\nУправлениеПрогрессом.Метод();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 6. Источник молчит (`object_exists`/`collection_names` возвращают `None`) —
/// находок нет ни на выдуманном общем модуле, ни на выдуманном справочнике.
#[test]
fn silent_source_suppresses_common_module_finding() {
    let index = empty_index();
    let source = StubSource::silent();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nУправлениеПрогрессом.Установить(1, 0);\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn silent_source_suppresses_catalog_finding() {
    let index = empty_index();
    let source = StubSource::silent();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nСправочники.НесуществующийСправочник.ПустаяСсылка();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 7. Одноимённые объекты в разных коллекциях: `Номенклатура` есть среди
/// `Catalogs`, но `Documents` пуста — находка только на втором.
#[test]
fn same_name_different_collections() {
    let index = empty_index();
    let source = StubSource::new();

    let catalog_result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nСправочники.Номенклатура.ПустаяСсылка();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(
        catalog_result.errors.is_empty(),
        "Справочники.Номенклатура должен быть тихим: {:?}",
        catalog_result.errors
    );

    let document_result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nДокументы.Номенклатура.ПустаяСсылка();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    assert!(
        document_result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownMetadataObject),
        "Документы.Номенклатура должен дать находку (Documents пуста): {:?}",
        document_result.errors
    );
}

/// 8. Опечатка в имени общего модуля даёт подсказку («возможно, вы имели в
/// виду...»), а выдуманное имя, не похожее ни на одно реальное, — нет.
#[test]
fn typo_gives_suggestion_invented_name_does_not() {
    let index = empty_index();
    let source = StubSource::new();

    let typo_result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nОбщегоНазначнеия.Метод();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    let typo_finding = typo_result
        .errors
        .iter()
        .find(|e| e.kind == ExprErrorKind::UnknownCommonModule)
        .expect("опечатка в имени общего модуля должна дать находку");
    assert_eq!(
        typo_finding.suggestion,
        Some("ОбщегоНазначения".to_string()),
        "{:?}",
        typo_result.errors
    );

    let invented_result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nУправлениеПрогрессом.Установить();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        None,
        None,
        Some(&source),
    );
    let invented_finding = invented_result
        .errors
        .iter()
        .find(|e| e.kind == ExprErrorKind::UnknownCommonModule)
        .expect("выдуманное имя тоже должно дать находку");
    assert_eq!(
        invented_finding.suggestion, None,
        "выдуманное имя не опечатка — подсказки быть не должно: {:?}",
        invented_result.errors
    );
}

/// 9а. Модуль объекта: `Товары` — реквизит объекта, а не общий модуль.
/// Валидатор не знает состава реквизитов объекта — молчит.
#[test]
fn object_module_requisite_is_silent() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nТовары.Очистить();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/Documents/Заказ/Ext/ObjectModule.bsl"),
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 9б. Модуль формы без переданных реквизитов — молчим совсем: любое имя
/// может оказаться реквизитом, а его состава валидатор не видит.
#[test]
fn form_module_without_attributes_is_silent() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nСписокТоваров.Очистить();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/Documents/Заказ/Forms/Форма/Ext/Form/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 9в. Модуль формы с известным реквизитом `СписокТоваров` — реквизит
/// перекрывает имя, находки нет.
#[test]
fn form_module_with_known_attribute_is_silent() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nСписокТоваров.Очистить();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/Documents/Заказ/Forms/Форма/Ext/Form/Module.bsl"),
        Some(&attrs(&["списоктоваров"])),
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 9г. Модуль формы с реквизитом `Объект` — основной реквизит формы, не
/// общий модуль, находки нет.
#[test]
fn form_module_object_attribute_is_silent() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nОбъект.Записать();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/Documents/Заказ/Forms/Форма/Ext/Form/Module.bsl"),
        Some(&attrs(&["объект"])),
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// 9д. Общий модуль: контекста объекта нет вовсе, `Товары` там не реквизит,
/// а либо описка, либо выдуманный модуль — находка ЕСТЬ.
#[test]
fn common_module_unknown_head_is_reported() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nТовары.Очистить();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownCommonModule),
        "{:?}",
        result.errors
    );
}

// ── Причины провала первого замера на корпусе УТ ──────────────────────────
//
// Прогон по 14905 модулям УТ дал 40398 находок на ЗАВЕДОМО рабочем коде.
// Обе причины закреплены тестами ниже: 15 зелёных тестов их не показали,
// потому что таких конструкций в тестах не было.

/// Переменная цикла `Для Каждого` связывается циклом, а не присваиванием.
/// Причина 39431 ложной находки из 40398: `КлючЗначение`, `Элемент`, `СтрокаТЧ`
/// — верхушка списка на корпусе УТ.
#[test]
fn foreach_loop_variable_is_not_a_common_module() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nДля Каждого КлючЗначение Из Структура Цикл\nЗ = КлючЗначение.Ключ;\nКонецЦикла;\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownCommonModule),
        "{:?}",
        result.errors
    );
}

/// Счётчик цикла `Для Сч = 1 По 10` — инициализатор тоже НЕ является
/// присваиванием в дереве разбора.
#[test]
fn counter_loop_variable_is_not_a_common_module() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nДля Сч = 1 По 10 Цикл\nЗ = Сч.Представление;\nКонецЦикла;\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownCommonModule),
        "{:?}",
        result.errors
    );
}

/// Экспортная переменная модуля приложения (`Перем ПараметрыПриложения Экспорт;`)
/// видна без префикса из любого клиентского модуля. Причина 123 ложных находок
/// из 343 на втором замере корпуса УТ — все на одном этом имени.
#[test]
fn application_module_variable_is_not_a_common_module() {
    let index = empty_index();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nПараметрыПриложения.Вставить(\"Ключ\", 1);\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// Источник не знает экспортных переменных модуля приложения → правило про
/// общий модуль молчит ЦЕЛИКОМ. Иначе каждое такое имя стало бы находкой.
/// Правило про объекты конфигурации при этом продолжает работать: там голова —
/// менеджер платформы, а не произвольное имя.
#[test]
fn source_without_global_variables_disables_common_module_rule_only() {
    let index = empty_index();
    let source = StubSource::without_global_vars();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nУправлениеПрогрессом.Установить(1, 0);\nС = Справочники.НетТакого.ПустаяСсылка();\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownCommonModule),
        "правило про общий модуль обязано молчать: {:?}",
        result.errors
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownMetadataObject),
        "правило про объекты должно продолжать работать: {:?}",
        result.errors
    );
}

/// Метод самого менеджера, а не имя объекта: `ПланыОбмена.ГлавныйУзел()`.
/// Причина остальных 967 ложных находок. Требует НАСТОЯЩЕЙ справки платформы:
/// тип менеджера и его методы берутся оттуда, на пустом индексе гейт
/// «сработает» по причине «в индексе ничего нет».
#[test]
#[ignore]
fn manager_own_method_is_not_a_metadata_object() {
    let Some(index) = real_platform_index() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let source = StubSource::new();
    for code in [
        "Узел = ПланыОбмена.ГлавныйУзел();",
        "Т = Справочники.ТипВсеСсылки();",
        "Т = Документы.ТипВсеСсылки();",
        "ПланыОбмена.ЗарегистрироватьИзменения(Узлы, Данные);",
    ] {
        let result = validate_module_with_symbols(
            &index,
            &format!("Процедура Тест()\n{code}\nКонецПроцедуры\n"),
            1,
            Profile::Full,
            Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
            None,
            Some(&source),
        );
        assert!(
            !result
                .errors
                .iter()
                .any(|e| e.kind == ExprErrorKind::UnknownMetadataObject),
            "{code} → {:?}",
            result.errors
        );
    }
}

/// Справка платформы, если она доступна в окружении.
fn real_platform_index() -> Option<PlatformIndex> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH").ok()?;
    let root = std::path::Path::new(&root);
    let hbk = [root.join("shcntx_ru.hbk"), root.join("bin").join("shcntx_ru.hbk")]
        .into_iter()
        .find(|p| p.exists())?;
    platform_index::load_from_hbk(&hbk).ok()
}

// ── Гейты «это платформа, а не общий модуль» ──────────────────────────────
//
// Проверяются ТОЛЬКО на синтетическом индексе: на пустом они молчат по причине
// «в индексе нет ничего», а не потому, что сработали. Каждый тест ниже устроен
// так, что снятие своего гейта роняет его в находку: имени головы нет в наборе
// `CommonModules` стаба, значит без гейта источник ответит `Some(false)`.

/// Голова — свойство глобального контекста платформы (`Метаданные`). Это не
/// общий модуль, находки быть не должно.
#[test]
fn platform_global_property_head_is_silent() {
    let index = index_with_platform_context();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nМ = Метаданные.Справочники;\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// Голова — имя платформенного типа (`ВидДвиженияНакопления.Приход`).
/// Существование значения проверяет `check_type_dot_members`, а не эта проверка;
/// общим модулем тип не является.
#[test]
fn platform_type_head_is_silent() {
    let index = index_with_platform_context();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "Процедура Тест()\nВ = ВидДвиженияНакопления.Приход;\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/CommonModules/МойМодуль/Ext/Module.bsl"),
        None,
        Some(&source),
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == ExprErrorKind::UnknownCommonModule),
        "{:?}",
        result.errors
    );
}

/// Голова — член контекста управляемой формы (`Элементы`). Реквизиты формы
/// известны, поэтому проверка включена, но `Элементы` — свойство самой формы,
/// а не общий модуль.
#[test]
fn form_context_member_head_is_silent() {
    let index = index_with_platform_context();
    let source = StubSource::new();
    let result = validate_module_with_symbols(
        &index,
        "&НаКлиенте\nПроцедура Тест()\nЭлементы.Список.Видимость = Ложь;\nКонецПроцедуры\n",
        1,
        Profile::Full,
        Some("base/Catalogs/Номенклатура/Forms/ФормаЭлемента/Ext/Form/Module.bsl"),
        Some(&attrs(&["объект"])),
        Some(&source),
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}
