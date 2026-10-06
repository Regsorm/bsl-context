//! Крафтовые атаки на hbk-контейнер — то, что в PR подтверждалось только его
//! же юнит-тестами, проверяется на собранных вручную файлах:
//!
//! * зип-бомба в `FileStorage` (маленький файл, разворачивается в 64 МиБ);
//! * зип-бомба в `PackBlock` (TOC сверх лимита);
//! * заявленный размер zip-entry, не совпадающий с фактическим (в обе стороны);
//! * крафтовая глубина TOC (переполнение стека на рекурсивном разборе/Drop);
//! * одинокий суррогат в имени entity (lossy UTF-16, без отказа контейнера);
//! * обрыв реального hbk (файл валидный, но усечённый);
//! * TOCTOU: нет предварительного `exists()`, ошибка open классифицируется,
//!   гонка «удалили/создали файл во время чтения» не роняет процесс.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hbk_reader::{HbkContainer, HbkContent, HbkError};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

// ── сборка контейнера по формату `container.rs` ──────────────────────────────

/// long-string: 8 ASCII hex + байт-разделитель.
fn long_string(v: usize) -> Vec<u8> {
    let mut out = format!("{v:08X}").into_bytes();
    out.push(b' ');
    assert_eq!(out.len(), 9);
    out
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

/// Контейнер с entity, у которых имя задано сырыми байтами UTF-16LE.
fn container_raw(entities: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let head = 18 + 9 + 9 + 11;
    let mut offset = head + entities.len() * 12;
    let mut layout = Vec::new();
    for (name, body) in entities {
        let header_addr = offset;
        offset += 2 + 9 + 40 + name.len();
        let body_addr = offset;
        offset += 2 + 9 + 20 + body.len();
        layout.push((header_addr, body_addr));
    }

    let mut buf = vec![0u8; 18];
    buf.extend(long_string(entities.len() * 12));
    buf.extend(long_string(entities.len() * 12));
    buf.extend([0u8; 11]);
    for (header_addr, body_addr) in &layout {
        buf.extend((*header_addr as i32).to_le_bytes());
        buf.extend((*body_addr as i32).to_le_bytes());
        buf.extend(i32::MAX.to_le_bytes());
    }
    for ((name, body), (_, body_addr)) in entities.iter().zip(layout.iter()) {
        buf.extend([0u8; 2]);
        buf.extend(long_string(24 + name.len()));
        buf.extend([0u8; 40]);
        buf.extend(name);
        assert_eq!(buf.len(), *body_addr, "смещение тела разошлось с расчётом");
        buf.extend([0u8; 2]);
        buf.extend(long_string(body.len()));
        buf.extend([0u8; 20]);
        buf.extend(body);
    }
    buf
}

fn container(entities: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let owned: Vec<(Vec<u8>, Vec<u8>)> = entities
        .iter()
        .map(|(name, body)| (utf16le(name), body.clone()))
        .collect();
    container_raw(&owned)
}

/// Настоящий zip (у `FileStorage` обязана быть central directory).
fn zip_of(entries: &[(&str, Vec<u8>, CompressionMethod)]) -> Vec<u8> {
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data, method) in entries {
        let opts = SimpleFileOptions::default()
            .compression_method(*method)
            .unix_permissions(0o644);
        w.start_file(*name, opts).expect("start_file");
        w.write_all(data).expect("запись данных entry");
    }
    w.finish().expect("finish zip").into_inner()
}

fn toc_text(html_path: &str) -> String {
    format!(
        "{{\n  1\n  {{ 1 0 0 {{ 0 0 {{ 0 0 {{\"ru\" \"Страница\"}} }} \"{html_path}\" }} }}\n}}"
    )
}

/// Подменить заявленный несжатый размер во всех заголовках zip на `value`.
fn patch_declared_size(zip: &mut [u8], value: u32) {
    let mut patched = 0;
    for i in 0..zip.len().saturating_sub(30) {
        if zip[i..i + 4] == *b"PK\x03\x04" {
            zip[i + 22..i + 26].copy_from_slice(&value.to_le_bytes());
            patched += 1;
        } else if zip[i..i + 4] == *b"PK\x01\x02" {
            zip[i + 24..i + 28].copy_from_slice(&value.to_le_bytes());
            patched += 1;
        }
    }
    assert!(patched >= 2, "заголовки zip не найдены, подменять нечего");
}

fn write_hbk(dir: &Path, bytes: &[u8]) -> PathBuf {
    let path = dir.join("shcntx_ru.hbk");
    std::fs::write(&path, bytes).expect("запись hbk");
    path
}

