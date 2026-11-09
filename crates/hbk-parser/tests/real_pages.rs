//! Integration-тесты на реальном `shcntx_ru.hbk`: парсим страницы конкретных
//! методов, свойств, объектов, конструкторов. Используем как acceptance для
//! Phase 2.
//!
//! Запуск:
//! ```pwsh
//! $env:BSL_CONTEXT_PLATFORM_PATH = 'C:\Program Files\1cv8\8.3.27.1786'
//! cargo test -p hbk-parser --test real_pages -- --nocapture
//! ```

use std::path::PathBuf;

use hbk_parser::{
    parse_constructor_page, parse_method_page, parse_object_page, parse_property_page,
    AvailabilityInfo,
};
use hbk_reader::{HbkContent, Page};

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

/// Найти первую страницу с непустым html_path и заданным паттерном в html_path.
fn find_with_html_pattern<'a>(pages: &'a [Page], pattern: &str) -> Option<&'a Page> {
    for p in pages {
        if !p.html_path.is_empty() && p.html_path.contains(pattern) {
            return Some(p);
        }
        if let Some(found) = find_with_html_pattern(&p.children, pattern) {
            return Some(found);
        }
    }
    None
}

/// Найти первую страницу типа (объект) с дочерней страницей-каталогом
/// «Конструкторы» (en) и хотя бы одним конструктором в нём.
fn find_type_with_constructor(pages: &[Page]) -> Option<(String, String)> {
    fn walk(page: &Page) -> Option<(String, String)> {
        // У типа есть дочерняя глава "Конструкторы" — значит у типа есть
        // непустой constructors-каталог.
        for child in &page.children {
            if child.title.en == "Конструкторы" || child.title.ru == "Конструкторы"
            {
                // Берём первого конструктора из /ctors/
                for ctor in &child.children {
                    if ctor.html_path.contains("/ctors/") && !ctor.html_path.is_empty() {
                        return Some((page.html_path.clone(), ctor.html_path.clone()));
                    }
                }
            }
        }
        for c in &page.children {
            if let Some(found) = walk(c) {
                return Some(found);
            }
        }
        None
    }
    for root in pages {
        if let Some(found) = walk(root) {
            return Some(found);
        }
    }
    None
}

#[test]
fn parse_real_method() {
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    // Берём конкретный известный метод. У `/methods/` в htmlPath встречаются
    // и страницы-каталоги (без блока «Синтаксис:») — на них наши assert'ы
    // упадут. Конкретное имя — надёжнее.
    let html_path =
        find_in_toc(&content.toc.pages, "СтрНайти").expect("в TOC должен быть метод СтрНайти");

    let html = content.get_entry_text(&html_path).expect("read");
    let info = parse_method_page(&html);

    println!(
        "Method: ru={:?} en={:?} signatures={} sig_names={:?} return={:?}",
        info.name_ru,
        info.name_en,
        info.signatures.len(),
        info.signatures
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        info.return_value.as_ref().map(|v| v.type_name.as_str())
    );
    assert_eq!(info.name_ru, "СтрНайти");
    assert!(
        !info.signatures.is_empty(),
        "должна быть хотя бы одна сигнатура"
    );
    // У СтрНайти есть параметры, проверяем что блок «Синтаксис:» прочитан.
    assert!(
        !info.signatures[0].syntax.is_empty(),
        "syntax должен быть непустым"
    );
}

