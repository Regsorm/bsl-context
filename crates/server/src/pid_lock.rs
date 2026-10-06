//! Singleton-защита через PID-lock.
//!
//! Гарантирует, что у `bsl-context-rs` ровно один процесс на машине. Без этого
//! второй экземпляр успевает 5 секунд крутить cold-start (парсинг hbk),
//! расходовать RAM/CPU, и только потом упасть на `bind 10048`. С этим locks
//! второй экземпляр выходит до загрузки индекса с понятным сообщением о PID
//! уже работающего инстанса.
//!
//! Порт из `code-index/crates/code-index-core/src/daemon_core/lock.rs` с двумя
//! отличиями:
//!
//! 1. дополнительно сверяем имя процесса (карточка #2424 — на Windows ОС
//!    переиспользует PID после reboot, проверка только PID даёт ложное «уже
//!    запущен»);
//! 2. записанный PID, равный НАШЕМУ собственному, считается протухшим.
//!
//! Второе отличие — про контейнер. Внутри контейнера сервис всегда PID 1, а
//! файл лежит в примонтированном с хоста каталоге логов и переживает
//! пересоздание контейнера. Убили процесс жёстко (`docker kill`, OOM,
//! перезагрузка хоста) — `Drop` не отработал, в файле остался «1». Новый
//! процесс тоже PID 1, имя своё же, и проверка «жив ли PID 1 с нашим именем»
//! отвечает «да» — замок ловит сам себя, и сервис не поднимется НИКОГДА.
//! Ровно так bsl-context простоял с 20.08.2026 (4767 перезапусков подряд), и
//! раньше 21–22.07.2026. Живой предшественник не может иметь тот же PID, что и
//! мы, поэтому совпадение с `std::process::id()` — однозначный признак
//! протухшего файла.
//!
//! Файл-лок — `<log_dir>/bsl-context-rs.pid` (`log_dir` уже создаётся `run.bat`,
//! значит каталог гарантированно существует).
//!
//! Известное ограничение: владелец определяется по PID и имени процесса, а не
//! по дескриптору файла. Если в файле окажется PID живого экземпляра
//! `bsl-context-rs`, не владеющего замком (например, stdio-сессия — она замок
//! не берёт), запуск HTTP-режима будет отклонён до ручного удаления файла.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

const PID_FILE_NAME: &str = "bsl-context-rs.pid";
/// Голое имя процесса (без расширения): сверка идёт по префиксу с границей
/// слова, чтобы распознавать и `bsl-context-rs.exe`, и переименованные сборки
/// с суффиксом (`bsl-context-rs-v2`), но не `my-bsl-context-rs-tool`.
const EXPECTED_PROC_STEM: &str = "bsl-context-rs";
/// Сколько раз пробуем разобрать замок, пока за него конкурируют старты.
const ACQUIRE_ATTEMPTS: usize = 5;
const RETRY_DELAY: Duration = Duration::from_millis(30);

