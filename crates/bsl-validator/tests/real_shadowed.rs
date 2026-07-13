//! Integration-тесты `ShadowedContextName` на реальном `shcntx_ru.hbk`.
//!
//! Проверяет обе раскладки модуля формы (конфигурация и v8unpack-выгрузка
//! внешней обработки), правило A (свойство глобального контекста в любом
//! модуле), правило B (свойство `ФормаКлиентскогоПриложения` только в модуле
//! УПРАВЛЯЕМОЙ формы) и все четыре случая, когда имя занято НЕ бывает:
//! параметр процедуры, процедура «БезКонтекста», обычная форма и свойство,
//! доступное для записи. Каждый из них подтверждён кодом УТ 11.5.

use std::collections::HashSet;
use std::path::PathBuf;

use bsl_validator::{validate_module_with_profile, Confidence, ExprError, ExprErrorKind, Profile};
use platform_index::load_from_hbk;

const FORM_MODULE: &str = "base/Catalogs/Х/Forms/Ф/Ext/Form/Module.bsl";
const EXTERNAL_FORM_MODULE: &str = "External/Загрузка/Form/Форма/Form.obj.bsl";
const OBJECT_MODULE: &str = "base/Documents/Заказ/Ext/ObjectModule.bsl";
const COMMON_MODULE: &str = "base/CommonModules/Х/Ext/Module.bsl";

