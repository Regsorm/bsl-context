//! Integration-тест Phase 3 на реальном `shcntx_ru.hbk`.
//!
//! Acceptance:
//! - `types.len() > 1000`
//! - есть десятки/сотни типов с непустым `enum_values`
//! - канонический баг #638: `ТипРазмещенияТекстаТабличногоДокумента` имеет 4 значения
//! - `ТаблицаЗначений` имеет непустые `methods`, `properties`, `constructors`,
//!   у методов signatures непустые
//!
//! Запуск:
//! ```pwsh
//! $env:BSL_CONTEXT_PLATFORM_PATH = 'C:\Program Files\1cv8\8.3.27.1786'
//! cargo test -p platform-index --test real_index -- --nocapture
//! ```

use std::path::PathBuf;

use platform_index::load_from_hbk;

fn hbk_path() -> Option<PathBuf> {
    let root = std::env::var("BSL_CONTEXT_PLATFORM_PATH").ok().map(PathBuf::from)?;
    let candidates = [root.join("shcntx_ru.hbk"), root.join("bin").join("shcntx_ru.hbk")];
    candidates.into_iter().find(|p| p.exists())
}

#[test]
fn loads_real_platform_index() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан или shcntx_ru.hbk не найден");
        return;
    };

    let index = load_from_hbk(&path).expect("PlatformIndex должен загружаться");

    println!(
        "PlatformIndex: global_methods={}, global_properties={}, types={}, enum_types={}",
        index.global_methods.len(),
        index.global_properties.len(),
        index.types.len(),
        index.enum_types_count(),
    );

    // Границы — по реальным числам версии 8.3.27 (2414 типов, 500 методов,
    // 100 свойств) с запасом вниз. Проверка «не пусто» пропускала бы потерю
    // почти всего индекса: страницы справки выбрасываются молча, а у
    // пользователя это оборачивается находкой «метод не найден» на законном
    // вызове. Границы держат именно этот случай, а не точное число.
    assert!(
        index.types.len() >= 2000,
        "ожидается ≥2000 типов, получено {}",
        index.types.len()
    );
    assert!(
        index.enum_types_count() >= 500,
        "ожидается ≥500 типов-перечислений, получено {}",
        index.enum_types_count()
    );
    assert!(
        index.global_methods.len() >= 400,
        "ожидается ≥400 глобальных методов, получено {}",
        index.global_methods.len()
    );
    assert!(
        index.global_properties.len() >= 80,
        "ожидается ≥80 глобальных свойств, получено {}",
        index.global_properties.len()
    );

    // Поимённо: опорные элементы, потеря которых означает разъехавшийся разбор.
    for name in ["Сообщить", "СтрНайти", "ЗначениеЗаполнено"] {
        assert!(
            index.find_global_method(name).is_some(),
            "глобальный метод '{name}' пропал из индекса"
        );
    }
    for name in ["ТаблицаЗначений", "Массив", "Структура", "Запрос"] {
        assert!(
            index.find_type(name).is_some(),
            "тип '{name}' пропал из индекса"
        );
    }
    assert!(
        index.find_global_property("Справочники").is_some(),
        "свойство глобального контекста 'Справочники' пропало из индекса"
    );
}

/// Платформа принимает и русское, и английское написание. Оба пути поиска —
/// прямой по индексу и через `SearchEngine` (на нём стоят справочные
/// инструменты `info`/`get_member`) — обязаны отвечать одинаково.
#[test]
fn english_names_resolve_in_both_lookup_paths() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex");
    let engine = platform_index::SearchEngine::from_index(&index);

    assert!(index.find_global_method("Message").is_some());
    assert!(index.find_type("Array").is_some());
    assert!(index.find_global_property("Catalogs").is_some());

    assert!(
        engine.find_method("Message").is_some(),
        "info('Message') обязан находить тот же метод, что validate_method_call"
    );
    assert!(engine.find_type("Array").is_some());
    assert!(engine.find_property("Catalogs").is_some());
    // Русское имя не потеряно английским омонимом.
    assert!(engine.find_type("Массив").is_some());
    assert!(engine.find_method("Сообщить").is_some());
}

#[test]
fn enum_values_for_canonical_638() {
    let Some(path) = hbk_path() else {
        return;
    };

    let index = load_from_hbk(&path).expect("PlatformIndex");

    let ty = index
        .find_type("ТипРазмещенияТекстаТабличногоДокумента")
        .expect("тип ТипРазмещенияТекстаТабличногоДокумента должен быть в storage");

    assert!(ty.is_enum(), "тип должен быть распознан как перечисление");
    let values: Vec<&str> = ty.enum_values.iter().map(|v| v.name_ru.as_str()).collect();
    println!("enum_values ТипРазмещения...Документа: {values:?}");

    let expected = ["Авто", "Забивать", "Обрезать", "Переносить"];
    for name in expected {
        assert!(
            values.contains(&name),
            "значение {name} должно быть в enum_values"
        );
    }
}

#[test]
fn value_table_has_full_members() {
    let Some(path) = hbk_path() else {
        return;
    };

    let index = load_from_hbk(&path).expect("PlatformIndex");

    let ty = index
        .find_type("ТаблицаЗначений")
        .expect("тип ТаблицаЗначений должен быть в storage");

    println!(
        "ТаблицаЗначений: methods={}, properties={}, constructors={}",
        ty.methods.len(),
        ty.properties.len(),
        ty.constructors.len()
    );

    assert!(
        !ty.methods.is_empty(),
        "ТаблицаЗначений должна иметь методы (например, Добавить, Очистить)"
    );
    assert!(
        !ty.properties.is_empty(),
        "ТаблицаЗначений должна иметь свойства (например, Колонки)"
    );

    // У методов должны быть непустые signatures (главное исправление vs апстрим).
    let first_method = ty
        .methods
        .iter()
        .find(|m| !m.signatures.is_empty())
        .expect("хотя бы у одного метода ТаблицаЗначений должна быть signature");
    println!(
        "пример метода с signature: {} ({} перегрузок)",
        first_method.name_ru,
        first_method.signatures.len()
    );
}
