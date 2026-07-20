//! Приёмка парсера на реальных запросах конфигурации.
//!
//! Тесты подмножества написаны по моим же представлениям о том, как выглядит
//! запрос. Настоящая УТ пишет иначе: соединения в пять этажей, вложенные
//! подзапросы, `ИТОГИ`, конструкции, которых в подмножестве нет вовсе.
//!
//! Тест не требует стопроцентного разбора — полной грамматики у нас нет и не
//! будет. Он показывает долю и, главное, распределение причин отказа: по нему
//! видно, какая одна недостающая конструкция закрывает тысячи запросов.
//!
//! ```pwsh
//! cargo test -p sdbl-parse --test corpus_parse --release -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const CORPUS: &str = r"C:\RepoUT-test";

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

/// Вырезать окрестность места отказа, пометив само место символом `⟨⟩`.
fn window_at(text: &str, offset: usize) -> String {
    let start = text[..offset.min(text.len())]
        .char_indices()
        .rev()
        .nth(70)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[offset.min(text.len())..]
        .char_indices()
        .nth(70)
        .map(|(i, _)| offset + i)
        .unwrap_or(text.len());
    let head = &text[start..offset.min(text.len())];
    let tail = &text[offset.min(text.len())..end];
    format!("{head}⟨HERE⟩{tail}").replace('\n', " ")
}

#[test]
#[ignore = "требует выгрузку УТ в C:\\RepoUT-test"]
fn parses_real_ut_queries() {
    let root = Path::new(CORPUS);
    assert!(root.is_dir(), "корпус не найден: {CORPUS}");

    let mut files = Vec::new();
    collect_bsl(root, &mut files);

    let mut total = 0usize;
    let mut parsed = 0usize;
    let mut with_joins = 0usize;
    let mut with_temp_tables = 0usize;
    let mut with_virtual_tables = 0usize;
    let mut reasons: HashMap<String, usize> = HashMap::new();
    // Примеры отказов — по одному на причину, чтобы было что разбирать.
    let mut samples: HashMap<String, String> = HashMap::new();

    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };

        for query in bsl_parse::collect_query_texts(&text) {
            total += 1;
            match sdbl_parse::parse(&query.text) {
                Ok(package) => {
                    parsed += 1;
                    for q in &package.queries {
                        if !q.joins.is_empty() {
                            with_joins += 1;
                        }
                        if q.into.is_some() {
                            with_temp_tables += 1;
                        }
                        for source in q.all_sources() {
                            if let sdbl_parse::Table::Meta(meta) = &source.table {
                                if meta
                                    .sub_table
                                    .as_deref()
                                    .is_some_and(sdbl_parse::is_virtual_table)
                                {
                                    with_virtual_tables += 1;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    *reasons.entry(err.message.clone()).or_default() += 1;
                    // Окно вокруг места отказа, а не начало запроса: причина
                    // всегда там, где парсер споткнулся, и по первым строкам
                    // выборки о ней не сказать ничего.
                    samples
                        .entry(err.message.clone())
                        .or_insert_with(|| window_at(&query.text, err.offset));
                }
            }
        }
    }

    let share = if total > 0 {
        parsed as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    println!("── Разбор запросов УТ ──");
    println!("всего запросов:        {total}");
    println!("разобрано:             {parsed} ({share:.1}%)");
    println!("с соединениями:        {with_joins}");
    println!("с временными таблицами: {with_temp_tables}");
    println!("с виртуальными таблицами: {with_virtual_tables}");
    println!("\n── Причины отказа ──");

    let mut sorted: Vec<_> = reasons.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (message, count) in sorted.iter().take(15) {
        println!("{count:>6}  {message}");
        if let Some(sample) = samples.get(*message) {
            println!("        пример: {sample}");
        }
    }

    assert!(total > 0, "из корпуса не извлечено ни одного запроса");
}