/// RAII-guard PID-lock. Удаляет файл в `Drop`.
pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Захватить PID-lock в каталоге `log_dir`.
    ///
    /// Захват атомарен (`O_EXCL`): файл создаёт ровно один процесс, остальные
    /// видят `AlreadyExists` и разбирают владельца. Живой процесс с нашим
    /// именем — отказ «уже запущен»; мёртвый PID, чужое имя (переиспользование
    /// PID) или наш собственный PID (контейнер после жёсткого kill) — файл
    /// снимается и пересоздаётся.
    pub fn acquire(log_dir: &Path) -> Result<Self> {
        let pid_path = log_dir.join(PID_FILE_NAME);

        for _ in 0..ACQUIRE_ATTEMPTS {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pid_path)
            {
                // Файл создан атомарно: владелец — мы.
                Ok(mut file) => {
                    use std::io::Write;
                    let own_pid = std::process::id().to_string();
                    file.write_all(own_pid.as_bytes())
                        .with_context(|| format!("запись PID в {}", pid_path.display()))?;
                    file.sync_all()
                        .with_context(|| format!("fsync PID-файла {}", pid_path.display()))?;
                    // Read-back: пока писали, конкурент мог снять и пересоздать
                    // файл — тогда владелец не мы, и стартовать нельзя.
                    let check = std::fs::read_to_string(&pid_path).unwrap_or_default();
                    if check.trim() == own_pid {
                        return Ok(Self { path: pid_path });
                    }
                    bail!(
                        "PID-файл {} перехвачен другим процессом при захвате",
                        pid_path.display()
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let content = match std::fs::read_to_string(&pid_path) {
                        Ok(content) => content,
                        Err(e) => {
                            bail!("не удалось прочитать PID-файл {}: {e}", pid_path.display());
                        }
                    };
                    if let Ok(pid) = content.trim().parse::<u32>() {
                        match probe_owner(pid) {
                            Owner::Busy(pid) => bail!(
                                "Сервис bsl-context-rs уже запущен (PID {pid}). PID-файл: {}. \
                                 Если это ошибочное срабатывание — удалите файл или дождитесь \
                                 его автоудаления при graceful shutdown.",
                                pid_path.display()
                            ),
                            // PID жив, но имя не прочиталось: не перезаписываем,
                            // иначе рискуем запустить второй экземпляр.
                            Owner::Unknown(pid) => bail!(
                                "PID-файл {} принадлежит живому процессу PID {pid}, но определить \
                                 его имя не удалось — перезапись запрещена. Если процесс не \
                                 относится к bsl-context-rs, удалите файл вручную.",
                                pid_path.display()
                            ),
                            Owner::Stale => {}
                        }
                    }
                    // Перечитываем и сверяем содержимое прямо перед снятием:
                    // конкурент мог успеть снять файл и создать СВОЙ живой замок
                    // между проверкой и удалением — чужой файл трогать нельзя.
                    // Переименование в приватное имя атомарно «забирает» файл
                    // у конкурентов: loser получит NotFound и начнёт захват заново.
                    let current = std::fs::read_to_string(&pid_path).unwrap_or_default();
                    if current.trim() != content.trim() {
                        std::thread::sleep(RETRY_DELAY);
                        continue;
                    }
                    let stale_name = unique_sidecar_name(&pid_path, "stale");
                    match std::fs::rename(&pid_path, &stale_name) {
                        Ok(()) => {
                            let _ = std::fs::remove_file(&stale_name);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            bail!(
                                "не удалось снять устаревший PID-файл {}: {e}",
                                pid_path.display()
                            );
                        }
                    }
                    tracing::warn!(
                        pid_file = %pid_path.display(),
                        "найден устаревший/нечитаемый PID-файл — пересоздаём"
                    );
                    std::thread::sleep(RETRY_DELAY);
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("создание PID-файла {}", pid_path.display()));
                }
            }
        }
        bail!(
            "не удалось захватить PID-файл {} за {ACQUIRE_ATTEMPTS} попыток",
            pid_path.display()
        );
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        // Удаляем только СВОЙ замок: файл мог перезаписать другой процесс
        // (гонка/PID reuse), и снимать чужой живой замок нельзя. Если
        // содержимое не подтверждено — безопаснее оставить файл.
        let own_pid = std::process::id().to_string();
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if content.trim() == own_pid {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Состояние владельца PID-файла.
enum Owner {
    /// Файл протух: PID мёртв, переиспользован посторонним процессом или это
    /// наш собственный PID (контейнер: и старый, и новый процесс — PID 1).
    Stale,
    /// Живой процесс с нашим именем — второй экземпляр.
    Busy(u32),
    /// PID жив, но имя процесса недоступно — не трогаем (fail-closed).
    Unknown(u32),
}

/// Проверить владельца PID-файла: жив ли процесс и наш ли он.
///
/// Имя сверяется по префиксу с границей слова (`bsl-context-rs`,
/// `bsl-context-rs.exe`, `bsl-context-rs-v2`), а не строгим равенством:
/// sysinfo на разных ОС отдаёт имя то с `.exe`, то без, а сборки могут иметь
/// суффиксы. Если имя прочитать не удалось — ответ `Unknown`, и замок не
/// перезаписывается: ложный отказ безопаснее второго экземпляра.
fn probe_owner(pid: u32) -> Owner {
    use sysinfo::{Pid, ProcessesToUpdate, System};

    if pid == std::process::id() {
        return Owner::Stale;
    }

    let mut sys = System::new();
    let spid = Pid::from(pid as usize);
    sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), false);

    let Some(proc) = sys.process(spid) else {
        return Owner::Stale;
    };
    let name = proc.name().to_string_lossy().to_lowercase();
    if name.is_empty() {
        return Owner::Unknown(pid);
    }
    if is_our_name(&name) {
        Owner::Busy(pid)
    } else {
        // PID переиспользован посторонним процессом — файл можно снять.
        Owner::Stale
    }
}

