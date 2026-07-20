//! Приёмка извлечения текстов запроса на реальной конфигурации.
//!
//! Модульные тесты проверяют разбор на образцах, которые я же и придумал.
//! Настоящие модули УТ содержат то, чего в образцах не бывает: запросы,
//! собранные в цикле, комментарии посреди текста, `#Удаление` внутри литерала,
//! двоичные модули поставщика. Замер на `config_objects` показал, чем это
//! кончается: 15 зелёных модульных тестов при 40398 ложных находках на корпусе.
//!
//! Тест ничего не утверждает про «правильную» долю извлечения — он ловит паники
//! и печатает статистику, по которой видно, не просел ли разбор после правок.
//!
//! ```pwsh
//! cargo test -p bsl-parse --test corpus_query_texts --release -- --ignored --nocapture
//! ```

use std::fs;
use std::path::{Path, PathBuf};

const CORPUS: &str = r"C:\RepoUT-test";

/// Рекурсивный обход без walkdir: у крейта зависимостей нет, и заводить их
/// ради одного теста незачем.
fn collect_bsl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_bsl(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("bsl") {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "требует выгрузку УТ в C:\\RepoUT-test"]
fn query_texts_on_real_ut_corpus() {
    let root = Path::new(CORPUS);
    assert!(root.is_dir(), "корпус не найден: {CORPUS}");

    let mut files = Vec::new();
    collect_bsl(root, &mut files);
    assert!(!files.is_empty(), "в корпусе нет .bsl");

    let mut modules_read = 0usize;
    let mut modules_with_queries = 0usize;
    let mut queries = 0usize;
    let mut spans = 0usize;
    let mut multi_part = 0usize;
    let mut mapping_errors = 0usize;
    // Литералы, где ключевое слово стоит вплотную за кавычкой. Это НЕ оценка
    // сверху: запрос часто начинается с переноса строки (`"` + перевод +
    // `|ВЫБРАТЬ`), такие сюда не попадают — потому извлечено больше, чем
    // насчитано здесь. Строка полезна только как маячок: резкое расхождение
    // между замерами означает, что разбор границ литерала поехал.
    let mut literal_starts = 0usize;

    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue; // двоичные модули поставщика и битая кодировка
        };
        modules_read += 1;

        for keyword in ["\"ВЫБРАТЬ", "\"SELECT", "\"УНИЧТОЖИТЬ"] {
            literal_starts += text.matches(keyword).count();
        }

        let found = bsl_parse::collect_query_texts(&text);
        if !found.is_empty() {
            modules_with_queries += 1;
        }
        queries += found.len();

        for query in &found {
            spans += query.spans.len();
            if query.spans.len() > 1 {
                multi_part += 1;
            }
            // Карта смещений обязана попадать в границы модуля и в границы
            // символов: иначе находка либо потеряет позицию, либо уронит
            // валидатор при нарезке строки.
            //
            // Пробы берутся по границам символов собранного текста, а не по
            // произвольным байтам: текст запроса — кириллица, и `len/2` сам по
            // себе обычно указывает в середину двухбайтного символа. Спрашивать
            // карту о смещении, которого не существует, бессмысленно.
            let bounds: Vec<usize> = query.text.char_indices().map(|(i, _)| i).collect();
            let probes = [
                bounds.first().copied(),
                bounds.get(bounds.len() / 2).copied(),
                bounds.last().copied(),
            ];
            for probe in probes.into_iter().flatten() {
                let byte = query.map_offset(probe);
                if byte >= text.len() || !text.is_char_boundary(byte) {
                    mapping_errors += 1;
                }
            }
        }
    }

    println!("── Тексты запросов на корпусе УТ ──");
    println!("файлов .bsl:                {}", files.len());
    println!("прочитано как текст:        {modules_read}");
    println!("модулей с запросами:        {modules_with_queries}");
    println!("извлечено запросов:         {queries}");
    println!("ключевое слово вплотную за кавычкой: {literal_starts} (маячок, не оценка)");
    println!("склеенных из нескольких кусков: {multi_part}");
    println!("всего кусков в картах:      {spans}");
    println!("сбоев карты смещений:       {mapping_errors}");

    assert_eq!(mapping_errors, 0, "карта смещений указывает мимо модуля");
    assert!(queries > 0, "на корпусе УТ не извлечено ни одного запроса");
}
