//! Единый слой разбора BSL, которым пользуются и валидатор (`bsl-validator`),
//! и внешний индексатор кода: нормализация исходника под дефекты грамматики
//! `tree-sitter-bsl`, разбор дерева и сбор фактов (`collect_facts`), а также
//! текстовая маскировка строк/комментариев и объявлений процедур/функций.
//! Здесь нет ничего про платформенный контекст 1С — крейт не зависит от
//! `platform-index`.

use std::collections::HashSet;

/// Голый вызов `Имя(...)` — не метод объекта.
pub struct CallFact {
    pub name: String,
    pub arg_count: usize,
    /// Начало идентификатора в БАЙТАХ исходного текста (для pos_at и scope_map).
    pub byte: usize,
}

/// Обращение `Голова.Член` — свойство, значение перечисления или метод объекта.
pub struct DotFact {
    pub head: String,
    pub member: String,
    pub head_byte: usize,
    pub member_byte: usize,
    /// Член — ВЫЗОВ (`Модуль.Метод()`), а не обращение к свойству
    /// (`Перечисление.Значение`). Различие существенно: процедуру общего модуля
    /// можно только вызвать, свойств у него не бывает. Без этого признака
    /// `ТипЭлементаФорматированногоДокумента.ПереводСтроки` (значение
    /// платформенного перечисления, которого нет в справке) неотличимо от
    /// обращения к общему модулю.
    pub member_is_call: bool,
}

/// Конструктор `Новый ИмяТипа`.
pub struct NewFact {
    pub type_name: String,
    pub byte: usize,
}

/// Вызов метода у менеджера объекта конфигурации:
/// `Справочники.Сотрудники.НайтиПоРеквизиту(...)`. Получатель метода —
/// двухсегментная голова `Коллекция.Объект`, поэтому [`simple_head`] его не
/// берёт и обычного [`DotFact`] на метод не возникает. Отдельный факт нужен,
/// чтобы сверить имя метода с методами типа-менеджера объекта
/// (`СправочникМенеджер.<Имя справочника>` и т.п.) в валидаторе.
pub struct ManagerCallFact {
    /// Коллекция менеджеров (первый сегмент): `Справочники`, `Документы`, …
    pub collection: String,
    /// Имя объекта конфигурации (второй сегмент): `Сотрудники`.
    pub object: String,
    /// Имя вызванного метода (третий сегмент): `НайтиПоРеквизиту`.
    pub method: String,
    /// Начало идентификатора метода в БАЙТАХ исходного текста (для `pos_at`).
    pub method_byte: usize,
}

/// Присваивание простому идентификатору (`Имя = ...`) или его объявление (`Перем Имя`).
/// Обращения `A.B = ...` и `A[i] = ...` сюда не попадают: у них левая часть —
/// `property_access`, а не `identifier`.
pub struct AssignFact {
    pub name: String,
    /// Начало идентификатора в БАЙТАХ исходного текста (для pos_at).
    pub byte: usize,
    /// `true` — объявление `Перем Имя`, `false` — присваивание `Имя = ...`.
    pub declaration: bool,
    /// Имя типа, если справа стоит конструктор: `Запрос = Новый Запрос;` → `Запрос`.
    /// Даёт тип переменной без всякого вывода типов — и позволяет проверить её
    /// члены (`Запрос.Текстъ` — опечатка) там, где имя переменной совпало с
    /// именем платформенного типа (issue #11).
    pub new_type: Option<String>,
}

/// Процедура/функция модуля: границы и то, что нужно знать о её именах.
pub struct ProcScope {
    /// Границы определения в БАЙТАХ, `[byte_start..byte_end)`.
    pub byte_start: usize,
    pub byte_end: usize,
    /// Имена параметров в нижнем регистре: это локальные имена, они перекрывают
    /// члены контекста модуля.
    pub params: HashSet<String>,
    /// Скомпилирована БЕЗ контекста формы (`&НаСервереБезКонтекста`,
    /// `&НаКлиентеНаСервереБезКонтекста`): членов формы внутри не существует.
    pub no_context: bool,
}

impl ProcScope {
    pub fn contains(&self, byte: usize) -> bool {
        byte >= self.byte_start && byte < self.byte_end
    }
}

/// Имя и байтовое смещение места, где оно связано (например, заголовок цикла).
#[derive(Debug, Clone)]
pub struct NameSite {
    /// Имя в нижнем регистре — сравнение в BSL регистронезависимо.
    pub name: String,
    pub byte: usize,
}

/// Ветви одного условного оператора: диапазоны тел `Тогда`, `ИначеЕсли…`, `Иначе`.
///
/// Нужны выводу типов: если переменная получает РАЗНЫЕ типы в разных ветвях
/// (`Если … Х = Новый Массив; Иначе Х = Новый Структура; КонецЕсли;`), то после
/// `КонецЕсли` её тип — объединение альтернатив, а не тип последней ветки. Без
/// этого имя получало тип одной ветки и давало ложную находку на члене другой
/// (issue #15, класс 1).
#[derive(Debug, Clone)]
pub struct IfBranches {
    /// Диапазон всего оператора `Если … КонецЕсли`.
    pub span: (usize, usize),
    /// Тела ветвей в порядке появления.
    pub branches: Vec<(usize, usize)>,
}

#[derive(Default)]
pub struct AstFacts {
    /// Имена объявленных процедур/функций в нижнем регистре.
    pub declarations: HashSet<String>,
    pub calls: Vec<CallFact>,
    pub dots: Vec<DotFact>,
    pub news: Vec<NewFact>,
    /// Вызовы методов у менеджеров объектов конфигурации
    /// (`Справочники.Сотрудники.НайтиПоРеквизиту(...)`). См. [`ManagerCallFact`].
    pub manager_calls: Vec<ManagerCallFact>,
    /// Присваивания простому идентификатору и объявления `Перем` — для проверки,
    /// не занято ли имя членом контекста модуля (`ShadowedContextName`).
    pub assigns: Vec<AssignFact>,
    /// Имена переменных циклов в нижнем регистре: `Для Каждого Стр Из ... Цикл`
    /// и `Для Сч = 1 По 10 Цикл`. Цикл связывает имя, но НЕ порождает
    /// присваивания в дереве, поэтому в `assigns` таких имён нет. Для
    /// проверяющего кода это полноценное локальное имя: без него `Стр.Поле`
    /// выглядит обращением к чужому объекту (замер на УТ: 39431 ложная находка
    /// именно на переменных циклов — `КлючЗначение`, `Элемент`, `СтрокаТЧ`).
    pub loop_vars: HashSet<String>,
    /// Те же переменные циклов, но с местом связывания — там, где важна ОБЛАСТЬ
    /// ВИДИМОСТИ: имя, связанное циклом в одной процедуре, не должно глушить
    /// проверки в другой. На замере 14943 модулей модуль-широкое правило стоило
    /// 2633 находок: модуль с `Для Каждого Поле Из СписокПолей Цикл` терял
    /// проверку члена у ТИПА `Поле` во всех остальных процедурах.
    pub loop_var_sites: Vec<NameSite>,
    /// Ветви условных операторов — для объединения типов после `КонецЕсли`.
    pub if_branches: Vec<IfBranches>,
    /// Процедуры/функции модуля с их параметрами и признаком «без контекста».
    pub procs: Vec<ProcScope>,
    /// В модуле есть хотя бы одна директива компиляции (`&НаКлиенте`, `&НаСервере`, …).
    /// Признак УПРАВЛЯЕМОЙ формы: в модуле обычной (неуправляемой) формы директив
    /// нет вовсе, и её контекст — другой тип, с другим составом членов.
    pub has_directives: bool,
    /// false — дерево получить не удалось (двоичный модуль, таймаут, сбой языка).
    /// Сегодня отдельно не проверяется: пустое дерево и так даёт пустые facts,
    /// проверки над ними естественно молчат. Поле — задел для вызывающего кода,
    /// которому важно различить «кода нет ошибок» и «дерево не разобралось».
    pub parsed: bool,
}

/// Обход трёх дефектов грамматики `tree-sitter-bsl` 0.1.7 (последняя доступная;
/// у автора открыт issue #7 «parenthesized expressions cause parse errors»).
/// Все замены выполняются ПОБАЙТНО и сохраняют длину, поэтому смещения узлов
/// совпадают с оригиналом — тексты имён мы читаем из исходного текста, а не из
/// нормализованного.
///
/// 1. **Буква `ё`.** Идентификатор описан как `/[\wа-я_][\wа-я_0-9]*/i`, а `ё`
///    (U+0451) в диапазон `а-я` не входит. Имя `СчётаУчёта` рвётся на куски,
///    объявление теряется. Меняем `ё`→`е`, `Ё`→`Е` (обе пары двухбайтные).
///
/// 2. **Тернарный оператор с пробелом.** `? (Усл, А, Б)` уходит в `ERROR`, хотя
///    `?(Усл, А, Б)` разбирается. Переносим пробелы за скобку: `?( Усл, А, Б)`.
///
/// 3. **`ВызватьИсключение;` без аргумента.** Разбирается, только когда стоит
///    первым в теле `Исключение`; внутри `Если`/цикла — `ERROR`. Затираем само
///    слово пробелами: остаётся пустой оператор `;`, для наших фактов он пуст.
///
/// 4. **Неразрывный пробел** (U+00A0) в отступах — грамматика не считает его
///    пробелом. Занимает 2 байта (`C2 A0`), меняем на два обычных пробела.
///
/// 5. **`# Если`** с пробелом после решётки: `#Если` разбирается, `# Если` — нет.
///    Переносим пробелы за имя директивы, как в случае с тернарным оператором.
///
/// 6. **Отрицательное значение параметра по умолчанию** — `Процедура П(А = -1)`.
///    Минус в заголовке меняем на пробел (1 байт → 1 байт). Значения по умолчанию
///    в фактах не используются, поэтому смысл разбора не страдает.
///
/// 7. **Кириллица вне русского алфавита.** Грамматика знает только `а-я` (как и
///    `ё`, см. п.1), поэтому украинские, белорусские, казахские, сербские буквы
///    (`і`, `ї`, `є`, `ґ`, `ў`, `қ`, `ң`, `ә`, `ө`, `ұ`, `ү`, `һ`, `ђ`, `ј`, `ѕ`,
///    `ѣ`, `ѳ`, `ѵ`) рвут идентификатор на куски: `Прав(Закінчення, 1)` выглядит
///    как три аргумента, и корректный код получает `wrong_argument_count` с
///    `confidence: high` (issue #20: 1137 находок на шести конфигурациях, из них
///    1090 — двуязычные ru/uk сообщения `НСтр`). Каждую такую букву меняем на
///    русского двойника (2 байта → 2 байта), не-русские буквы вне таблицы — на
///    `е`: для разбора важно лишь то, что буква законная, а имена мы читаем из
///    ИСХОДНОГО текста, поэтому подмена на смысл не влияет.
///
/// Остаётся один дефект, который так обойти НЕЛЬЗЯ (длина изменится): обращение
/// к результату тернарного оператора — `?(У, А, Б).Метод()`. Он даёт локальный
/// `ERROR`, объявления и вызовы вокруг не теряются, а сам метод обезвреживается
/// в `collect_facts` проверкой точки слева от вызова, иначе он выглядел бы
/// глобальной функцией.
///
/// Если ни один случай не встретился, копия не создаётся.
///
/// Публична, потому что тот же текст должен подавать парсеру любой внешний
/// потребитель этой грамматики (индексатор кода), иначе он унаследует все три
/// дефекта.
pub fn normalize_for_parser(source: &str) -> std::borrow::Cow<'_, str> {
    let bytes = source.as_bytes();
    let has_yo = bytes
        .windows(2)
        .any(|w| w == [0xD1, 0x91] || w == [0xD0, 0x81]);
    let has_nbsp = bytes.windows(2).any(|w| w == [0xC2, 0xA0]);
    let has_ternary_gap = find_ternary_gap(bytes).is_some();
    let has_bare_raise = find_bare_raise(bytes, 0).is_some();
    let has_preproc_gap = find_preproc_gap(bytes, 0).is_some();
    // Позиции минусов считаем один раз: повторный вызов на выходном буфере
    // означал бы ещё один полный проход с обратными сканами заголовков.
    let neg_defaults = negative_defaults(bytes);
    let has_neg_default = !neg_defaults.is_empty();
    let has_other_cyrillic = has_non_russian_cyrillic(bytes);
    if !has_yo
        && !has_nbsp
        && !has_ternary_gap
        && !has_bare_raise
        && !has_preproc_gap
        && !has_neg_default
        && !has_other_cyrillic
    {
        return std::borrow::Cow::Borrowed(source);
    }

    let mut out = bytes.to_vec();

    // ── 1. ё → е, Ё → Е;  4. неразрывный пробел → два обычных;
    //      7. кириллица вне русского алфавита → русский двойник
    let mut i = 0;
    while i + 1 < out.len() {
        if let Some((b0, b1)) = fold_non_russian_cyrillic(out[i], out[i + 1]) {
            out[i] = b0;
            out[i + 1] = b1;
            i += 2;
            continue;
        }
        match (out[i], out[i + 1]) {
            (0xD1, 0x91) => {
                out[i] = 0xD0;
                out[i + 1] = 0xB5;
                i += 2;
            }
            (0xD0, 0x81) => {
                out[i] = 0xD0;
                out[i + 1] = 0x95;
                i += 2;
            }
            (0xC2, 0xA0) => {
                out[i] = b' ';
                out[i + 1] = b' ';
                i += 2;
            }
            _ => i += 1,
        }
    }

    // ── 2. `?` + пробелы/табы + `(`  →  `?(` + те же пробелы/табы
    let mut from = 0;
    while let Some((q, open)) = find_ternary_gap_from(&out, from) {
        out[q + 1] = b'(';
        for b in out.iter_mut().take(open + 1).skip(q + 2) {
            *b = b' ';
        }
        from = open + 1;
    }

    // ── 3. `ВызватьИсключение` / `Raise`, за которыми сразу `;` → пробелы
    let mut from = 0;
    while let Some((start, end)) = find_bare_raise(&out, from) {
        for b in out.iter_mut().take(end).skip(start) {
            *b = b' ';
        }
        from = end;
    }

    // ── 5. `#` + пробелы + буква  →  `#` + буква … пробелы уходят за слово
    let mut from = 0;
    while let Some((hash, word_start, word_end)) = find_preproc_gap(&out, from) {
        let gap = word_start - hash - 1;
        out.copy_within(word_start..word_end, hash + 1);
        for b in out.iter_mut().take(word_end).skip(word_end - gap) {
            *b = b' ';
        }
        from = word_end;
    }

    // ── 6. `= -1` в заголовке процедуры/функции → минус меняем на пробел
    for pos in neg_defaults {
        if out[pos] == b'-' {
            out[pos] = b' ';
        }
    }

    std::borrow::Cow::Owned(String::from_utf8(out).expect("побайтные замены сохраняют UTF-8"))
}