#[test]
fn parse_real_property() {
    let Some(path) = hbk_path() else {
        eprintln!("skip");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");
    let html_path = find_with_html_pattern(&content.toc.pages, "/properties/")
        .expect("должно быть хотя бы одно свойство")
        .html_path
        .clone();

    let html = content.get_entry_text(&html_path).expect("read");
    let info = parse_property_page(&html);

    println!(
        "Property: ru={:?} en={:?} type={:?} readonly={}",
        info.name_ru, info.name_en, info.type_name, info.readonly
    );
    assert!(!info.name_ru.is_empty());
}

#[test]
fn parse_real_object() {
    let Some(path) = hbk_path() else {
        eprintln!("skip");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    // Попробуем популярный тип — ТаблицаЗначений (часто упоминается в карточках).
    let html_path = find_in_toc(&content.toc.pages, "ТаблицаЗначений")
        .or_else(|| {
            find_with_html_pattern(&content.toc.pages, "/objects/").map(|p| p.html_path.clone())
        })
        .expect("должна быть найдена страница объекта");

    let html = content.get_entry_text(&html_path).expect("read");
    let info = parse_object_page(&html);

    println!(
        "Object: ru={:?} en={:?} desc_len={}",
        info.name_ru,
        info.name_en,
        info.description.len()
    );
    assert!(!info.name_ru.is_empty());
}

fn find_in_toc(pages: &[Page], target_ru: &str) -> Option<String> {
    for p in pages {
        if p.title.ru == target_ru && !p.html_path.is_empty() {
            return Some(p.html_path.clone());
        }
        if let Some(found) = find_in_toc(&p.children, target_ru) {
            return Some(found);
        }
    }
    None
}

/// Страница по фрагменту html_path; запасной путь — русское имя из TOC.
///
/// html_path нумерованный (`/ValueTable/methods/Find602.html`) и меняется от
/// версии к версии, поэтому по имени ищем, только если фрагмент не совпал.
fn find_page(pages: &[Page], html_pattern: &str, title_ru: &str) -> Option<String> {
    find_with_html_pattern(pages, html_pattern)
        .map(|page| page.html_path.clone())
        .or_else(|| find_in_toc(pages, title_ru))
}

/// Html страницы по фрагменту html_path (с запасным поиском по имени в TOC).
fn page_html(content: &mut HbkContent, html_pattern: &str, title_ru: &str) -> String {
    let path = find_page(&content.toc.pages, html_pattern, title_ru)
        .unwrap_or_else(|| panic!("страница '{title_ru}' не найдена в TOC"));
    content.get_entry_text(&path).expect("чтение страницы")
}

/// Есть ли метка в списке доступности. Справка пишет первую метку с заглавной,
/// остальные — строчными, поэтому сверка без учёта регистра.
fn has_label(info: &AvailabilityInfo, label: &str) -> bool {
    info.labels.iter().any(|item| item.to_lowercase() == label)
}

#[test]
fn parse_real_constructor() {
    let Some(path) = hbk_path() else {
        eprintln!("skip");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    let (_type_html, ctor_html) = find_type_with_constructor(&content.toc.pages)
        .expect("должен быть хотя бы один тип с конструктором в TOC");

    let html = content.get_entry_text(&ctor_html).expect("read");
    let info = parse_constructor_page(&html);

    println!(
        "Constructor: name={:?} syntax={:?}... params={}",
        info.name,
        info.syntax.chars().take(60).collect::<String>(),
        info.parameters.len()
    );
    assert!(!info.name.is_empty(), "у конструктора должно быть имя");
}

// ── Глава «Доступность:» на реальном hbk (TZ, §7.1) ─────────────────────────

#[test]
fn availability_of_global_method_lists_all_contexts() {
    // `СтрНайти` доступен везде: 10 меток, включая сервер и тонкий клиент.
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    let html = page_html(&mut content, "StrFind", "СтрНайти");
    let info = parse_method_page(&html);
    let availability = info
        .availability
        .expect("у СтрНайти должна быть глава «Доступность:»");

    println!("СтрНайти: {:?}", availability);
    assert_eq!(availability.labels.len(), 10, "{availability:?}");
    assert!(has_label(&availability, "сервер"), "{availability:?}");
    assert!(
        has_label(&availability, "тонкий клиент"),
        "{availability:?}"
    );
    assert!(
        has_label(&availability, "внешнее соединение"),
        "{availability:?}"
    );
    assert!(
        !availability.has_unknown,
        "на 8.3.27 все метки обязаны быть известны: {availability:?}"
    );
    assert!(availability.note.is_none(), "{:?}", availability.note);
}

#[test]
fn client_only_global_method_has_no_server() {
    // `ПоказатьВопрос` — клиентский метод: сервера в списке нет.
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    let html = page_html(&mut content, "ShowQueryBox", "ПоказатьВопрос");
    let info = parse_method_page(&html);
    let availability = info
        .availability
        .expect("у ПоказатьВопрос должна быть глава «Доступность:»");

    println!("ПоказатьВопрос: {:?}", availability);
    assert!(!has_label(&availability, "сервер"), "{availability:?}");
    assert!(
        !has_label(&availability, "внешнее соединение"),
        "{availability:?}"
    );
    assert!(
        has_label(&availability, "тонкий клиент"),
        "{availability:?}"
    );
    assert!(
        has_label(&availability, "толстый клиент"),
        "{availability:?}"
    );
    assert!(
        has_label(&availability, "мобильное приложение (клиент)"),
        "{availability:?}"
    );
    assert!(!availability.has_unknown, "{availability:?}");
}

#[test]
fn server_only_method_and_property_of_value_table() {
    // `ТаблицаЗначений.Найти` и `.Колонки` — серверный список: клиентских меток нет.
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    let method_html = page_html(&mut content, "/ValueTable/methods/Find", "Найти");
    let method = parse_method_page(&method_html);
    let method_availability = method
        .availability
        .expect("у метода ТаблицаЗначений.Найти должна быть глава «Доступность:»");

    let property_html = page_html(&mut content, "/ValueTable/properties/Columns", "Колонки");
    let property = parse_property_page(&property_html);
    let property_availability = property
        .availability
        .expect("у свойства ТаблицаЗначений.Колонки должна быть глава «Доступность:»");

    println!("Найти: {method_availability:?}");
    println!("Колонки: {property_availability:?}");
    for availability in [&method_availability, &property_availability] {
        assert!(has_label(availability, "сервер"), "{availability:?}");
        assert!(
            has_label(availability, "внешнее соединение"),
            "{availability:?}"
        );
        assert!(
            has_label(availability, "мобильный автономный сервер"),
            "{availability:?}"
        );
        assert!(
            !has_label(availability, "тонкий клиент"),
            "{availability:?}"
        );
        assert!(!has_label(availability, "веб-клиент"), "{availability:?}");
        assert!(
            !has_label(availability, "мобильный клиент"),
            "{availability:?}"
        );
        assert!(!availability.has_unknown, "{availability:?}");
    }
}

#[test]
fn server_only_type_availability_is_the_source_for_new() {
    // `Новый Запрос` берёт доступность у ТИПА: страница типа серверная.
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    let html = page_html(&mut content, "/Query.html", "Запрос");
    let info = parse_object_page(&html);
    let availability = info
        .availability
        .expect("у типа Запрос должна быть глава «Доступность:»");

    println!("Запрос: {:?}", availability);
    assert!(has_label(&availability, "сервер"), "{availability:?}");
    assert!(
        has_label(&availability, "внешнее соединение"),
        "{availability:?}"
    );
    assert!(
        !has_label(&availability, "тонкий клиент"),
        "{availability:?}"
    );
    assert!(!availability.has_unknown, "{availability:?}");
}

#[test]
fn constructor_page_has_no_availability_chapter() {
    // Своей главы «Доступность:» у страницы конструктора нет — фиксируем
    // отсутствие данных: источник доступности конструктора — страница типа.
    let Some(path) = hbk_path() else {
        eprintln!("skip: BSL_CONTEXT_PLATFORM_PATH не задан");
        return;
    };
    let mut content = HbkContent::read(&path).expect("hbk open");

    let (_type_path, ctor_path) = find_type_with_constructor(&content.toc.pages)
        .expect("должен быть хотя бы один тип с конструктором в TOC");
    let ctor_html = content.get_entry_text(&ctor_path).expect("read");
    let ctor = parse_constructor_page(&ctor_html);
    println!("Конструктор: {:?}", ctor.name);
    // Именно ГЛАВА, а не слово в тексте: у конструкторов
    // `WindowsClientCertificate`/`MacOSClientCertificate` «Доступность:» стоит
    // прямо в описании (`<br>Доступность: Толстый клиент, тонкий клиент.`), и
    // проверка по подстроке там дала бы ложное срабатывание.
    assert!(
        !ctor_html.contains("V8SH_chapter\">Доступность:"),
        "у страницы конструктора не должно быть главы «Доступность:»"
    );
}