fn hbk_with(storage: Vec<u8>, pack: Vec<u8>) -> Vec<u8> {
    container(&[("PackBlock", pack), ("FileStorage", storage)])
}

// ── зип-бомбы ────────────────────────────────────────────────────────────────

/// Страница, разворачивающаяся в 64 МиБ (лимит — 32 МиБ), обязана получить
/// внятный отказ, а не съесть память.
#[test]
fn zip_bomb_page_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = zip_of(&[(
        "page.html",
        vec![0u8; 64 * 1024 * 1024],
        CompressionMethod::Deflated,
    )]);
    let pack = zip_of(&[(
        "toc",
        toc_text("page.html").into_bytes(),
        CompressionMethod::Deflated,
    )]);
    let hbk = hbk_with(storage, pack);
    assert!(
        hbk.len() < 1024 * 1024,
        "бомба обязана быть крошечной на диске, получено {} байт",
        hbk.len()
    );
    let path = write_hbk(dir.path(), &hbk);

    let started = Instant::now();
    let mut content = HbkContent::read(&path).expect("контейнер и TOC читаются");
    let err = content
        .get_entry("/page.html")
        .expect_err("страница сверх лимита обязана быть отказом");
    assert!(matches!(err, HbkError::PageTooLarge { .. }), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "отказ занял {:?}",
        started.elapsed()
    );
}

/// TOC, разворачивающийся за 64 МиБ, — тоже отказ, а не гигабайты в куче.
#[test]
fn zip_bomb_toc_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = vec![b'A'; 65 * 1024 * 1024];
    let pack = zip_of(&[("toc", big, CompressionMethod::Deflated)]);
    let storage = zip_of(&[(
        "page.html",
        b"<html><body>x</body></html>".to_vec(),
        CompressionMethod::Deflated,
    )]);
    let hbk = hbk_with(storage, pack);
    assert!(hbk.len() < 1024 * 1024, "{} байт", hbk.len());
    let path = write_hbk(dir.path(), &hbk);

    let err = match HbkContent::read(&path) {
        Ok(_) => panic!("TOC сверх лимита обязан быть отказом"),
        Err(e) => e,
    };
    assert!(matches!(err, HbkError::Inflate(_)), "{err}");
}

// ── врущий заявленный размер ─────────────────────────────────────────────────

/// Заявлено мало (100 байт), фактически 64 МиБ: читатель zip режет по
/// заявленному размеру, поэтому бомбы не выходит — проверяем, что нет ни
/// паники, ни гигантского буфера.
#[test]
fn lying_small_declared_size_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut storage = zip_of(&[(
        "page.html",
        vec![0u8; 64 * 1024 * 1024],
        CompressionMethod::Deflated,
    )]);
    patch_declared_size(&mut storage, 100);
    let pack = zip_of(&[(
        "toc",
        toc_text("page.html").into_bytes(),
        CompressionMethod::Deflated,
    )]);
    let path = write_hbk(dir.path(), &hbk_with(storage, pack));

    let mut content = HbkContent::read(&path).expect("контейнер читается");
    match content.get_entry("/page.html") {
        Err(HbkError::PageTooLarge { .. }) => {}
        Ok(buf) => assert!(
            buf.len() <= 1024,
            "буфер обязан быть ограничен заявленным размером, получено {}",
            buf.len()
        ),
        Err(e) => panic!("неожиданная ошибка: {e}"),
    }
}

/// Заявлено 2 ГиБ, фактически килобайт: ёмкость буфера ограничена лимитом
/// (32 МиБ), а данные не теряются.
#[test]
fn lying_huge_declared_size_keeps_data() {
    let dir = tempfile::tempdir().expect("tempdir");
    let page = b"<html><body><p class=\"V8SH_title\">X</p></body></html>".to_vec();
    let mut storage = zip_of(&[("page.html", page.clone(), CompressionMethod::Deflated)]);
    patch_declared_size(&mut storage, 0x7FFF_FFFF);
    let pack = zip_of(&[(
        "toc",
        toc_text("page.html").into_bytes(),
        CompressionMethod::Deflated,
    )]);
    let path = write_hbk(dir.path(), &hbk_with(storage, pack));

    let mut content = HbkContent::read(&path).expect("контейнер читается");
    let got = content
        .get_entry("/page.html")
        .expect("страница в пределах лимита читается");
    assert_eq!(got, page, "данные страницы не должны пострадать");
}

// ── глубина TOC ──────────────────────────────────────────────────────────────