/// Кодовая точка двухбайтной последовательности UTF-8 (0xD0..0xD3 — кириллица).
fn cyrillic_code_point(b0: u8, b1: u8) -> Option<u32> {
    if !(0xD0..=0xD3).contains(&b0) || !(0x80..=0xBF).contains(&b1) {
        return None;
    }
    let cp = ((b0 as u32 & 0x1F) << 6) | (b1 as u32 & 0x3F);
    (0x0400..=0x04FF).contains(&cp).then_some(cp)
}

/// Буква русского алфавита? `ё`/`Ё` считаются русскими: их обрабатывает
/// отдельная ветка нормализации (п.1), и здесь они не трогаются.
fn is_russian_cyrillic(cp: u32) -> bool {
    (0x0410..=0x044F).contains(&cp) || cp == 0x0401 || cp == 0x0451
}

/// Привести код русской буквы к нижнему регистру (для таблицы двойников).
fn lower_russian_cp(cp: u32) -> u32 {
    match cp {
        // А-Я → а-я
        0x0410..=0x042F => cp + 0x20,
        _ => cp,
    }
}

/// Есть ли в тексте кириллица вне русского алфавита (issue #20).
fn has_non_russian_cyrillic(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 1 < bytes.len() {
        if let Some(cp) = cyrillic_code_point(bytes[i], bytes[i + 1]) {
            if !is_russian_cyrillic(cp) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Заменить кириллическую букву вне русского алфавита русским двойником.
///
/// Возвращает новую пару байт (длина сохраняется: кириллица в UTF-8 всегда
/// занимает два байта), либо `None`, если буква русская или это не кириллица.
/// Регистр сохраняем: нормализованный текст остаётся визуально близким к
/// исходному, что помогает при разборе ошибок.
fn fold_non_russian_cyrillic(b0: u8, b1: u8) -> Option<(u8, u8)> {
    let cp = cyrillic_code_point(b0, b1)?;
    if is_russian_cyrillic(cp) {
        return None;
    }
    // Верхний регистр: в основной кириллице (U+0400–U+045F) он на 0x50 ниже
    // строчного, в расширенной (U+0460–U+04FF) — на единицу.
    let (lower_cp, uppercase) = match cp {
        0x0400..=0x040F => (cp + 0x50, true),
        0x0460..=0x0481 | 0x048A..=0x04FF if cp % 2 == 0 => (cp + 1, true),
        _ => (cp, false),
    };
    let russian = russian_lookalike(lower_cp);
    let out_cp = if uppercase {
        lower_russian_cp(russian as u32) - 0x20
    } else {
        russian as u32
    };
    Some((
        0xC0 | ((out_cp >> 6) as u8 & 0x1F),
        0x80 | (out_cp as u8 & 0x3F),
    ))
}

/// Русский двойник для строчной кириллической буквы вне русского алфавита.
///
/// Таблица покрывает буквы, которые встречаются в реальных конфигурациях
/// (украинские, белорусские, казахские, сербские, исторические русские).
/// Незнакомая буква превращается в `е`: для разбора важно лишь то, что это
/// законная буква, а имена читаются из ИСХОДНОГО текста.
fn russian_lookalike(lower_cp: u32) -> char {
    match lower_cp {
        0x0450 | 0x0454 | 0x0463 => 'е', // ѐ є ѣ
        0x0455 => 'з',                   // ѕ
        0x0456 | 0x0475 => 'и',          // і ѵ
        0x0457 | 0x0458 => 'й',          // ї ј
        0x045B => 'ч',                   // ћ
        0x045C => 'к',                   // ќ
        0x045E => 'у',                   // ў
        0x045F => 'ц',                   // џ
        0x0473 => 'ф',                   // ѳ
        0x0491 | 0x0493 => 'г',          // ґ ғ
        0x049B => 'к',                   // қ
        0x04A3 => 'н',                   // ң
        0x04AF | 0x04B1 => 'у',          // ү ұ
        0x04B3 => 'х',                   // ҳ
        0x04BB => 'н',                   // һ
        0x04D9 => 'а',                   // ә
        0x04E9 => 'о',                   // ө
        _ => 'е',
    }
}

/// Позиции минусов в отрицательных значениях параметров по умолчанию.
///
/// Идём от РЕДКИХ кандидатов: минус, слева от которого через пробелы `=`,
/// а справа цифра. Только для них проверяем, что место — список параметров
/// заголовка `Процедура|Функция Имя( … )`. Обратный порядок (искать заголовки,
/// потом минусы внутри) сканировал весь текст и стоил дороже самого разбора.
fn negative_defaults(bytes: &[u8]) -> Vec<usize> {
    /// Заголовок процедуры не бывает длиннее — дальше назад не смотрим.
    const HEADER_LOOKBACK: usize = 4096;

    let mut out = Vec::new();
    for i in 0..bytes.len() {
        if bytes[i] != b'-' {
            continue;
        }
        // справа — цифра?
        let mut d = i + 1;
        while d < bytes.len() && (bytes[d] == b' ' || bytes[d] == b'\t') {
            d += 1;
        }
        if d >= bytes.len() || !bytes[d].is_ascii_digit() {
            continue;
        }
        // слева — знак `=`?
        let mut e = i;
        while e > 0 && (bytes[e - 1] == b' ' || bytes[e - 1] == b'\t') {
            e -= 1;
        }
        if e == 0 || bytes[e - 1] != b'=' {
            continue;
        }
        if in_procedure_header(bytes, e - 1, HEADER_LOOKBACK) {
            out.push(i);
        }
    }
    out
}

/// Позиция `pos` находится внутри списка параметров заголовка процедуры/функции?
/// Идём назад до непарной `(`, затем проверяем «имя» и ключевое слово перед ним.
fn in_procedure_header(bytes: &[u8], pos: usize, lookback: usize) -> bool {
    let stop = pos.saturating_sub(lookback);
    let mut depth = 0i32;
    let mut i = pos;
    let open = loop {
        if i == stop {
            return false;
        }
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    break i;
                }
                depth -= 1;
            }
            // до заголовка эти символы встретиться не должны
            b';' | b'}' => return false,
            _ => {}
        }
    };

    // назад от `(`: пробелы, имя процедуры, пробелы, ключевое слово
    let mut j = open;
    while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t') {
        j -= 1;
    }
    while j > 0 && is_ident_byte(bytes[j - 1]) {
        j -= 1;
    }
    while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t') {
        j -= 1;
    }
    for kw in ["процедура", "функция", "procedure", "function"] {
        let len = kw.len();
        if j >= len && kw_ends_at(bytes, j - len, kw).is_some() {
            return true;
        }
    }
    false
}

/// `#`, за которым идут пробелы/табы, а потом буква: `# Если`, `# Область`.
/// Возвращает `(позиция #, начало слова, конец слова)`.
fn find_preproc_gap(bytes: &[u8], from: usize) -> Option<(usize, usize, usize)> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && is_ident_byte(bytes[j]) {
                let mut k = j;
                while k < bytes.len() && is_ident_byte(bytes[k]) {
                    k += 1;
                }
                return Some((i, j, k));
            }
        }
        i += 1;
    }
    None
}

fn find_ternary_gap(bytes: &[u8]) -> Option<(usize, usize)> {
    find_ternary_gap_from(bytes, 0)
}

/// Позиции `?` и следующей за пробелами `(`. Только если между ними есть хотя бы
/// один пробел или таб — иначе чинить нечего.
fn find_ternary_gap_from(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'?' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == b'(' {
                return Some((i, j));
            }
        }
        i += 1;
    }
    None
}

/// Границы слова `ВызватьИсключение`/`Raise`, за которым (через пробелы) сразу
/// идёт `;`. Форма с аргументом (`ВызватьИсключение "текст";`) грамматике понятна
/// и не трогается. Регистр значения не имеет — в BSL он не различается.
fn find_bare_raise(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    // Отсев по первой букве: `В` = D0 92, `в` = D0 B2, плюс ASCII `R`/`r`.
    // Без него регистронезависимое сравнение звалось бы на каждом байте
    // кириллицы — это и был главный источник замедления нормализации.
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        let maybe_ru = b == 0xD0 && bytes.get(i + 1).is_some_and(|&c| c == 0x92 || c == 0xB2);
        let maybe_en = b == b'R' || b == b'r';
        if maybe_ru || maybe_en {
            for kw in ["вызватьисключение", "raise"] {
                let Some(end) = kw_ends_at(bytes, i, kw) else {
                    continue;
                };
                if !on_word_boundary(bytes, i, end) {
                    continue;
                }
                let mut j = end;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b';' {
                    return Some((i, end));
                }
            }
        }
        i += 1;
    }
    None
}

/// Конец слова, если срез с позиции `i` регистронезависимо равен `kw_lower`.
/// Сравнение побайтное, без аллокаций: `to_lowercase()` на каждом кандидате
/// стоил вдвое дороже всего остального разбора.
fn kw_ends_at(bytes: &[u8], i: usize, kw_lower: &str) -> Option<usize> {
    let kw = kw_lower.as_bytes();
    let end = i + kw.len();
    if end > bytes.len() {
        return None;
    }
    let mut a = i;
    let mut b = 0;
    while b < kw.len() {
        if kw[b] < 0x80 {
            if !bytes[a].eq_ignore_ascii_case(&kw[b]) {
                return None;
            }
            a += 1;
            b += 1;
        } else {
            // кириллица: два байта, приводим исходник к нижнему регистру
            let (l1, l2) = lower_cyrillic(bytes[a], *bytes.get(a + 1)?);
            if l1 != kw[b] || l2 != *kw.get(b + 1)? {
                return None;
            }
            a += 2;
            b += 2;
        }
    }
    Some(end)
}

/// Нижний регистр для двухбайтной кириллицы UTF-8.
/// `А`-`П` = D0 90..9F → +0x20; `Р`-`Я` = D0 A0..AF → D1, −0x20.
fn lower_cyrillic(b1: u8, b2: u8) -> (u8, u8) {
    match (b1, b2) {
        (0xD0, 0x90..=0x9F) => (0xD0, b2 + 0x20),
        (0xD0, 0xA0..=0xAF) => (0xD1, b2 - 0x20),
        _ => (b1, b2),
    }
}