fn hbk_path() -> Option<PathBuf> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH")
        .ok()
        .map(PathBuf::from)?;
    let candidates = [
        root.join("shcntx_ru.hbk"),
        root.join("bin").join("shcntx_ru.hbk"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Только находки `ShadowedContextName` — остальные виды здесь не интересуют.
fn shadowed(errors: &[ExprError]) -> Vec<&ExprError> {
    errors
        .iter()
        .filter(|e| e.kind == ExprErrorKind::ShadowedContextName)
        .collect()
}

// ── Настоящая ошибка (инцидент 13.07.2026) ────────────────────────────────

#[test]
fn form_readonly_property_assignment_is_high() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "&НаСервере\nПроцедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    let found = shadowed(&result.errors);
    assert_eq!(found.len(), 1, "{:#?}", result.errors);
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn form_readonly_property_assignment_is_case_insensitive() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "&НаСервере\nПроцедура Т()\nпараметры = Новый Структура;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert_eq!(shadowed(&result.errors).len(), 1, "{:#?}", result.errors);
}

#[test]
fn external_processing_form_layout_is_high() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Раскладка v8unpack — ровно та, в которой лежал модуль из инцидента.
    let src = "&НаСервере\nПроцедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n";
    let result =
        validate_module_with_profile(&index, src, Some(EXTERNAL_FORM_MODULE), None, 1, Profile::Full);
    let found = shadowed(&result.errors);
    assert_eq!(found.len(), 1, "{:#?}", result.errors);
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn form_var_declaration_is_high() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "&НаСервере\nПроцедура Т()\nПерем Элементы;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    let found = shadowed(&result.errors);
    assert_eq!(found.len(), 1, "{:#?}", result.errors);
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn common_module_global_property_assignment_is_high() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "Процедура Т()\nДокументы = Новый Массив;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(COMMON_MODULE), None, 1, Profile::Full);
    let found = shadowed(&result.errors);
    assert_eq!(found.len(), 1, "{:#?}", result.errors);
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn no_context_procedure_still_checks_global_property() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Глобальный контекст доступен и в процедуре «БезКонтекста».
    let src = "&НаСервереБезКонтекста\nПроцедура Т()\nДокументы = Новый Массив;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert_eq!(shadowed(&result.errors).len(), 1, "{:#?}", result.errors);
}

// ── Реквизиты формы известны: правило A работает и внутри формы ────────────

#[test]
fn form_attributes_enable_global_rule_inside_form() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Состав реквизитов передан, `Справочники` среди них нет — значит имя занято
    // глобальным контекстом и присваивание упадёт. Без form_attributes валидатор
    // здесь молчит: он не знает, не реквизит ли это.
    let attrs: HashSet<String> = ["объект", "списокплатежей"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let src = "&НаСервере\nПроцедура Т()\nСправочники = Новый Соответствие;\nКонецПроцедуры\n";
    let result =
        validate_module_with_profile(&index, src, Some(FORM_MODULE), Some(&attrs), 1, Profile::Full);
    let found = shadowed(&result.errors);
    assert_eq!(found.len(), 1, "{:#?}", result.errors);
    assert_eq!(found[0].confidence, Confidence::High);
}

#[test]
fn form_attribute_name_is_silent_even_if_context_occupies_it() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // В УТ есть формы с реквизитом `Метаданные` — реквизит перекрывает свойство
    // глобального контекста, присваивание законно.
    let attrs: HashSet<String> = ["объект", "метаданные"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let src = "&НаСервере\nПроцедура Т(Знач Значение)\nМетаданные = Значение;\nКонецПроцедуры\n";
    let result =
        validate_module_with_profile(&index, src, Some(FORM_MODULE), Some(&attrs), 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn without_form_attributes_global_rule_stays_off_inside_form() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Состав реквизитов неизвестен — молчим, чтобы не выдать ложную находку.
    let src = "&НаСервере\nПроцедура Т()\nСправочники = Новый Соответствие;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

// ── Законный код: находки быть не должно ──────────────────────────────────

#[test]
fn procedure_parameter_frees_the_name() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Так пишет 1С в УТ: &НаКлиенте Процедура …(УчетнаяЗаписьНастроена, Параметры).
    let src = "&НаКлиенте\nПроцедура Т(Знач Результат, Параметры) Экспорт\n\
               Параметры = Новый Структура;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn no_context_procedure_frees_form_member_name() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Идиома БСП: контекста формы нет, форму передают параметром. На УТ 319 мест.
    let src = "&НаКлиентеНаСервереБезКонтекста\nПроцедура Т(Форма)\n\
               Элементы = Форма.Элементы;\nПараметры = Форма.Параметры;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn ordinary_form_module_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Обычная (неуправляемая) форма: директив компиляции нет, контекст другого
    // типа. На УТ так устроен модуль внешней обработки «Контур EDI» — 30 мест.
    let src = "Процедура КнопкаНажатие(Элемент)\nПараметры = Новый Структура();\nКонецПроцедуры\n";
    let result =
        validate_module_with_profile(&index, src, Some(EXTERNAL_FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn form_method_name_assignment_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Реквизит формы можно назвать как метод: в штатной форме учётной записи
    // ЭДО соседствуют `Закрыть = Ложь;` и `Закрыть();`.
    let src = "&НаКлиенте\nПроцедура Т()\nЗакрыть = Ложь;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn form_writable_property_assignment_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // Заголовок доступен для записи: присваивание задаёт заголовок формы —
    // штатный приём. На УТ 1393 места.
    let src = "&НаСервере\nПроцедура Т()\nЗаголовок = \"Тест\";\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn member_assignment_through_dot_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "&НаСервере\nПроцедура Т()\nЭлементы.Список.Видимость = Ложь;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn method_call_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "&НаКлиенте\nПроцедура Т()\nЗакрыть();\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn form_attribute_assignment_is_silent() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    // «Результат» — реквизит формы, а не член ФормаКлиентскогоПриложения.
    let src = "&НаСервере\nПроцедура Т()\nРезультат = \"текст\";\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(FORM_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn object_module_ignores_form_member() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "Процедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, Some(OBJECT_MODULE), None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}

#[test]
fn no_module_path_disables_form_rule() {
    let Some(path) = hbk_path() else { return };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let src = "&НаСервере\nПроцедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n";
    let result = validate_module_with_profile(&index, src, None, None, 1, Profile::Full);
    assert!(shadowed(&result.errors).is_empty(), "{:#?}", result.errors);
}