/// Крафтовая цепочка `parentId` глубиной 50 000: парсер обязан вернуть ошибку,
/// а не переполнить стек (рекурсивный разбор и рекурсивный `Drop` дерева).
#[test]
fn crafted_deep_toc_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let n = 50_000usize;
    let mut toc = String::from("{\n");
    toc.push_str(&format!("{n}\n"));
    for id in 1..=n {
        let parent = id - 1;
        toc.push_str(&format!(
            "{{ {id} {parent} 0 {{ 0 0 {{ 0 0 {{\"ru\" \"P{id}\"}} }} \"p{id}.html\" }} }}\n"
        ));
    }
    toc.push('}');

    let pack = zip_of(&[("toc", toc.into_bytes(), CompressionMethod::Deflated)]);
    let storage = zip_of(&[(
        "p1.html",
        b"<html><body>x</body></html>".to_vec(),
        CompressionMethod::Deflated,
    )]);
    let path = write_hbk(dir.path(), &hbk_with(storage, pack));

    let err = match HbkContent::read(&path) {
        Ok(_) => panic!("глубина выше лимита обязана быть отказом"),
        Err(e) => e,
    };
    assert!(matches!(err, HbkError::TocParse(_)), "{err}");
}

// ── битый UTF-16 в имени ─────────────────────────────────────────────────────

/// Одинокий старший суррогат в имени entity: имя декодируется lossy
/// (U+FFFD), контейнер не отказывает целиком.
#[test]
fn unpaired_surrogate_in_name_is_lossy_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut name = vec![0x00, 0xD8]; // lone high surrogate D800
    name.extend(utf16le("A"));
    let path = write_hbk(dir.path(), &container_raw(&[(name, b"body".to_vec())]));

    let container = HbkContainer::read(&path).expect("контейнер читается");
    let body = container
        .get_entity("\u{FFFD}A")
        .expect("имя обязано декодироваться с заменой, а не теряться");
    assert_eq!(body, b"body");
}

// ── усечённый реальный hbk ───────────────────────────────────────────────────

/// Настоящий hbk, обрезанный до 1 МиБ: внятная ошибка, без паники и без
/// бесконечного чтения.
#[test]
fn truncated_real_hbk_is_an_error() {
    let platform = std::env::var("BSL_CONTEXT_PLATFORM_PATH")
        .unwrap_or_else(|_| r"C:\Program Files\1cv8\8.3.27.1786".to_string());
    let real = Path::new(&platform).join("bin").join("shcntx_ru.hbk");
    let Ok(bytes) = std::fs::read(&real) else {
        eprintln!("пропуск: нет {real:?}");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_hbk(dir.path(), &bytes[..1024 * 1024]);

    let err = match HbkContent::read(&path) {
        Ok(_) => panic!("усечённый hbk обязан быть ошибкой"),
        Err(e) => e,
    };
    assert!(!format!("{err}").is_empty());
}

// ── TOCTOU ───────────────────────────────────────────────────────────────────

#[test]
fn missing_file_is_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = match HbkContainer::read(&dir.path().join("нет-такого.hbk")) {
        Ok(_) => panic!("отсутствующий файл обязан быть ошибкой"),
        Err(e) => e,
    };
    assert!(matches!(err, HbkError::NotFound(_)), "{err}");
}

/// Ошибка классифицируется по результату `File::open`, а не по
/// предварительной проверке `exists()`: каталог существует, но открыть его
/// как hbk нельзя — это НЕ NotFound.
#[test]
fn directory_path_is_io_error_not_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    match HbkContainer::read(dir.path()) {
        Ok(_) => panic!("каталог не может быть hbk-контейнером"),
        Err(HbkError::NotFound(_)) => {
            panic!("каталог существует: NotFound означал бы пред-проверку")
        }
        Err(_) => {}
    }
}

/// Гонка «файл удаляют и создают заново, пока его читают»: результат всегда
/// либо валидный контейнер, либо классифицированная ошибка ввода-вывода
/// (на Windows открытие файла в момент пересоздания штатно даёт NotFound или
/// AccessDenied — sharing violation). Паники и зависаний быть не должно.
#[test]
fn race_delete_and_recreate_does_not_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_hbk(dir.path(), &container(&[("PackBlock", b"x".to_vec())]));
    let flipper = path.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    let handle = std::thread::spawn(move || {
        let bytes = std::fs::read(&flipper).expect("исходные байты");
        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = std::fs::remove_file(&flipper);
            let _ = std::fs::write(&flipper, &bytes);
        }
    });

    for _ in 0..200 {
        match HbkContainer::read(&path) {
            Ok(_) => {}
            Err(HbkError::NotFound(_)) | Err(HbkError::Io(_)) => {}
            Err(e) => panic!("недопустимая ошибка в гонке: {e}"),
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.join().expect("поток-перекидыватель");
}
