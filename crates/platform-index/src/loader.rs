//! Pipeline `HbkContent → PlatformIndex`.
//!
//! Один проход по TOC платформы:
//! 1. Найти `Global context` → собрать `global_methods` и `global_properties`.
//! 2. Найти каталоги перечислений → распарсить как типы с `enum_values`.
//! 3. Найти каталоги типов → распарсить с `methods/properties/constructors`.
//!
//! Все типы (обычные и перечисления) складываются в одну `HashMap` по `name_ru`.
//!
//! Разбор страниц перечислений и типов распараллелен (rayon): это основная
//! цена сборки — на hbk 8.3.20 замер дал ~3,4–5,1 с на одном потоке и
//! ~1,1–1,2 с на двенадцати. Чтение страницы из zip требует `&mut HbkContent`,
//! поэтому контент живёт под `Mutex`, а блокировка берётся на время ОДНОЙ
//! страницы и снимается до разбора html (см. [`LockedSource`]) — иначе потоки
//! выстроились бы в очередь на весь проход и распараллеливание не дало бы
//! ничего. Ко входу в индекс результаты приходят в порядке TOC: вставка
//! последовательная, чтобы содержимое вторичных карт (`types_en`) не зависело
//! от планировщика.

use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use hbk_parser::{EnumInfo, ObjectInfo};
use hbk_reader::HbkContent;
use rayon::prelude::*;
use tracing::{info, warn};

use crate::mapper::{method_from, property_from, type_from_enum, type_from_object};
use crate::storage::PlatformIndex;
use crate::visitor::{
    collect_global_methods, collect_global_properties, collect_root_pages, drill_down,
    visit_enum_page, visit_type_page, HtmlSource,
};

/// `HbkContent` за блокировкой: параллельный разбор берёт её на чтение одной
/// страницы. Всё остальное время потоки заняты разбором html и не мешают друг
/// другу.
struct LockedSource<'a, 'b>(&'a Mutex<&'b mut HbkContent>);

impl HtmlSource for LockedSource<'_, '_> {
    fn read_html(&mut self, html_path: &str) -> Option<String> {
        // Отравленную блокировку игнорируем: причину паники потока уже видели
        // выше, а отказ здесь лишь замаскировал бы её исходной ошибкой.
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.read_html(html_path)
    }
}

/// Загрузить `PlatformIndex` из hbk-файла платформы.
///
/// Принимает путь к `shcntx_ru.hbk`. Не разделяет файлы — всё в один проход.
pub fn load_from_hbk(path: &Path) -> Result<PlatformIndex> {
    info!(?path, "загрузка платформенного контекста из hbk");
    let t = Instant::now();
    let mut content = HbkContent::read(path)
        .map_err(|e| anyhow!("не удалось открыть hbk {}: {}", path.display(), e))?;
    info!(
        elapsed_ms = t.elapsed().as_millis() as u64,
        "[этап 1/4] контейнер и TOC hbk"
    );
    build_index(&mut content).with_context(|| format!("сборка PlatformIndex из {}", path.display()))
}

/// Та же логика, но для уже открытого `HbkContent` (удобно в тестах).
pub fn build_index(content: &mut HbkContent) -> Result<PlatformIndex> {
    let mut index = PlatformIndex::new();

    // Снимаем заимствование TOC clone'ом дерева — нам нужно одновременно
    // итерировать по страницам и читать их html через `&mut HbkContent`.
    let pages = content.toc.pages.clone();
    let roots = collect_root_pages(&pages);

    if let Some(global) = roots.global_context {
        let t = Instant::now();
        index.global_methods = collect_global_methods(content, global)
            .iter()
            .map(method_from)
            .collect();
        index.global_properties = collect_global_properties(content, global)
            .iter()
            .map(property_from)
            .collect();
        // Этапы пишутся в info один раз на сборку (старт или reboot кэша):
        // по ним скрипт замера и журнал сервиса видят, где прошло время.
        info!(
            methods = index.global_methods.len(),
            properties = index.global_properties.len(),
            elapsed_ms = t.elapsed().as_millis() as u64,
            "[этап 2/4] глобальный контекст"
        );
    } else {
        warn!("раздел 'Global context' не найден в TOC — global_methods/global_properties пусты");
    }

    // Дальше к контенту ходят несколько потоков. Единственная мутация внутри —
    // чтение zip-entry, поэтому блокировки на одну страницу достаточно.
    let content = Mutex::new(&mut *content);

    // Перечисления (типы с enum_values).
    let t = Instant::now();
    let mut enum_pages = Vec::new();
    for root in &roots.enums {
        drill_down(root, &mut enum_pages);
    }
    dedup_pages(&mut enum_pages);
    // `collect` у rayon сохраняет порядок исходной последовательности, поэтому
    // вставка ниже идёт в порядке TOC независимо от планировщика.
    let enum_infos: Vec<Option<EnumInfo>> = enum_pages
        .par_iter()
        .map(|page| visit_enum_page(&mut LockedSource(&content), page))
        .collect();
    for info in enum_infos.into_iter().flatten() {
        index.insert_type(type_from_enum(&info));
    }
    info!(
        pages = enum_pages.len(),
        types = index.types.len(),
        elapsed_ms = t.elapsed().as_millis() as u64,
        "[этап 3/4] перечисления"
    );

    // Обычные типы.
    let t = Instant::now();
    let mut type_pages = Vec::new();
    for root in &roots.types {
        drill_down(root, &mut type_pages);
    }
    dedup_pages(&mut type_pages);
    // «Таблицы запросов» (`/tables/...`) — справочник ПОЛЕЙ таблиц языка
    // запросов («<Имя ресурса>», «ВедущаяЗадача», …), а не типы платформы:
    // в индекс такие страницы не берём, иначе search/get_member наполняются
    // мусорными «типами» без членов.
    type_pages.retain(|p| !p.html_path.contains("/tables/"));
    let type_infos: Vec<Option<ObjectInfo>> = type_pages
        .par_iter()
        .map(|page| visit_type_page(&mut LockedSource(&content), page))
        .collect();
    for info in type_infos.into_iter().flatten() {
        index.insert_type(type_from_object(&info));
    }
    info!(
        pages = type_pages.len(),
        types = index.types.len(),
        elapsed_ms = t.elapsed().as_millis() as u64,
        "[этап 4/4] типы"
    );

    info!(
        global_methods = index.global_methods.len(),
        global_properties = index.global_properties.len(),
        types = index.types.len(),
        enum_types = index.enum_types_count(),
        "PlatformIndex собран"
    );
    Ok(index)
}

/// Убрать повторяющиеся `html_path` (TOC иногда ссылается на страницу дважды),
/// сохранив порядок первого появления: иначе страница читается и парсится
/// повторно.
fn dedup_pages(pages: &mut Vec<&hbk_reader::Page>) {
    let mut seen = std::collections::HashSet::new();
    pages.retain(|p| seen.insert(p.html_path.as_str()));
}