/// Слово стоит на границе: слева и справа не буква/цифра/подчёркивание.
fn on_word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let left_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
    let right_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
    left_ok && right_ok
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Слева от `start` (через пробелы и переводы строк) стоит точка?
fn preceded_by_dot(src: &[u8], start: usize) -> bool {
    let mut i = start;
    while i > 0 {
        match src[i - 1] {
            b' ' | b'\t' | b'\r' | b'\n' => i -= 1,
            b'.' => return true,
            _ => return false,
        }
    }
    false
}

/// Слева от `start` (через пробелы и переводы строк) стоит слово `Новый`/`New`?
///
/// Обычно конструктор виден по узлу `new_expression`, и текстовая проверка не
/// нужна. Но если рядом стоит конструкция, которой грамматика не знает (например
/// `#Если` внутри списка аргументов), узел разваливается, и `Новый Тип(...)`
/// приходит к нам обычным вызовом. Тогда спасает только слово слева.
fn preceded_by_new(src: &[u8], start: usize) -> bool {
    let mut i = start;
    while i > 0 && matches!(src[i - 1], b' ' | b'\t' | b'\r' | b'\n') {
        i -= 1;
    }
    for kw in ["новый", "new"] {
        let len = kw.len();
        if i >= len && kw_ends_at(src, i - len, kw).is_some() && on_word_boundary(src, i - len, i) {
            return true;
        }
    }
    false
}

/// Разобрать `source` деревом tree-sitter-bsl и одним проходом собрать факты
/// для всех проверок уровня 1: объявления процедур/функций, голые вызовы,
/// обращения через точку и конструкторы `Новый`.
pub fn collect_facts(source: &str) -> AstFacts {
    // Двоичный .bsl (EDT-защищённые модули поставщика) — не отдаём в
    // tree-sitter, иначе он деградирует на бесструктурном вводе. Маркер —
    // NUL-байт в первых 8 КБ (см. `code-index::parser::bsl::looks_binary`).
    if source.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return AstFacts::default();
    }

    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_bsl::LANGUAGE.into())
        .is_err()
    {
        // Не смогли выставить язык — молча возвращаем пустые факты.
        return AstFacts::default();
    }
    // Страховка от патологического ввода: 10-секундный дедлайн парсинга.
    #[allow(deprecated)]
    parser.set_timeout_micros(10_000 * 1000);

    // Разбираем нормализованный текст, читаем — оригинальный: смещения совпадают.
    let for_parser = normalize_for_parser(source);
    let Some(tree) = parser.parse(for_parser.as_ref(), None) else {
        return AstFacts::default();
    };

    let src = source.as_bytes();
    let mut facts = AstFacts {
        parsed: true,
        ..Default::default()
    };

    // Итеративный обход: Vec как стек вместо рекурсии — модули конфигурации
    // достигают десятков тысяч строк, а глубина AST у них непредсказуема.
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "procedure_definition" | "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(src) {
                        facts.declarations.insert(name.to_lowercase());
                    }
                }
                let directive = directive_of(node, src);
                if directive.is_some() {
                    facts.has_directives = true;
                }
                facts.procs.push(ProcScope {
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    params: param_names(node, src),
                    no_context: directive.is_some_and(|d| is_no_context(&d)),
                });
            }
            "new_expression" => {
                let mut cursor = node.walk();
                let ident = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "identifier");
                if let Some(ident) = ident {
                    if let Ok(text) = ident.utf8_text(src) {
                        facts.news.push(NewFact {
                            type_name: text.to_string(),
                            byte: ident.start_byte(),
                        });
                    }
                }
            }
            "property_access" | "access" => {
                // `property_access` — цепочка `Голова.Член`, целиком составляющая
                // выражение. У промежуточного звена более длинной цепочки
                // (`ТЗ.Колонкы.Добавить(...)`) та же форма (голова + `property`),
                // но грамматика алиасит в `property_access` только САМЫЙ внешний
                // сегмент — внутренние остаются простым `access`. Голый
                // `access(identifier)` с одним ребёнком сюда не попадает: без
                // второго именованного ребёнка `find` ниже вернёт `None`.
                //
                // Член звена — либо `property` (`Запрос.Текст`), либо `method_call`
                // (`Запрос.Выполнить().Выбрать()`: внутреннее звено — вызов, а не
                // свойство). Без второго случая опечатка в имени метода внутри
                // цепочки не находилась бы, хотя прежняя проверка её ловила.
                let mut cursor = node.walk();
                let member_raw = node
                    .named_children(&mut cursor)
                    .find(|c| matches!(c.kind(), "property" | "method_call"));
                let member_is_call = member_raw.is_some_and(|c| c.kind() == "method_call");
                let member_node = member_raw
                    // У `method_call` именем является его первый ребёнок-identifier.
                    .and_then(|c| {
                        if c.kind() == "method_call" {
                            c.child(0)
                        } else {
                            Some(c)
                        }
                    });
                if let Some(member_node) = member_node {
                    if let Some((head, head_byte)) = simple_head(node.child(0), src) {
                        if let Ok(member) = member_node.utf8_text(src) {
                            facts.dots.push(DotFact {
                                head,
                                member: member.to_string(),
                                head_byte,
                                member_byte: member_node.start_byte(),
                                member_is_call,
                            });
                        }
                    }
                }
            }
            "call_expression" => {
                let mut cursor = node.walk();
                let method_call_node = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "method_call");
                if let Some(mc) = method_call_node {
                    if let Some(member_node) = mc.child(0) {
                        if let Ok(member) = member_node.utf8_text(src) {
                            let receiver = node.child(0);
                            if let Some((head, head_byte)) = simple_head(receiver, src) {
                                facts.dots.push(DotFact {
                                    head,
                                    member: member.to_string(),
                                    head_byte,
                                    member_byte: member_node.start_byte(),
                                    // Ветка `call_expression`: член — всегда вызов.
                                    member_is_call: true,
                                });
                            } else if let Some((collection, object)) =
                                two_segment_head(receiver, src)
                            {
                                // Двухсегментный получатель `Коллекция.Объект` —
                                // `simple_head` его не берёт, DotFact на метод не
                                // рождается. Отдельный факт: метод вызван у
                                // менеджера объекта конфигурации.
                                facts.manager_calls.push(ManagerCallFact {
                                    collection,
                                    object,
                                    method: member.to_string(),
                                    method_byte: member_node.start_byte(),
                                });
                            }
                        }
                    }
                }
            }
            "method_call" => {
                // Метод объекта (`Объект.Метод(...)`) уже разобрала ветка
                // `call_expression` выше — здесь его пропускаем, чтобы не
                // задвоить находку и не принять его за голый глобальный вызов.
                //
                // Исключение — ГОЛОВА цепочки (`ПустаяСсылка().Метаданные()`):
                // слева от неё точки нет, это обычный голый вызов. В дереве она
                // лежит внутри `access`, у которого она единственный именованный
                // ребёнок. Без этого исключения любая опечатка в начале цепочки
                // не проверялась бы вовсе.
                let parent = node.parent();
                let mut is_member_call = parent.is_some_and(|p| {
                    matches!(p.kind(), "call_expression" | "access") && p.named_child_count() > 1
                });
                // Страховка от «восстановления» дерева на конструкциях, которые
                // грамматика не понимает. Два случая, оба подтверждены на УТ:
                //
                // 1. `?(У, А, Б).ПолучитьИмена()` — дерево рвёт так, что
                //    `.ПолучитьИмена()` становится ОТДЕЛЬНЫМ оператором вызова с
                //    обычным родителем, и метод результата выглядит глобальной
                //    функцией. Точка слева говорит, что это не так.
                //
                // 2. Директива препроцессора внутри списка аргументов
                //    (`Новый Структура("а,б", Новый ОписаниеТипов(...), #Если … )`)
                //    — грамматика такого не допускает, узел `new_expression`
                //    разваливается, и конструктор выглядит вызовом функции. Слово
                //    `Новый` слева говорит, что это конструктор.
                if !is_member_call {
                    if let Some(id) = node.child(0) {
                        let b = id.start_byte();
                        is_member_call = preceded_by_dot(src, b) || preceded_by_new(src, b);
                    }
                }
                if !is_member_call {
                    if let Some(id_node) = node.child(0) {
                        if let Ok(name) = id_node.utf8_text(src) {
                            facts.calls.push(CallFact {
                                name: name.to_string(),
                                arg_count: count_arguments(node, src),
                                byte: id_node.start_byte(),
                            });
                        }
                    }
                }
            }
            "if_statement" => {
                // Тела ветвей `Если`: тело `Тогда` — от конца ключевого слова до
                // первой ветви-продолжения; `ИначеЕсли…`/`Иначе` — собственные
                // узлы с готовыми диапазонами (проверено печатью дерева).
                let mut then_start: Option<usize> = None;
                let mut then_end: Option<usize> = None;
                let mut branches: Vec<(usize, usize)> = Vec::new();
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "THEN_KEYWORD" => then_start = Some(child.end_byte()),
                        "elseif_clause" | "else_clause" => {
                            if then_start.is_some() && then_end.is_none() {
                                then_end = Some(child.start_byte());
                            }
                            branches.push((child.start_byte(), child.end_byte()));
                        }
                        // Конец ветви `Тогда` — начало первой ветви-продолжения.
                        "ENDIF_KEYWORD" if then_start.is_some() && then_end.is_none() => {
                            then_end = Some(child.start_byte());
                        }
                        _ => {}
                    }
                }
                if let (Some(start), Some(end)) = (then_start, then_end) {
                    if start < end {
                        branches.insert(0, (start, end));
                    }
                }
                if branches.len() > 1 {
                    facts.if_branches.push(IfBranches {
                        span: (node.start_byte(), node.end_byte()),
                        branches,
                    });
                }
            }
            "for_each_statement" | "for_statement" => {
                // Переменная цикла связывается самим циклом, присваивания в
                // дереве нет: у `for_each_statement` (`Для Каждого Стр Из Т`)
                // его нет вовсе, у `for_statement` (`Для Сч = 1 По 10`)
                // инициализатор — не `assignment_statement`. В обеих формах
                // переменная — ПЕРВЫЙ дочерний `identifier` (проверено печатью
                // дерева: остальные identifier'ы лежат глубже, внутри
                // `expression`).
                let mut cursor = node.walk();
                let ident = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "identifier");
                if let Some(ident) = ident {
                    if let Ok(name) = ident.utf8_text(src) {
                        let lower = name.to_lowercase();
                        facts.loop_vars.insert(lower.clone());
                        facts.loop_var_sites.push(NameSite {
                            name: lower,
                            byte: ident.start_byte(),
                        });
                    }
                }
            }
            "assignment_statement" => {
                // Левая часть — либо `identifier` (`Имя = ...`), либо `property_access`
                // (`A.B = ...`, `A[i] = ...`) — вторые сюда не попадают.
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "identifier" {
                        if let Ok(name) = left.utf8_text(src) {
                            let new_type = constructor_type(node, src);
                            facts.assigns.push(AssignFact {
                                name: name.to_string(),
                                byte: left.start_byte(),
                                declaration: false,
                                new_type,
                            });
                        }
                    }
                }
            }
            "var_statement" => {
                // `Перем А, Б;` внутри процедуры — поле `var_name` уже отдаёт
                // identifier-узлы напрямую, без промежуточного `variable_spec`.
                let mut cursor = node.walk();
                for var_name in node.children_by_field_name("var_name", &mut cursor) {
                    if let Ok(name) = var_name.utf8_text(src) {
                        facts.assigns.push(AssignFact {
                            name: name.to_string(),
                            byte: var_name.start_byte(),
                            declaration: true,
                            new_type: None,
                        });
                    }
                }
            }
            "var_definition" => {
                // `Перем А, Б Экспорт;` на уровне модуля — поле `variable` отдаёт
                // `variable_spec`, а имя лежит в его поле `name`.
                let mut cursor = node.walk();
                for spec in node.children_by_field_name("variable", &mut cursor) {
                    if let Some(name_node) = spec.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(src) {
                            facts.assigns.push(AssignFact {
                                name: name.to_string(),
                                byte: name_node.start_byte(),
                                declaration: true,
                                new_type: None,
                            });
                        }
                    }
                }
            }
            _ => {}
        }

        // Детей кладём справа налево: `pop()` тогда возвращает их слева направо,
        // и факты собираются в порядке текста, а не задом наперёд.
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }

    facts
}

