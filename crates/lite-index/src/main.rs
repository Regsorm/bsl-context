//! CLI облегчённого индекса методов конфигурации 1С.
//!
//! ```text
//! bsl-lite-index build --root <каталог выгрузки> --db <файл.db> [--jobs N]
//! bsl-lite-index stats --db <файл.db>
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ошибка: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .context("укажите команду: build | stats")?;

    match command.as_str() {
        "build" => run_build(args),
        "stats" => run_stats(args),
        other => bail!("неизвестная команда '{other}', ожидалась build | stats"),
    }
}

fn run_build(args: impl Iterator<Item = String>) -> Result<()> {
    let mut root: Option<PathBuf> = None;
    let mut db: Option<PathBuf> = None;
    let mut jobs: usize = 0;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    args.next().context("--root требует значение")?,
                ))
            }
            "--db" => {
                db = Some(PathBuf::from(
                    args.next().context("--db требует значение")?,
                ))
            }
            "--jobs" => {
                jobs = args
                    .next()
                    .context("--jobs требует значение")?
                    .parse()
                    .context("--jobs: ожидалось целое число")?
            }
            other => bail!("неизвестный флаг '{other}'"),
        }
    }

    let root = root.context("укажите --root <каталог выгрузки>")?;
    let db = db.context("укажите --db <файл.db>")?;

    let stats = lite_index::build(&root, &db, jobs)?;
    println!("модулей: {}", stats.modules);
    println!("методов: {}", stats.methods);
    println!("глобальных модулей: {}", stats.global_modules);
    println!("время: {} мс", stats.elapsed_ms);

    Ok(())
}

fn run_stats(args: impl Iterator<Item = String>) -> Result<()> {
    let mut db: Option<PathBuf> = None;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => {
                db = Some(PathBuf::from(
                    args.next().context("--db требует значение")?,
                ))
            }
            other => bail!("неизвестный флаг '{other}'"),
        }
    }
    let db = db.context("укажите --db <файл.db>")?;

    let conn = rusqlite::Connection::open(&db)
        .with_context(|| format!("не удалось открыть индекс {}", db.display()))?;

    let mut stmt = conn.prepare("SELECT key, value FROM meta ORDER BY key")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (key, value) = row?;
        println!("{key}: {value}");
    }

    let modules_count: i64 = conn.query_row("SELECT COUNT(*) FROM modules", [], |r| r.get(0))?;
    let methods_count: i64 = conn.query_row("SELECT COUNT(*) FROM methods", [], |r| r.get(0))?;
    println!("modules(count): {modules_count}");
    println!("methods(count): {methods_count}");

    Ok(())
}