/// Имя процесса — это наш бинарь? Сверка с границей, а не по вхождению:
/// `my-bsl-context-rs-tool.exe` не должен блокировать старт, а переименованные
/// сборки (`bsl-context-rs-v2.exe`) — распознаваться.
fn is_our_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n == EXPECTED_PROC_STEM {
        return true;
    }
    match n.strip_prefix(EXPECTED_PROC_STEM) {
        Some(rest) => rest.starts_with(['.', '-', ' ']),
        None => false,
    }
}

/// Уникальное имя файла-отвода для атомарного снятия stale-замка.
fn unique_sidecar_name(path: &Path, tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{tag}.{}.{}", std::process::id(), n));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Файл с НАШИМ собственным PID — протухший, а не «уже запущен».
    ///
    /// Это случай контейнера: и предыдущий, и новый процесс — PID 1. До правки
    /// сервис не поднимался вовсе.
    #[test]
    fn own_pid_in_file_is_stale() {
        let dir = tempfile::tempdir().expect("временный каталог");
        std::fs::write(
            dir.path().join(PID_FILE_NAME),
            std::process::id().to_string(),
        )
        .expect("запись PID-файла");

        let lock = PidLock::acquire(dir.path());
        assert!(lock.is_ok(), "свой же PID должен считаться протухшим");
    }

    /// Мёртвый PID тоже перезаписывается — прежнее поведение не сломано.
    #[test]
    fn dead_pid_is_stale() {
        let dir = tempfile::tempdir().expect("временный каталог");
        // PID заведомо не занят: максимум для Linux — 4194304, для Windows PID
        // кратен 4 и таких больших значений на практике не бывает.
        std::fs::write(dir.path().join(PID_FILE_NAME), "4194303").expect("запись PID-файла");

        assert!(PidLock::acquire(dir.path()).is_ok());
    }

    /// После `Drop` файл удаляется — следующий старт видит чистый каталог.
    #[test]
    fn drop_removes_file() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let pid_path = dir.path().join(PID_FILE_NAME);

        {
            let _lock = PidLock::acquire(dir.path()).expect("захват замка");
            assert!(pid_path.exists(), "файл создан на время работы");
        }

        assert!(!pid_path.exists(), "файл удалён при graceful shutdown");
    }

    /// Чужой замок наш `Drop` не трогает: файл перезаписан другим процессом —
    /// значит, владелец уже не мы.
    #[test]
    fn drop_does_not_remove_foreign_lock() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let pid_path = dir.path().join(PID_FILE_NAME);
        {
            let _lock = PidLock::acquire(dir.path()).expect("захват замка");
            std::fs::write(&pid_path, "4194303").expect("подмена владельца");
        }
        assert!(
            pid_path.exists(),
            "Drop обязан трогать только свой PID-файл"
        );
    }

    /// Захват публикует ровно свой PID (read-back внутри acquire).
    #[test]
    fn acquisition_creates_file_with_own_pid() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let lock = PidLock::acquire(dir.path()).expect("захват");
        let content = std::fs::read_to_string(dir.path().join(PID_FILE_NAME)).expect("чтение");
        assert_eq!(content.trim(), std::process::id().to_string());
        drop(lock);
    }

    /// Сверка имени процесса — по границе слова, а не по вхождению: посторонний
    /// `my-bsl-context-rs-tool.exe` не должен блокировать старт.
    #[test]
    fn process_name_matching_is_boundary_aware() {
        assert!(is_our_name("bsl-context-rs"));
        assert!(is_our_name("bsl-context-rs.exe"));
        assert!(is_our_name("bsl-context-rs-v2.exe"));
        assert!(is_our_name("bsl-context-rs (1).exe"));
        assert!(!is_our_name("my-bsl-context-rs-tool.exe"));
        assert!(!is_our_name("other.exe"));
    }
}