/// Голова обращения через точку: узел `access`, состоящий ровно из одного
/// `identifier`. Цепочки (`Запрос.Выполнить().Выбрать()`) головой не считаются —
/// их разбирает return-type tracking уровня 3 через scope_map.
fn simple_head<'a>(node: Option<tree_sitter::Node<'a>>, src: &[u8]) -> Option<(String, usize)> {
    let node = node?;
    if node.kind() != "access" || node.named_child_count() != 1 {
        return None;
    }
    let ident = node.named_child(0)?;
    if ident.kind() != "identifier" {
        return None;
    }
    let text = ident.utf8_text(src).ok()?;
    Some((text.to_string(), ident.start_byte()))
}

/// Двухсегментная голова обращения `<Идентификатор1>.<Идентификатор2>`
/// (`Справочники.Сотрудники`): узел `access`/`property_access`, у которого
/// первый сегмент — сам простая односегментная голова, а второй — узел
/// `property` (без вызова). Возвращает (первый сегмент, второй сегмент).
///
/// Более длинные цепочки (`А.Б.В`) и головы с вызовом внутри (`Х().Y`) под
/// правило не подходят: у них первый сегмент составной, `simple_head` вернёт
/// `None`. Нужна только для распознавания `Коллекция.Объект` как получателя
/// метода менеджера.
fn two_segment_head(node: Option<tree_sitter::Node>, src: &[u8]) -> Option<(String, String)> {
    let node = node?;
    if !matches!(node.kind(), "access" | "property_access") {
        return None;
    }
    let (head, _) = simple_head(node.child(0), src)?;
    let mut cursor = node.walk();
    let property = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "property")?;
    let object = property.utf8_text(src).ok()?.to_string();
    Some((head, object))
}

/// Директива компиляции процедуры/функции без амперсанда (`НаСервере`, `Вместо`).
///
/// В дереве она лежит ПЕРЕД узлом определения: предыдущий сосед — `preprocessor`,
/// внутри которого узел `annotation` вида `&НаСервере`. Между директивой и
/// объявлением встречаются комментарии (на УТ — сотни случаев, из них десятки
/// `…БезКонтекста`), а директив бывает несколько подряд (`&НаКлиенте` +
/// `&Вместо(…)`): комментарии пропускаем, а из директив предпочитаем контекстную
/// (без параметров) — именно она решает, существует ли контекст формы.
fn directive_of(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let mut nearest: Option<String> = None;
    let mut context: Option<String> = None;
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "line_comment" | "comment" => {
                prev = p.prev_sibling();
            }
            "preprocessor" => {
                if let Some(directive) = annotation_text(p, src) {
                    let with_params = p.utf8_text(src).is_ok_and(|t| t.contains('('));
                    if !with_params && context.is_none() {
                        context = Some(directive.clone());
                    }
                    if nearest.is_none() {
                        nearest = Some(directive);
                    }
                }
                prev = p.prev_sibling();
            }
            _ => break,
        }
    }
    context.or(nearest)
}

/// Текст аннотации внутри `preprocessor` без ведущего амперсанда.
fn annotation_text(preprocessor: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let mut cursor = preprocessor.walk();
    let annotation = preprocessor
        .named_children(&mut cursor)
        .find(|c| c.kind() == "annotation")?;
    annotation
        .utf8_text(src)
        .ok()
        .map(|t| t.trim_start_matches('&').to_string())
}

/// Имена параметров процедуры/функции в нижнем регистре.
///
/// Параметр — локальное имя: оно перекрывает член контекста модуля. В УТ так
/// делает сама 1С (`&НаКлиенте Процедура …(УчетнаяЗаписьНастроена, Параметры)`),
/// и присваивание такому параметру законно.
fn param_names(node: tree_sitter::Node, src: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    let Some(params) = node.child_by_field_name("parameters") else {
        return names;
    };
    let mut cursor = params.walk();
    for param in params.children_by_field_name("parameter", &mut cursor) {
        if let Some(name_node) = param.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(src) {
                names.insert(name.to_lowercase());
            }
        }
    }
    names
}

/// Тип из конструктора в правой части присваивания: `Запрос = Новый Запрос;`.
///
/// Тип берётся только тогда, когда конструктор составляет ВСЮ правую часть:
/// `Х = Новый Массив` — да; `Х = ?(У, Новый Массив, Неопределено)` и
/// `Х = Новый Массив().Количество()` — нет (там тип переменной другой).
/// Так тип известен точно, без вывода типов и без догадок, — и проверка членов
/// работает даже там, где имя переменной совпало с именем платформенного типа
/// (`Запрос = Новый HTTPЗапрос`, issue #11).
fn constructor_type(statement: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let right = statement.child_by_field_name("right")?;
    let mut node = right;
    // Правая часть обёрнута в узлы выражения — спускаемся, пока начало совпадает.
    loop {
        if node.kind() == "new_expression" {
            let tail = src.get(node.end_byte()..right.end_byte())?;
            if !tail.iter().all(|b| b.is_ascii_whitespace()) {
                return None;
            }
            // Хвост после правой части — `;` (возможно, с комментарием за ним) или
            // конец строки. Иначе за выражением остался код, который дерево НЕ
            // включило в правую часть: цепочку `Новый Массив().Количество()`
            // грамматика оставляет отдельным узлом, и типом переменной был бы
            // `Массив` вместо результата вызова.
            if !statement_tail_is_end(src, right.end_byte()) {
                return None;
            }
            let mut cursor = node.walk();
            let ident = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "identifier")?;
            return ident.utf8_text(src).ok().map(|s| s.to_string());
        }
        let child = node.named_child(0)?;
        if child.start_byte() != node.start_byte() {
            return None;
        }
        node = child;
    }
}

/// После позиции `from` идёт конец оператора — `;`, возможно с комментарием,
/// либо конец строки или текста?
fn statement_tail_is_end(src: &[u8], from: usize) -> bool {
    let mut i = from;
    let skip_spaces = |i: &mut usize| {
        while matches!(src.get(*i), Some(b' ') | Some(b'\t')) {
            *i += 1;
        }
    };
    let at_line_end = |i: usize| matches!(src.get(i), None | Some(b'\n') | Some(b'\r'));
    skip_spaces(&mut i);
    if at_line_end(i) {
        return true;
    }
    if src.get(i) != Some(&b';') {
        return false;
    }
    i += 1;
    skip_spaces(&mut i);
    at_line_end(i) || (src.get(i) == Some(&b'/') && src.get(i + 1) == Some(&b'/'))
}

/// Директива компилирует процедуру БЕЗ контекста формы?
///
/// `&НаСервереБезКонтекста` и `&НаКлиентеНаСервереБезКонтекста` (в английской
/// локали — `AtServerNoContext`, `AtClientAtServerNoContext`). В таких процедурах
/// членов формы не существует: `Элементы`, `Параметры`, `Объект` — свободные имена,
/// и форму туда передают параметром (`Элементы = Форма.Элементы;`).
fn is_no_context(directive: &str) -> bool {
    let d = directive.to_lowercase();
    d.ends_with("безконтекста") || d.ends_with("nocontext")
}

/// Число аргументов голого вызова `Имя(...)`.
///
/// Считаем РАЗДЕЛИТЕЛИ в тексте списка аргументов, а не узлы дерева: аргументы
/// разделяются запятыми верхнего уровня, и только они. Узловой подсчёт ошибался на
/// соседних строковых литералах: платформа склеивает их через перевод строки
/// (`СтрДлина("a" "b")` → 3, `Формат(Дата, "ДФ=" "дддд")` → «суббота»), а
/// грамматика `tree-sitter-bsl` восстанавливается после ошибки и режет текст по
/// запятым ВНУТРИ второго литерала:
///
/// ```text
/// НСтр("a"
/// "b, c, d")   →  узлы: "a" | "b | c | d | "   →  четыре «аргумента»
/// ```
///
/// Сначала это давало ложный `wrong_argument_count` на соседних литералах
/// (issue #21), а после правки по зазорам — снова ложный, но уже на запятых внутри
/// литерала-продолжения (issue #26). Счёт по тексту закрывает оба случая:
/// содержимое строкового литерала (включая продолжения через `|`) и комментарии
/// пропускаются целиком, запятые внутри скобок и индексов не считаются.
///
/// Побочные эффекты осознанные: пропущенная запятая между настоящими аргументами
/// (`Метод(А Б)`) даёт один аргумент, а лишняя (`Метод(1, )`) — два: на
/// синтаксически неверном коде счёт аргументов не проверяем.
///
/// Узла `arguments` нет — аргументов 0.
fn count_arguments(method_call: tree_sitter::Node, src: &[u8]) -> usize {
    let mut cursor = method_call.walk();
    let args = method_call
        .named_children(&mut cursor)
        .find(|c| c.kind() == "arguments");
    let Some(args) = args else {
        return 0;
    };
    // Внешние скобки списка в счёт не идут.
    let mut start = args.start_byte();
    let mut end = args.end_byte();
    if src.get(start) == Some(&b'(') {
        start += 1;
    }
    if end > start && src.get(end - 1) == Some(&b')') {
        end -= 1;
    }
    count_arguments_in_text(&src[start..end])
}

/// Число аргументов по тексту списка (без внешних скобок).
fn count_arguments_in_text(text: &[u8]) -> usize {
    let mut commas = 0usize;
    let mut has_content = false;
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < text.len() {
        let b = text[i];
        match b {
            // Строковый литерал: `""` внутри — экранированная кавычка, перевод
            // строки — продолжение строки (`|…`). Запятые внутри не разделители.
            b'"' => {
                has_content = true;
                i += 1;
                while i < text.len() {
                    if text[i] == b'"' {
                        if text.get(i + 1) == Some(&b'"') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            // Комментарий — до конца строки.
            b'/' if text.get(i + 1) == Some(&b'/') => {
                while i < text.len() && text[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'(' | b'[' => {
                depth += 1;
                has_content = true;
            }
            b')' | b']' => {
                depth -= 1;
                has_content = true;
            }
            b',' if depth == 0 => commas += 1,
            b if !b.is_ascii_whitespace() => has_content = true,
            _ => {}
        }
        i += 1;
    }
    if has_content {
        commas + 1
    } else if commas > 0 {
        // Список из одних запятых — это ПРОПУЩЕННЫЙ аргумент: в выгрузке УТ есть
        // `ПоказатьПредупреждение(,)`, и дерево отдаёт на него ровно один узел
        // `omitted_argument`. Считать такое за ноль аргументов значит выдать
        // ложную находку на рабочем коде.
        1
    } else {
        0
    }
}

// ── Очистка строк и комментариев ──────────────────────────────────────────

/// Замаскировать пробелами строковые литералы и комментарии. Длина и позиции
/// строк сохраняются — это важно для line/col, передаваемых в ошибки. Пробелами
/// заменяются ВСЕ байты содержимого (кроме переводов строк): многобайтный символ
/// превращается в несколько пробелов, UTF-8 остаётся валидным.
/// Убрать директивы препроцессора расширений, сохранив длину строк.
///
/// В модуле расширения блок `#Удаление … #КонецУдаления` содержит код исходного
/// модуля, который расширение выбрасывает: в скомпилированный модуль он не
/// попадает. Беда в том, что этот код может обрывать строковый литерал на
/// середине — тогда сам файл перестаёт быть корректным BSL, а маскировка строк
/// «съезжает» и весь текст запроса ниже начинает считаться кодом (наблюдалось на
/// `#Удаление` внутри текста запроса: закрывающая кавычка стояла в удаляемой
/// строке, а вставляемая её не имела).
///
/// Поэтому удаляемые строки и сами строки-маркеры затираются пробелами до всякого
/// разбора. Позиции сохраняются: и номера строк, и колонки остаются прежними.
pub fn strip_extension_directives(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let mut in_deleted = false;
    let mut pos = 0usize;

    for line in src.split_inclusive('\n') {
        let body_len = line.trim_end_matches(['\n', '\r']).len();
        let head = line
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}')
            .to_lowercase();

        // Имя директивы — до пробела: `#УдалениеВременныхТаблиц` не должно
        // считаться маркером `#Удаление` и затирать весь остаток файла.
        let name = head.split_whitespace().next().unwrap_or("");
        let starts_delete = matches!(name, "#удаление" | "#delete");
        let ends_delete = matches!(name, "#конецудаления" | "#enddelete");
        let is_marker = starts_delete
            || ends_delete
            || matches!(
                name,
                "#вставка" | "#конецвставки" | "#insert" | "#endinsert"
            );

        if starts_delete {
            in_deleted = true;
        }
        if is_marker || in_deleted {
            // Затираем побайтно: длина и позиции сохраняются, UTF-8 остаётся валидным.
            for b in out.iter_mut().take(pos + body_len).skip(pos) {
                if *b != b'\t' {
                    *b = b' ';
                }
            }
        }
        if ends_delete {
            in_deleted = false;
        }
        pos += line.len();
    }

    String::from_utf8(out).expect("затирание пробелами сохраняет UTF-8 валидность")
}

pub fn mask_strings_and_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Однострочный комментарий //...
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' {
                if bytes[j] != b'\r' {
                    out[j] = b' ';
                }
                j += 1;
            }
            i = j;
            continue;
        }
        // Строка "..."
        if b == b'"' {
            out[i] = b' '; // открывающая кавычка
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'"' {
                    if j + 1 < bytes.len() && bytes[j + 1] == b'"' {
                        // escaped quote — затираем обе и идём дальше
                        out[j] = b' ';
                        out[j + 1] = b' ';
                        j += 2;
                        continue;
                    }
                    out[j] = b' ';
                    j += 1;
                    break;
                }
                if bytes[j] == b'\n' {
                    // Перевод строки внутри многострочного литерала. Платформа
                    // разрешает вставлять между строками-продолжениями (`|`)
                    // обычные комментарии. Кавычка в таком комментарии литерал
                    // НЕ закрывает — иначе всё, что ниже, инвертируется:
                    // код считается строкой, а текст запроса кодом.
                    j += 1;
                    let mut k = j;
                    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                        k += 1;
                    }
                    if k + 1 < bytes.len() && bytes[k] == b'/' && bytes[k + 1] == b'/' {
                        while k < bytes.len() && bytes[k] != b'\n' {
                            if bytes[k] != b'\r' {
                                out[k] = b' ';
                            }
                            k += 1;
                        }
                        j = k;
                    }
                    continue;
                }
                if bytes[j] != b'\r' {
                    out[j] = b' ';
                }
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }

    String::from_utf8(out).expect("mask_strings_and_comments сохраняет UTF-8 валидность")
}

