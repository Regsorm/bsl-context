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

use platform_index::{load_from_hbk, Definition};

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
    for name in ["ТаблицаЗначений", "Массив", "Структура", "Запрос"]
    {
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

/// Кэш обязан отдавать ТОТ ЖЕ индекс, что собирается из hbk: холодный старт
/// ускоряется, а не подменяется другой версией данных. Первый вызов пишет
/// кэш (сборка из hbk), второй обязан прочитать его и совпасть.
#[test]
fn cache_round_trip_matches_fresh_build() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let dir = tempfile::tempdir().expect("временный каталог");
    let cache = dir.path().join("platform-index.cache");

    let fresh = load_from_hbk(&path).expect("сборка из hbk");

    let (loaded, source) = platform_index::load_cached(&path, &cache).expect("первая загрузка");
    assert_eq!(
        source,
        platform_index::LoadSource::Hbk,
        "первый старт обязан собрать индекс из hbk (кэша ещё нет)"
    );
    assert_eq!(loaded, fresh, "записанный кэш разошёлся со свежей сборкой");

    let (cached, source) = platform_index::load_cached(&path, &cache).expect("вторая загрузка");
    assert_eq!(
        source,
        platform_index::LoadSource::Cache,
        "второй старт обязан прочитать кэш, а не собирать индекс заново"
    );
    assert_eq!(cached, fresh, "индекс из кэша разошёлся со свежей сборкой");
}

/// Есть ли у сущности непустое примечание.
fn has_note(note: &Option<String>) -> bool {
    note.as_deref().is_some_and(|n| !n.trim().is_empty())
}

/// `note` и английские имена обязаны доезжать от страницы справки до вывода
/// инструментов: до этого `note` разбирался, но в домен не попадал вовсе, а
/// `name_en` не показывался нигде, кроме значений перечислений.
#[test]
fn notes_and_english_names_reach_the_output() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: hbk не найден");
        return;
    };
    let index = load_from_hbk(&path).expect("PlatformIndex должен загружаться");

    let mut notes = 0usize;
    let mut rendered = 0usize;
    let mut examples: Vec<String> = Vec::new();

    for ty in index.types.values() {
        // Английское имя — в заголовке типа.
        if !ty.name_en.is_empty() {
            let out = platform_index::format::format_type(ty);
            assert!(
                out.contains(&ty.name_en),
                "английское имя типа '{}' обязано быть в выводе",
                ty.name_ru
            );
        }
        // Примечание: тип, значение перечисления, метод, свойство, конструктор.
        if let Some(note) = ty.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            notes += 1;
            let out = platform_index::format::format_type(ty);
            assert!(
                out.contains("**Примечание:**"),
                "примечание типа '{}' не в выводе",
                ty.name_ru
            );
            assert!(
                out.contains(note.lines().next().unwrap_or("")),
                "текст примечания типа '{}' потерян",
                ty.name_ru
            );
            rendered += 1;
            if examples.len() < 3 {
                examples.push(format!(
                    "тип {} — {}",
                    ty.name_ru,
                    note.lines().next().unwrap_or("")
                ));
            }
        }
        if let Some(v) = ty.enum_values.iter().find(|v| has_note(&v.note)) {
            notes += 1;
            let out = platform_index::format::format_enum_values(&ty.enum_values, &ty.name_ru);
            assert!(
                out.contains("**Примечание:**"),
                "примечание значения '{}' не в выводе",
                v.name_ru
            );
            rendered += 1;
            if examples.len() < 3 {
                examples.push(format!(
                    "значение {} — {}",
                    v.name_ru,
                    v.note.as_deref().unwrap_or("").lines().next().unwrap_or("")
                ));
            }
        }
        if let Some(m) = ty.methods.iter().find(|m| has_note(&m.note)) {
            notes += 1;
            let out = platform_index::format::format_member(&Definition::Method(m.clone()));
            assert!(
                out.contains("**Примечание:**"),
                "примечание метода '{}' не в выводе",
                m.name_ru
            );
            rendered += 1;
            if examples.len() < 3 {
                examples.push(format!(
                    "метод {} — {}",
                    m.name_ru,
                    m.note.as_deref().unwrap_or("").lines().next().unwrap_or("")
                ));
            }
        }
        if let Some(p) = ty.properties.iter().find(|p| has_note(&p.note)) {
            notes += 1;
            let out = platform_index::format::format_member(&Definition::Property(p.clone()));
            assert!(
                out.contains("**Примечание:**"),
                "примечание свойства '{}' не в выводе",
                p.name_ru
            );
            rendered += 1;
            if examples.len() < 3 {
                examples.push(format!(
                    "свойство {} — {}",
                    p.name_ru,
                    p.note.as_deref().unwrap_or("").lines().next().unwrap_or("")
                ));
            }
        }
        if let Some(c) = ty.constructors.iter().find(|c| has_note(&c.note)) {
            notes += 1;
            let out =
                platform_index::format::format_constructors(std::slice::from_ref(c), &ty.name_ru);
            assert!(
                out.contains("**Примечание:**"),
                "примечание конструктора '{}' не в выводе",
                c.name
            );
            rendered += 1;
            if examples.len() < 3 {
                examples.push(format!(
                    "конструктор {} — {}",
                    c.name,
                    c.note.as_deref().unwrap_or("").lines().next().unwrap_or("")
                ));
            }
        }
    }

    println!("сущностей с примечанием: {notes}; проверок вывода: {rendered}");
    for example in &examples {
        println!("  пример: {example}");
    }
    assert!(
        notes > 0,
        "на 8.3.27 примечания есть (52 страницы значений перечислений и др.) — поле обязано доезжать до домена"
    );
    assert!(rendered > 0, "примечание обязано попадать в markdown-вывод");
}