/// Запасной сбор объявлений построчно — на случай, когда tree-sitter не смог
/// разобрать модуль и часть `proc_declaration`/`func_declaration` потерялась.
///
/// Строки и комментарии предварительно замаскированы, поэтому слово `Процедура`
/// внутри строкового литерала объявлением не станет. Имя берётся до первой
/// открывающей скобки; строки без скобки игнорируются.
pub fn scan_declarations(source: &str) -> HashSet<String> {
    let cleaned = mask_strings_and_comments(source);
    let mut names = HashSet::new();
    for line in cleaned.lines() {
        let trimmed = line.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
        let lower = trimmed.to_lowercase();
        // После ключевого слова — ЛЮБОЙ пробельный разделитель (в корпусе
        // встречается табуляция: `Функция\tИмя()`), поэтому пробел в шаблон
        // не входит, но разделитель обязателен.
        let Some(rest) = ["процедура", "функция", "procedure", "function"]
            .iter()
            .find_map(|kw| lower.strip_prefix(kw))
        else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(char::is_whitespace) else {
            continue;
        };
        let Some((name, _)) = rest.split_once('(') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && !name.contains(char::is_whitespace) {
            names.insert(name.to_string());
        }
    }
    names
}

/// Имена процедур и функций, объявленных в модуле (в нижнем регистре).
///
/// Объединение ДВУХ источников: дерева и текстового прохода. Ни один не полон —
/// дерево теряет объявления на файлах с `ERROR`, а текстовый проход не видит
/// заголовков с переносом строки перед скобкой. Платформенный индекс здесь не
/// нужен: это чистый разбор.
///
/// Вынесено в публичный API, потому что тот же список объявлений нужен внешнему
/// индексатору кода (code-index), а не только валидатору.
pub fn module_declarations(source: &str) -> HashSet<String> {
    let (from_ast, from_text) = module_declarations_split(source);
    let mut all = from_ast;
    all.extend(from_text);
    all
}

/// Те же объявления, но раздельно: `(из дерева, из текста)`. Нужно для замеров
/// качества разбора — какой источник что теряет.
pub fn module_declarations_split(source: &str) -> (HashSet<String>, HashSet<String>) {
    let source = &strip_extension_directives(source);
    let facts = collect_facts(source);
    (facts.declarations, scan_declarations(source))
}

/// Объявление процедуры/функции модуля — то, что нужно облегчённому индексу.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodDecl {
    pub name: String,
    pub is_function: bool,
    pub is_export: bool,
    /// Директива компиляции без амперсанда: "НаСервере", "Вместо" и т.п.
    pub directive: Option<String>,
    /// 1-based номер строки объявления.
    pub line_start: u32,
    /// Текст списка параметров вместе со скобками: "(А, Знач Б = 1)".
    pub params: Option<String>,
}

/// Методы, объявленные в модуле. Дерево + нормализация (та же, что в collect_facts).
/// Текстовой страховки здесь НЕТ: она даёт только имена, без строк и параметров.
pub fn collect_methods(source: &str) -> Vec<MethodDecl> {
    // Двоичный .bsl (EDT-защищённые модули поставщика) — не отдаём в
    // tree-sitter, см. collect_facts.
    if source.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return Vec::new();
    }

    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_bsl::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    #[allow(deprecated)]
    parser.set_timeout_micros(10_000 * 1000);

    // Разбираем нормализованный текст, читаем — оригинальный: смещения совпадают.
    let for_parser = normalize_for_parser(source);
    let Some(tree) = parser.parse(for_parser.as_ref(), None) else {
        return Vec::new();
    };

    let src = source.as_bytes();
    let mut methods = Vec::new();

    // Итеративный обход: та же схема, что в collect_facts.
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let "procedure_definition" | "function_definition" = node.kind() {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(src) {
                    let params = node
                        .child_by_field_name("parameters")
                        .and_then(|p| p.utf8_text(src).ok())
                        .map(|s| s.to_string());
                    let is_export = node.child_by_field_name("export").is_some();

                    let directive = directive_of(node, src);

                    methods.push(MethodDecl {
                        name: name.to_string(),
                        is_function: node.kind() == "function_definition",
                        is_export,
                        directive,
                        line_start: node.start_position().row as u32 + 1,
                        params,
                    });
                }
            }
        }

        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }

    methods
}

// ── Тексты запросов на языке запросов 1С ──────────────────────────────────

/// Кусок текста запроса, непрерывный и в собранном тексте, и в тексте модуля.
///
/// Куски нужны потому, что значение строкового литерала BSL не совпадает с его
/// записью в файле: символ продолжения `|` и отступ перед ним в значение не
/// входят, `""` даёт одну кавычку, а конкатенация склеивает части, лежащие в
/// модуле далеко друг от друга. Без такой карты находку нельзя показать на
/// строке модуля — а находка без координаты бесполезна.
pub struct QuerySpan {
    /// Смещение куска в собранном тексте запроса, в БАЙТАХ.
    pub text_offset: usize,
    /// Смещение того же куска в тексте модуля, в БАЙТАХ.
    pub module_byte: usize,
    pub len: usize,
}

/// Текст запроса, собранный из одного или нескольких строковых литералов.
pub struct QueryText {
    /// Значение, которое платформа отдаст движку запросов.
    pub text: String,
    pub spans: Vec<QuerySpan>,
    /// Начало первого литерала в модуле — координата запроса как целого.
    pub byte: usize,
}

impl QueryText {
    /// Смещение внутри собранного текста → смещение в тексте модуля.
    ///
    /// Смещение, попавшее на стык кусков (символ продолжения, пропущенный
    /// комментарий), отдаёт конец ближайшего куска слева: приблизительная
    /// позиция полезнее потерянной.
    pub fn map_offset(&self, text_offset: usize) -> usize {
        let mut best = self.byte;
        for span in &self.spans {
            if text_offset < span.text_offset {
                break;
            }
            best = if text_offset < span.text_offset + span.len {
                span.module_byte + (text_offset - span.text_offset)
            } else {
                span.module_byte + span.len
            };
        }
        best
    }
}

/// Строковый литерал модуля вместе с картой кусков, попадающих в его значение.
struct Literal {
    /// Байт открывающей кавычки.
    start: usize,
    /// Байт сразу за закрывающей кавычкой.
    end: usize,
    /// Куски значения: (смещение в модуле, длина).
    parts: Vec<(usize, usize)>,
}

/// Собрать тексты запросов, записанные в модуле строковыми литералами.
///
/// Запрос узнаётся по первому слову собранного значения, а не по тому, куда оно
/// присваивается: одним механизмом покрываются `Запрос.Текст = "ВЫБРАТЬ …"`,
/// `Новый Запрос("ВЫБРАТЬ …")`, накопление через `+=` и текст схемы компоновки.
///
/// Конкатенация с не-литералом (`"ВЫБРАТЬ " + ИмяПоля + " ИЗ …"`) даёт запрос,
/// текст которого известен лишь частично. Такой запрос НЕ возвращается вовсе:
/// подстановка чего-либо на место неизвестного куска порождает находки на
/// месте, которого в запросе нет.
///
/// Разбор идёт по оригинальному тексту, а не по замаскированному
/// (`mask_strings_and_comments`) — там от запроса остаются одни пробелы.
/// Блоки `#Удаление` снимаются заранее: их код в конфигурацию не попадает.
pub fn collect_query_texts(source: &str) -> Vec<QueryText> {
    // Двоичный .bsl (EDT-защищённые модули поставщика) — см. collect_facts.
    if source.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return Vec::new();
    }

    let cleaned = strip_extension_directives(source);
    let bytes = cleaned.as_bytes();
    let literals = scan_literals(bytes);

    let mut queries = Vec::new();
    let mut i = 0;
    while i < literals.len() {
        // Плюс слева означает, что начало текста вычисляется, а не записано.
        let mut dirty = preceded_by_plus(bytes, literals[i].start);
        let mut last = i;

        loop {
            let after = skip_ws_and_comments(bytes, literals[last].end);
            if after >= bytes.len() || bytes[after] != b'+' {
                break;
            }
            let next = skip_ws_and_comments(bytes, after + 1);
            if last + 1 < literals.len() && literals[last + 1].start == next {
                last += 1;
                continue;
            }
            // За плюсом стоит не литерал — часть текста запроса неизвестна.
            dirty = true;
            break;
        }

        if !dirty {
            if let Some(query) = assemble_query(bytes, &literals[i..=last]) {
                queries.push(query);
            }
        }
        i = last + 1;
    }

    queries
}

/// Найти строковые литералы вместе с картой кусков их значения.
///
/// Границы литерала определяются ровно так же, как в `mask_strings_and_comments`
/// (включая `""` и комментарий между строками-продолжениями) — расхождение двух
/// разборов означало бы, что валидатор и извлечение запросов видят разный код.
fn scan_literals(bytes: &[u8]) -> Vec<Literal> {
    let mut literals = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        // Однострочный комментарий — литералов внутри нет.
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }

        let start = i;
        let mut parts: Vec<(usize, usize)> = Vec::new();
        // Текущий непрерывный кусок значения: (начало, длина).
        let mut part: Option<(usize, usize)> = None;
        let mut j = i + 1;

        while j < bytes.len() {
            if bytes[j] == b'"' {
                if j + 1 < bytes.len() && bytes[j + 1] == b'"' {
                    // Удвоенная кавычка: в значение попадает одна.
                    match &mut part {
                        Some((_, len)) => *len += 1,
                        None => part = Some((j, 1)),
                    }
                    if let Some(p) = part.take() {
                        parts.push(p);
                    }
                    j += 2;
                    continue;
                }
                j += 1;
                break;
            }

            if bytes[j] == b'\r' && j + 1 < bytes.len() && bytes[j + 1] == b'\n' {
                // CRLF: в значение входит только `\n`. `\r` разрывает кусок
                // карты — иначе он попал бы в собираемый текст, а смещение
                // куска указывало бы на последовательность с `\r`.
                if let Some(p) = part.take() {
                    parts.push(p);
                }
                j += 1;
                continue;
            }
            if bytes[j] == b'\n' {
                // Перевод строки — часть значения, а вот отступ, символ
                // продолжения `|` и комментарий между строками — нет.
                match &mut part {
                    Some((_, len)) => *len += 1,
                    None => part = Some((j, 1)),
                }
                if let Some(p) = part.take() {
                    parts.push(p);
                }
                j += 1;
                // Начало строки литерала: отступ, символ продолжения `|` и
                // комментарии между строками в значение не входят. Комментарий
                // съедается вместе со своим переводом строки — иначе он оставил
                // бы в тексте запроса пустую строку, которой в значении нет.
                loop {
                    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                    if j + 1 < bytes.len() && bytes[j] == b'/' && bytes[j + 1] == b'/' {
                        while j < bytes.len() && bytes[j] != b'\n' {
                            j += 1;
                        }
                        if j < bytes.len() {
                            j += 1;
                        }
                        continue;
                    }
                    if j < bytes.len() && bytes[j] == b'|' {
                        j += 1;
                    }
                    break;
                }
                continue;
            }

            match &mut part {
                Some((_, len)) => *len += 1,
                None => part = Some((j, 1)),
            }
            j += 1;
        }

        if let Some(p) = part.take() {
            parts.push(p);
        }
        literals.push(Literal {
            start,
            end: j,
            parts,
        });
        i = j;
    }

    literals
}

/// Пропустить пробелы, переводы строк и однострочные комментарии вправо.
fn skip_ws_and_comments(bytes: &[u8], mut i: usize) -> usize {
    loop {
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        return i;
    }
}

/// Стоит ли слева от литерала знак конкатенации.
///
/// Пробелы, переводы строк И однострочные комментарии между `+` и литералом не
/// делают текст «чистым»: `X + // c\n "ВЫБРАТЬ 1"` — вычисляемая часть, и
/// пропустить её значит выдать динамический текст за статический запрос.
fn preceded_by_plus(bytes: &[u8], start: usize) -> bool {
    let mut i = start;
    loop {
        while i > 0 && (bytes[i - 1] as char).is_ascii_whitespace() {
            i -= 1;
        }
        if i == 0 {
            return false;
        }
        // Хвост текущей строки может быть комментарием (в том числе после
        // `+`): пропускаем его и смотрим, что было до него.
        let line_start = bytes[..i]
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        if let Some(comment) = comment_start_on_line(&bytes[line_start..i], line_start) {
            i = comment;
            continue;
        }
        return bytes[i - 1] == b'+';
    }
}

/// Начало хвостового `//`-комментария в срезе строки — при условии, что `//`
/// не находится внутри строкового литерала. `line_start` — смещение среза
/// в исходном массиве.
fn comment_start_on_line(line: &[u8], line_start: usize) -> Option<usize> {
    let mut in_string = false;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'"' => {
                if in_string && i + 1 < line.len() && line[i + 1] == b'"' {
                    i += 2; // удвоенная кавычка — экранирование, строка не кончилась
                    continue;
                }
                in_string = !in_string;
                i += 1;
            }
            b'/' if !in_string && i + 1 < line.len() && line[i + 1] == b'/' => {
                return Some(line_start + i);
            }
            _ => i += 1,
        }
    }
    None
}

/// Склеить значение группы литералов и отсеять всё, что не является запросом.
fn assemble_query(bytes: &[u8], group: &[Literal]) -> Option<QueryText> {
    let mut text = String::new();
    let mut spans = Vec::new();

    for literal in group {
        for &(module_byte, len) in &literal.parts {
            let chunk = std::str::from_utf8(&bytes[module_byte..module_byte + len]).ok()?;
            spans.push(QuerySpan {
                text_offset: text.len(),
                module_byte,
                len,
            });
            text.push_str(chunk);
        }
    }

    if !looks_like_query(&text) {
        return None;
    }

    Some(QueryText {
        text,
        spans,
        byte: group[0].start,
    })
}

/// Начинается ли значение с ключевого слова, с которого может начинаться запрос.
///
/// Проверяется именно первое слово: подстроки вроде «выбрать» где-то в середине
/// сообщения пользователю запросом не являются. Ведущие `//`-строки —
/// комментарии языка запросов: запросы нередко начинаются с них.
fn looks_like_query(text: &str) -> bool {
    let mut rest = text.trim_start();
    while let Some(after) = rest.strip_prefix("//") {
        let Some(eol) = after.find('\n') else {
            return false;
        };
        rest = after[eol + 1..].trim_start();
    }
    let head: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    matches!(
        head.to_uppercase().as_str(),
        "ВЫБРАТЬ" | "SELECT" | "УНИЧТОЖИТЬ" | "DROP"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_calls_with_nested_call_argument() {
        let facts = collect_facts("Сообщить(Строка(1));");
        assert!(facts.parsed);
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Сообщить"), "нет Сообщить: {:?}", names);
        assert!(names.contains(&"Строка"), "нет Строка: {:?}", names);
        for call in &facts.calls {
            assert_eq!(call.arg_count, 1, "аргумент у {}", call.name);
        }
    }

    #[test]
    fn omitted_argument_counts_as_one() {
        let facts = collect_facts("Ф(1, , 3);");
        assert_eq!(facts.calls.len(), 1);
        assert_eq!(facts.calls[0].arg_count, 3);
    }

    #[test]
    fn comma_inside_string_is_not_a_separator() {
        let facts = collect_facts("Ф(\"а,б\", 2);");
        assert_eq!(facts.calls.len(), 1);
        assert_eq!(facts.calls[0].arg_count, 2);
    }

    fn arg_count_of(src: &str, name: &str) -> usize {
        collect_facts(src)
            .calls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("нет вызова {name} в {src:?}"))
            .arg_count
    }

    /// Issue #20: кириллица вне русского алфавита не рвёт идентификатор.
    ///
    /// Грамматика знает только `а-я`, поэтому украинские, белорусские и казахские
    /// буквы раньше делили имя на куски, и `Прав(Закінчення, 1)` выглядело как
    /// вызов с тремя аргументами.
    #[test]
    fn non_russian_cyrillic_letters_keep_identifier_whole() {
        for name in ["Закінчення", "Їжак", "Єнот", "Ґанок", "Қазақ", "ўсе", "ђак"]
        {
            let src = format!("Процедура Т()\n{name} = 1;\nР = Прав({name}, 1);\nД = СтрДлина({name});\nКонецПроцедуры\n");
            let facts = collect_facts(&src);
            let call = facts
                .calls
                .iter()
                .find(|c| c.name == "Прав")
                .unwrap_or_else(|| panic!("{name}: вызов Прав не найден"));
            assert_eq!(
                call.arg_count,
                2,
                "{name}: имя + 1 — два аргумента, а не три (факты: {:?})",
                facts
                    .calls
                    .iter()
                    .map(|c| (&c.name, c.arg_count))
                    .collect::<Vec<_>>()
            );
            let single = facts
                .calls
                .iter()
                .find(|c| c.name == "СтрДлина")
                .unwrap_or_else(|| panic!("{name}: вызов СтрДлина не найден"));
            assert_eq!(
                single.arg_count,
                1,
                "{name}: у СтрДлина один аргумент (факты: {:?})",
                facts
                    .calls
                    .iter()
                    .map(|c| (&c.name, c.arg_count))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn non_russian_cyrillic_normalization_keeps_length() {
        let src = "Закінчення = \"ок\";";
        let normalized = normalize_for_parser(src);
        assert_eq!(normalized.len(), src.len(), "длина обязана сохраняться");
        assert_ne!(normalized.as_ref(), src, "буква должна быть заменена");
        // Чисто русский текст не трогаем вовсе.
        let pure = "Закинчення = \"ок\";";
        assert!(matches!(
            normalize_for_parser(pure),
            std::borrow::Cow::Borrowed(_)
        ));
        // Латинские имена тоже не трогаем.
        let latin = "Check = \"ok\";";
        assert!(matches!(
            normalize_for_parser(latin),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// Issue #21: соседние строковые литералы — ОДИН аргумент.
    ///
    /// Платформа склеивает их через перевод строки (`СтрДлина("a" "b")` → 3,
    /// `Формат(Дата, "ДФ=" "дддд")` → «суббота»), а грамматика второй литерал
    /// отдаёт узлом `ERROR`. Пока он считался отдельным аргументом, корректный
    /// код получал ложный `wrong_argument_count` с `confidence: high`.
    #[test]
    fn adjacent_string_literals_are_one_argument() {
        assert_eq!(arg_count_of("СтрДлина(\"a\" \"b\");", "СтрДлина"), 1);
        assert_eq!(arg_count_of("СтрДлина(\"a\"\n\"b\");", "СтрДлина"), 1);
        assert_eq!(arg_count_of("Формат(Дата, \"ДФ=\" \"дддд\");", "Формат"), 2);
        assert_eq!(
            arg_count_of("НСтр(\"ru='x'\"\n\"';uk='y'\"\n\"'\");", "НСтр"),
            1,
            "многострочный НСтр из соседних литералов — один аргумент"
        );
        // Запятая по-прежнему разделяет аргументы.
        assert_eq!(arg_count_of("СтрДлина(\"a\", \"b\");", "СтрДлина"), 2);
        // Вложенные вызовы и пропущенный аргумент не затронуты.
        assert_eq!(arg_count_of("Ф(Ф(1, 2), 3);", "Ф"), 2);
        assert_eq!(arg_count_of("Ф(1, , 3);", "Ф"), 3);
    }

    /// Issue #26: запятые ВНУТРИ литерала-продолжения — не разделители.
    ///
    /// Грамматика режет второй литерал по запятым (`"b, c, d"` → `"b` | `c` | `d` |
    /// `"`), и подсчёт по узлам видел три-четыре «аргумента». Считаем по тексту:
    /// содержимое литералов пропускается целиком.
    #[test]
    fn commas_inside_adjacent_literals_are_not_separators() {
        // Таблица автора issue: все эти вызовы — ОДИН аргумент.
        for src in [
            "НСтр(\"a\"\n\"b, c\");",
            "НСтр(\"a\"\n\"b, c, d\");",
            "НСтр(\"a\" \"b, c, d\");",
            "СтрДлина(\"a\"\n\"b, c\");",
            "НСтр(\"a, b, c\");",
            "НСтр(\"a\n|b, c, d\");",
        ] {
            let name = if src.starts_with("СтрДлина") {
                "СтрДлина"
            } else {
                "НСтр"
            };
            assert_eq!(arg_count_of(src, name), 1, "{src:?}");
        }
        // Реальный вызов из issue: три литерала плюс второй аргумент.
        let real = "НСтр(\"ru='Не удалось %1: %2, для счета:'\"\n\"%3';uk='Не вдалося %1: %2, для рахунку:'\"\n\"%3'\", ОбщегоНазначения.КодОсновногоЯзыка());";
        assert_eq!(arg_count_of(real, "НСтр"), 2);
        // Запятые в тексте запроса внутри литерала — тоже не разделители.
        assert_eq!(arg_count_of("Ф(\"ВЫБРАТЬ А, Б, В\");", "Ф"), 1);
        // Список из одних запятых — пропущенный аргумент (в выгрузке УТ есть
        // `ПоказатьПредупреждение(,)`); пустой список — ноль.
        assert_eq!(arg_count_of("Ф(,);", "Ф"), 1);
        assert_eq!(arg_count_of("Ф();", "Ф"), 0);
    }

    #[test]
    fn query_text_cast_is_not_a_call() {
        let facts = collect_facts("З = Новый Запрос(\"ВЫБРАТЬ ВЫРАЗИТЬ(Т.С КАК ЧИСЛО(15,2))\");");
        assert!(
            facts.calls.is_empty(),
            "ЧИСЛО из текста запроса: {:?}",
            facts.calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        assert_eq!(facts.news.len(), 1);
        assert_eq!(facts.news[0].type_name, "Запрос");
    }

    #[test]
    fn if_keyword_is_not_a_call() {
        let facts = collect_facts("Если Условие(1) Тогда КонецЕсли;");
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Условие"]);
    }

    #[test]
    fn constructor_is_not_a_call() {
        let facts = collect_facts("М = Новый Массив(10);");
        assert_eq!(facts.news.len(), 1);
        assert_eq!(facts.news[0].type_name, "Массив");
        assert!(facts.calls.is_empty());
    }

    #[test]
    fn enum_property_access() {
        let facts = collect_facts("Ор = ОриентацияСтраницы.Ландшафт;");
        assert_eq!(facts.dots.len(), 1);
        assert_eq!(facts.dots[0].head, "ОриентацияСтраницы");
        assert_eq!(facts.dots[0].member, "Ландшафт");
    }

    #[test]
    fn object_method_call_is_not_a_bare_call() {
        let facts = collect_facts("ТабДок.Вывести(Рез);");
        assert_eq!(facts.dots.len(), 1);
        assert_eq!(facts.dots[0].head, "ТабДок");
        assert_eq!(facts.dots[0].member, "Вывести");
        assert!(facts.calls.is_empty());
    }

    #[test]
    fn chain_prefix_dot_is_caught_before_trailing_call() {
        // `ТЗ.Колонкы.Добавить(...)`: грамматика алиасит в `property_access`
        // только весь внешний сегмент цепочки, промежуточное звено `ТЗ.Колонкы`
        // остаётся простым узлом `access` — он должен попасть в dots тоже.
        let facts = collect_facts("ТЗ.Колонкы.Добавить(\"Поле\");");
        assert!(
            facts
                .dots
                .iter()
                .any(|d| d.head == "ТЗ" && d.member == "Колонкы"),
            "нет ТЗ.Колонкы: {:?}",
            facts
                .dots
                .iter()
                .map(|d| (&d.head, &d.member))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn chain_method_link_is_caught() {
        // `Запрос.Выполнить().Выбрать()`: внутреннее звено — вызов метода, а не
        // свойство. Опечатка в нём (`Выполнть`) должна попадать в dots, иначе
        // проверка члена типа молчит там, где прежняя регулярка находку давала.
        let facts = collect_facts("Р = Запрос.Выполнть().Выбрать();");
        assert!(
            facts
                .dots
                .iter()
                .any(|d| d.head == "Запрос" && d.member == "Выполнть"),
            "нет звена Запрос.Выполнть: {:?}",
            facts
                .dots
                .iter()
                .map(|d| (&d.head, &d.member))
                .collect::<Vec<_>>()
        );
        // Голова цепочки не должна попасть в голые вызовы.
        assert!(
            facts.calls.is_empty(),
            "цепочка дала голый вызов: {:?}",
            facts.calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn simple_member_call_is_not_duplicated() {
        // Один DotFact, а не два: `call_expression` и вложенный `access`
        // не должны собрать одно и то же звено дважды.
        let facts = collect_facts("Р = Запрос.Выполнить();");
        let links: Vec<(&str, &str)> = facts
            .dots
            .iter()
            .map(|d| (d.head.as_str(), d.member.as_str()))
            .collect();
        assert_eq!(links, vec![("Запрос", "Выполнить")]);
    }

    #[test]
    fn normalize_keeps_byte_length() {
        // Позиции узлов совпадают с оригиналом только если длина не изменилась.
        for src in [
            "Х = ? (Усл, 1, 2);",
            "Попытка\n А();\nИсключение\n Если Б Тогда\n  ВызватьИсключение;\n КонецЕсли;\nКонецПопытки;",
            "Процедура СчётаУчёта()\nКонецПроцедуры",
            "Х = ?(Усл, 1, 2);", // трогать нечего
        ] {
            let n = normalize_for_parser(src);
            assert_eq!(n.len(), src.len(), "длина изменилась для: {src}");
        }
    }

    #[test]
    fn ternary_with_space_is_parsed() {
        // `? (Усл, ...)` в грамматике 0.1.7 — ERROR; после нормализации разбирается,
        // и вложенный вызов внутри тернарного оператора становится виден.
        let facts = collect_facts("Кол = ? (Стр.Свойство(\"К\"), СтрокаЧисло(Стр.К), 0);");
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"СтрокаЧисло"),
            "вызов внутри тернарного не найден: {names:?}"
        );
        assert!(facts
            .dots
            .iter()
            .any(|d| d.head == "Стр" && d.member == "Свойство"));
    }

    #[test]
    fn bare_raise_does_not_break_tree() {
        // `ВызватьИсключение;` внутри `Если` грамматика не понимает.
        let facts = collect_facts(
            "Процедура П()\n Попытка\n  А();\n Исключение\n  Если Б Тогда\n   ВызватьИсключение;\n  КонецЕсли;\n  Лог(В);\n КонецПопытки;\nКонецПроцедуры",
        );
        assert!(facts.declarations.contains("п"));
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"Лог"),
            "вызов после ВызватьИсключение потерян: {names:?}"
        );
    }

    #[test]
    fn raise_with_argument_is_untouched() {
        // Форму с аргументом грамматика понимает — нормализация её не трогает.
        let src = "Попытка\n А();\nИсключение\n ВызватьИсключение \"текст\";\nКонецПопытки;";
        assert!(matches!(
            normalize_for_parser(src),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn nbsp_indent_is_parsed() {
        // Неразрывный пробел (U+00A0) в отступе грамматика пробелом не считает.
        let src = "Процедура П()\n\u{00A0}\u{00A0}Сообщить(1);\n  Лог(2);\nКонецПроцедуры";
        assert_eq!(normalize_for_parser(src).len(), src.len());
        let facts = collect_facts(src);
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Сообщить", "Лог"]);
        assert!(facts.declarations.contains("п"));
    }

    #[test]
    fn preproc_with_space_is_parsed() {
        // `# Если` с пробелом после решётки — ERROR, `#Если` — нет.
        let src = "Процедура П()\n  Лог(1);\n# Если Клиент Тогда\n  Сообщить(2);\n#КонецЕсли\nКонецПроцедуры";
        assert_eq!(normalize_for_parser(src).len(), src.len());
        let facts = collect_facts(src);
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Лог", "Сообщить"]);
    }

    #[test]
    fn method_of_ternary_result_is_not_a_bare_call() {
        // `?(У, А, Б).ПолучитьИмена()` грамматика рвёт так, что вызов метода
        // становится отдельным оператором. Точка слева спасает от ложной находки.
        let facts = collect_facts(
            "Процедура П()\n  А = ?(У, Б, В).ПолучитьИмена();\n  Лог(3);\nКонецПроцедуры",
        );
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Лог"],
            "метод результата тернарного принят за голый вызов: {names:?}"
        );
    }

    #[test]
    fn constructor_survives_preprocessor_inside_arguments() {
        // Директива `#Если` внутри списка аргументов грамматике неизвестна: узел
        // `new_expression` разваливается, и `Новый ОписаниеТипов(...)` приходит
        // обычным вызовом. Слово `Новый` слева спасает от ложной находки.
        // Взято из `external/Выгрузка накладных в Docsinbox` (35 ложных находок на УТ).
        let src = "Процедура П()\n\
                   \x20   В = Новый Структура(\"а,б\",\n\
                   \x20       Новый ОписаниеТипов(\"Строка\"),\n\
                   #Если ВебКлиент Тогда\n\
                   \x20       Новый ОписаниеТипов(\"Массив\"),\n\
                   #КонецЕсли\n\
                   \x20   );\n\
                   КонецПроцедуры";
        let facts = collect_facts(src);
        let calls: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert!(
            calls.is_empty(),
            "конструктор принят за голый вызов: {calls:?}"
        );
        assert!(facts.news.iter().any(|n| n.type_name == "ОписаниеТипов"));
    }

    #[test]
    fn new_keyword_check_is_case_insensitive_and_word_bounded() {
        // `Обновый` — не `Новый`; регистр значения не имеет.
        let facts = collect_facts(
            "Процедура П()\n  А = НОВЫЙ Массив();\n  Б = Обновый(1);\nКонецПроцедуры",
        );
        let calls: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            calls,
            vec!["Обновый"],
            "ожидался только вызов Обновый: {calls:?}"
        );
    }

    #[test]
    fn negative_default_keeps_facts() {
        // `Процедура П(А = -1)` — минус в заголовке грамматика не принимает.
        let src =
            "Процедура П(Знач А = -1, Б = 2)\n  Сообщить(1);\n  Х = Стр.Поле;\nКонецПроцедуры";
        assert_eq!(normalize_for_parser(src).len(), src.len());
        let facts = collect_facts(src);
        assert!(facts.declarations.contains("п"));
        assert_eq!(facts.calls.len(), 1);
        assert_eq!(facts.dots.len(), 1);
    }

    #[test]
    fn negative_value_in_body_is_untouched() {
        // Минус в ТЕЛЕ процедуры грамматике понятен — нормализация его не трогает.
        let src = "Процедура П()\n  Х = -1;\n  У = А - Б;\nКонецПроцедуры";
        assert!(matches!(
            normalize_for_parser(src),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn chain_head_call_is_bare() {
        // `ПустаяСсылка().Метаданные()`: слева от головы точки нет — это голый
        // вызов. В дереве он лежит внутри `access` единственным ребёнком.
        let facts = collect_facts("Х = ПустаяСсылка().Метаданные().ПолноеИмя();");
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["ПустаяСсылка"],
            "голова цепочки должна быть голым вызовом"
        );
    }

    #[test]
    fn member_call_in_chain_is_not_bare() {
        // А `Запрос.Выполнить()` — метод объекта, голым вызовом быть не должен.
        let facts = collect_facts("Р = Запрос.Выполнить().Выбрать();");
        assert!(
            facts.calls.is_empty(),
            "лишние голые вызовы: {:?}",
            facts.calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yo_letter_does_not_break_identifiers() {
        // Грамматика не считает `ё` частью идентификатора (диапазон `а-я` её не
        // включает). Без нормализации `СчётаУчёта` рвётся на «Сч»/«таУч»/«та»,
        // объявление теряется, а обрубки становятся ложными находками.
        let facts = collect_facts(
            "Процедура СчётаУчёта()\n    ЗаполнённыеДанные();\nКонецПроцедуры\n\
             Процедура ЗаполнённыеДанные()\nКонецПроцедуры\n",
        );
        assert_eq!(
            facts.declarations,
            HashSet::from(["счётаучёта".to_string(), "заполнённыеданные".to_string()]),
            "объявления с ё: {:?}",
            facts.declarations
        );
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["ЗаполнённыеДанные"],
            "имя вызова должно сохранить ё"
        );
    }

    #[test]
    fn yo_letter_in_member_name() {
        let facts = collect_facts("Х = ЦветаСтиля.УОП_ЗелёнаяСтрока;");
        assert_eq!(facts.dots.len(), 1);
        assert_eq!(facts.dots[0].head, "ЦветаСтиля");
        assert_eq!(facts.dots[0].member, "УОП_ЗелёнаяСтрока");
    }

    #[test]
    fn declarations_and_call_inside_module() {
        let facts = collect_facts(
            "Процедура А()\n  Б();\nКонецПроцедуры\nФункция Б()\n  Возврат 1;\nКонецФункции",
        );
        assert_eq!(
            facts.declarations,
            HashSet::from(["а".to_string(), "б".to_string()])
        );
        let names: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Б"]);
    }

    #[test]
    fn mask_keeps_positions() {
        let src = "Если А = \"строка\" Тогда";
        let masked = mask_strings_and_comments(src);
        assert_eq!(masked.len(), src.len());
        // Текст вне строки сохранился.
        assert!(masked.contains("Если А ="));
        // Содержимое строки замаскировано.
        assert!(!masked.contains("строка"));
    }

    #[test]
    fn mask_handles_comment_to_eol() {
        let src = "А = 1; // комментарий\nБ = 2;";
        let masked = mask_strings_and_comments(src);
        assert!(masked.contains("А = 1;"));
        assert!(!masked.contains("комментарий"));
        assert!(masked.contains("Б = 2;"));
    }

    #[test]
    fn collect_methods_directive_and_export() {
        let methods =
            collect_methods("&НаСервере\nПроцедура Тест(А, Знач Б = 1) Экспорт\nКонецПроцедуры");
        assert_eq!(methods.len(), 1);
        let m = &methods[0];
        assert_eq!(m.name, "Тест");
        assert!(!m.is_function);
        assert!(m.is_export);
        assert_eq!(m.directive.as_deref(), Some("НаСервере"));
        assert_eq!(m.params.as_deref(), Some("(А, Знач Б = 1)"));
        assert_eq!(m.line_start, 2);
    }

    #[test]
    fn collect_methods_function_export() {
        let methods = collect_methods("Функция Ф() Экспорт\n  Возврат 1;\nКонецФункции");
        assert_eq!(methods.len(), 1);
        assert!(methods[0].is_function);
        assert!(methods[0].is_export);
    }

    #[test]
    fn collect_methods_normalizes_yo() {
        let methods = collect_methods("Процедура СчётаУчёта()\nКонецПроцедуры");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "СчётаУчёта");
    }

    #[test]
    fn collect_methods_override_directive() {
        let methods = collect_methods("&Вместо(\"Ф\")\nПроцедура Р()\nКонецПроцедуры");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].directive.as_deref(), Some("Вместо"));
    }

    #[test]
    fn assignment_to_identifier_is_collected() {
        let facts = collect_facts("Процедура Т()\nПараметры = Новый Структура;\nКонецПроцедуры\n");
        assert_eq!(
            facts.assigns.len(),
            1,
            "assigns: {:?}",
            facts.assigns.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert_eq!(facts.assigns[0].name, "Параметры");
        assert!(!facts.assigns[0].declaration);
    }

    #[test]
    fn constructor_type_is_extracted() {
        // `Новый X` во всей правой части — тип переменной известен точно,
        // даже если имя переменной совпало с именем платформенного типа.
        // Хвост `;` с комментарием типу не мешает.
        let facts = collect_facts(
            "Процедура Т()\nЗапрос = Новый HTTPЗапрос(\"/\");\nМассив = Новый Массив ; // создаём\nСтр = Новый Структура(\"а)b\");\nКонецПроцедуры\n",
        );
        let by_name = |n: &str| {
            facts
                .assigns
                .iter()
                .find(|a| a.name == n)
                .and_then(|a| a.new_type.clone())
        };
        assert_eq!(by_name("Запрос").as_deref(), Some("HTTPЗапрос"));
        assert_eq!(by_name("Массив").as_deref(), Some("Массив"));
        assert_eq!(by_name("Стр").as_deref(), Some("Структура"));
    }

    #[test]
    fn constructor_not_the_whole_right_side_gives_no_type() {
        // Конструктор внутри выражения, под вызовом или в сумме типом переменной
        // не является: `Х = Новый Массив().Количество()` — это Число, а не Массив.
        let facts = collect_facts(
            "Процедура Т()\nА = ?(У, Новый Массив, Неопределено);\nБ = Новый Массив().Количество();\nВ = Новый Массив + Чтото;\nКонецПроцедуры\n",
        );
        let types: Vec<(&str, &Option<String>)> = facts
            .assigns
            .iter()
            .map(|a| (a.name.as_str(), &a.new_type))
            .collect();
        assert!(
            facts.assigns.iter().all(|a| a.new_type.is_none()),
            "тип не должен выводиться: {types:?}"
        );
    }

    #[test]
    fn var_declaration_has_no_constructor_type() {
        let facts = collect_facts("Перем Кэш Экспорт;\n");
        assert_eq!(facts.assigns.len(), 1, "assigns: {:?}", facts.assigns.len());
        assert!(facts.assigns[0].declaration);
        assert!(facts.assigns[0].new_type.is_none());
    }

    #[test]
    fn assignment_to_member_is_not_collected() {
        let facts =
            collect_facts("Процедура Т()\nЭлементы.Список.Видимость = Ложь;\nКонецПроцедуры\n");
        assert!(
            facts.assigns.is_empty(),
            "assigns: {:?}",
            facts.assigns.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn var_statement_is_collected() {
        let facts = collect_facts("Процедура Т()\nПерем А, Элементы;\nКонецПроцедуры\n");
        assert_eq!(
            facts.assigns.len(),
            2,
            "assigns: {:?}",
            facts.assigns.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert!(facts.assigns.iter().all(|a| a.declaration));
        let names: Vec<&str> = facts.assigns.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["А", "Элементы"]);
    }

    #[test]
    fn proc_scopes_carry_directive_and_params() {
        // Первая процедура — без контекста формы, у второй параметр `Параметры`.
        // Оба признака нужны проверке `ShadowedContextName`, чтобы не ругаться
        // на законный код.
        let src = "\
&НаКлиентеНаСервереБезКонтекста
Процедура УстановитьДоступность(Форма)
Элементы = Форма.Элементы;
КонецПроцедуры

&НаКлиенте
Процедура Обработчик(Знач Результат, Параметры)
Параметры = Новый Структура;
КонецПроцедуры
";
        let facts = collect_facts(src);
        assert!(facts.has_directives);
        assert_eq!(facts.procs.len(), 2);

        let elements = facts
            .assigns
            .iter()
            .find(|a| a.name == "Элементы")
            .expect("присваивание Элементы собрано");
        let scope = facts
            .procs
            .iter()
            .find(|p| p.contains(elements.byte))
            .expect("процедура найдена по байту");
        assert!(scope.no_context, "директива БезКонтекста распознана");

        let params = facts
            .assigns
            .iter()
            .find(|a| a.name == "Параметры")
            .expect("присваивание Параметры собрано");
        let scope = facts
            .procs
            .iter()
            .find(|p| p.contains(params.byte))
            .expect("процедура найдена по байту");
        assert!(!scope.no_context);
        assert!(scope.params.contains("параметры"), "{:?}", scope.params);
        assert!(scope.params.contains("результат"), "{:?}", scope.params);
    }

    #[test]
    fn ordinary_form_module_has_no_directives() {
        // Модуль обычной (неуправляемой) формы: директив компиляции нет вовсе.
        let facts = collect_facts("Процедура КнопкаНажатие(Элемент)\nКонецПроцедуры\n");
        assert!(!facts.has_directives);
    }

    #[test]
    fn module_level_var_definition_is_collected() {
        let facts = collect_facts("Перем Кэш Экспорт;\n");
        assert_eq!(
            facts.assigns.len(),
            1,
            "assigns: {:?}",
            facts.assigns.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert_eq!(facts.assigns[0].name, "Кэш");
        assert!(facts.assigns[0].declaration);
    }

    // ── Тексты запросов ───────────────────────────────────────────────────

    #[test]
    fn multiline_query_drops_continuation_bars() {
        let src = "Запрос.Текст = \"ВЫБРАТЬ\n\t|\tТовары.Ссылка\n\t|ИЗ\n\t|\tСправочник.Товары КАК Товары\";";
        let queries = collect_query_texts(src);
        assert_eq!(queries.len(), 1, "запрос не найден");
        assert_eq!(
            queries[0].text,
            "ВЫБРАТЬ\n\tТовары.Ссылка\nИЗ\n\tСправочник.Товары КАК Товары"
        );
    }

    #[test]
    fn offsets_point_back_into_the_module() {
        let src = "Запрос.Текст = \"ВЫБРАТЬ\n\t|\tТовары.Ссылка\n\t|ИЗ\n\t|\tСправочник.Товары КАК Товары\";";
        let queries = collect_query_texts(src);
        let query = &queries[0];

        // Каждое ключевое слово должно указывать на своё место в модуле.
        for word in ["ВЫБРАТЬ", "ИЗ", "Справочник.Товары"] {
            let in_text = query
                .text
                .find(word)
                .expect("слово потеряно в тексте запроса");
            let in_module = query.map_offset(in_text);
            assert!(
                src[in_module..].starts_with(word),
                "смещение для {word} указывает не туда: {:?}",
                &src[in_module..(in_module + 20).min(src.len())]
            );
        }
    }

    #[test]
    fn concatenation_of_literals_is_assembled() {
        let src = "Текст = \"ВЫБРАТЬ Т.Ссылка \"\n\t+ \"ИЗ Справочник.Товары КАК Т\";";
        let queries = collect_query_texts(src);
        assert_eq!(queries.len(), 1, "склейка литералов не сработала");
        assert_eq!(
            queries[0].text,
            "ВЫБРАТЬ Т.Ссылка ИЗ Справочник.Товары КАК Т"
        );

        let at_from = queries[0].text.find("ИЗ").unwrap();
        assert!(src[queries[0].map_offset(at_from)..].starts_with("ИЗ"));
    }

    #[test]
    fn concatenation_with_variable_is_skipped() {
        // Часть текста вычисляется — разбирать нечего, находки были бы на
        // месте, которого в запросе нет.
        let src = "Текст = \"ВЫБРАТЬ \" + ИмяПоля + \" ИЗ Справочник.Товары КАК Т\";";
        assert!(collect_query_texts(src).is_empty());
    }

    #[test]
    fn plain_strings_are_not_queries() {
        let src = "Сообщить(\"Не удалось выбрать элемент\");\nТ = \"ИЗ отчёта\";";
        assert!(collect_query_texts(src).is_empty());
    }

    #[test]
    fn query_in_constructor_argument_is_found() {
        let queries = collect_query_texts("З = Новый Запрос(\"ВЫБРАТЬ 1\");");
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].text, "ВЫБРАТЬ 1");
    }

    #[test]
    fn doubled_quote_becomes_single() {
        let queries = collect_query_texts("Т = \"ВЫБРАТЬ \"\"Да\"\" КАК Флаг\";");
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].text, "ВЫБРАТЬ \"Да\" КАК Флаг");
    }

    #[test]
    fn comment_between_continuation_lines_is_dropped() {
        // Комментарий между строками-продолжениями литерал не закрывает и в
        // значение не входит.
        let src = "Т = \"ВЫБРАТЬ\n\t// временно\n\t|\t1\";";
        let queries = collect_query_texts(src);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].text, "ВЫБРАТЬ\n\t1");
    }

    #[test]
    fn query_inside_deleted_block_is_ignored() {
        // Код блока `#Удаление` в конфигурацию не попадает.
        let src = "#Удаление\nТ = \"ВЫБРАТЬ 1\";\n#КонецУдаления\n";
        assert!(collect_query_texts(src).is_empty());
    }

    #[test]
    fn two_queries_side_by_side() {
        let src = "А = \"ВЫБРАТЬ 1\";\nБ = \"ВЫБРАТЬ 2\";";
        let queries = collect_query_texts(src);
        assert_eq!(
            queries.len(),
            2,
            "соседние запросы склеились или потерялись"
        );
        assert_eq!(queries[0].text, "ВЫБРАТЬ 1");
        assert_eq!(queries[1].text, "ВЫБРАТЬ 2");
    }

    // ── Регрессии аудита ─────────────────────────────────────────────────

    /// Директива не теряется, если между ней и объявлением стоит комментарий
    /// (на корпусе УТ — сотни таких мест).
    #[test]
    fn directive_with_comment_between_is_seen() {
        let src = "&НаКлиентеНаСервереБезКонтекста\n// пояснение\nПроцедура П()\nЭлементы = 1;\nКонецПроцедуры";
        let facts = collect_facts(src);
        assert!(facts.has_directives);
        assert_eq!(facts.procs.len(), 1);
        assert!(
            facts.procs[0].no_context,
            "директива потеряна из-за комментария между ней и объявлением"
        );
    }

    /// Стековые директивы расширений: контекстная (`&НаКлиенте`) важнее
    /// ближайшей `&Вместо("…")`.
    #[test]
    fn stacked_directives_prefer_context() {
        let methods = collect_methods("&НаКлиенте\n&Вместо(\"Х\")\nПроцедура Р()\nКонецПроцедуры");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].directive.as_deref(), Some("НаКлиенте"));
    }

    /// CRLF: в значение многострочного литерала входит только `\n`.
    #[test]
    fn crlf_multiline_query_has_lf_only() {
        let src = "Т = \"ВЫБРАТЬ\r\n\t|\t1\";";
        let queries = collect_query_texts(src);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].text, "ВЫБРАТЬ\n\t1");
    }

    /// Комментарий между `+` и литералом не делает текст статическим: часть
    /// выражения вычисляется, запрос не возвращается.
    #[test]
    fn plus_separated_by_comment_is_dirty() {
        assert!(collect_query_texts("Т = X + // c\n\"ВЫБРАТЬ 1\";").is_empty());
        assert!(collect_query_texts("Т = X + \"//в строке\" + // c\n\"ВЫБРАТЬ 1\";").is_empty());
    }

    /// Префиксное совпадение с `#Удаление` не должно затирать остаток файла.
    #[test]
    fn deletion_prefix_is_not_a_marker() {
        let src = "#УдалениеВременныхТаблиц\nТ = \"ВЫБРАТЬ 1\";\n";
        assert!(!strip_extension_directives(src).trim().is_empty());
        assert_eq!(collect_query_texts(src).len(), 1);
    }

    /// Запрос может начинаться с `//`-комментария языка запросов.
    #[test]
    fn query_starting_with_comment_is_found() {
        let queries = collect_query_texts("Т = \"// c\nВЫБРАТЬ 1\";");
        assert_eq!(queries.len(), 1);
        assert!(queries[0].text.starts_with("// c"));
    }

    /// Разделитель после ключевого слова — не только пробел: в корпусе
    /// встречается `Функция\tИмя()`.
    #[test]
    fn scan_declarations_accepts_tab_separator() {
        let names = scan_declarations("Процедура\tТаб()\nКонецПроцедуры");
        assert!(names.contains("таб"), "{names:?}");
    }

    /// BOM в первой строке не скрывает объявление (trim_start его не снимает).
    #[test]
    fn scan_declarations_sees_through_bom() {
        let names = scan_declarations("\u{FEFF}Процедура Моя()\nКонецПроцедуры");
        assert!(names.contains("моя"), "{names:?}");
    }

    /// BOM в первой строке не отменяет блочную директиву `#Удаление`.
    #[test]
    fn strip_extension_sees_through_bom() {
        let src = "\u{FEFF}#Удаление\nТ = \"ВЫБРАТЬ 1\";\n#КонецУдаления\n";
        assert!(collect_query_texts(src).is_empty());
    }
}
