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

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

const PID_FILE_NAME: &str = "bsl-context-rs.pid";
const EXPECTED_PROC_NAME: &str = "bsl-context-rs.exe";

/// RAII-guard PID-lock. Удаляет файл в `Drop`.
pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Захватить PID-lock в каталоге `log_dir`. Если файл существует и процесс
    /// с записанным PID жив И его имя совпадает с `bsl-context-rs.exe` —
    /// возвращается ошибка с указанием PID. Stale-файл (мёртвый PID, чужое имя
    /// процесса или наш собственный PID) перезаписывается.
    pub fn acquire(log_dir: &Path) -> Result<Self> {
        let pid_path = log_dir.join(PID_FILE_NAME);

        if pid_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    if pid != std::process::id() && is_our_process_alive(pid) {
                        bail!(
                            "Сервис bsl-context-rs уже запущен (PID {}). PID-файл: {}. \
                             Если это ошибочное срабатывание — удалите файл или дождитесь его \
                             автоудаления при graceful shutdown.",
                            pid,
                            pid_path.display()
                        );
                    }
                }
            }
            tracing::warn!(
                "найден устаревший PID-файл {} — перезаписываем",
                pid_path.display()
            );
        }

        std::fs::write(&pid_path, std::process::id().to_string())?;
        Ok(Self { path: pid_path })
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        // Удаление лучшее усилие — если упал не мы, а ОС, ничего не поделать.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Проверить, что процесс `pid` жив И его имя совпадает с `bsl-context-rs.exe`.
///
/// Двойная проверка нужна потому, что Windows переиспользует PID после
/// перезагрузки. Если PID совпал, но имя exe другое — это чужой процесс,
/// stale-файл, можем перезаписать.
fn is_our_process_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let mut sys = System::new();
    let spid = Pid::from(pid as usize);
    sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), false);

    let Some(proc) = sys.process(spid) else {
        return false;
    };
    let name = proc.name().to_string_lossy().to_lowercase();
    name == EXPECTED_PROC_NAME.to_lowercase()
        // На случай, если sysinfo вернёт имя без расширения — допускаем «голое» имя.
        || name == EXPECTED_PROC_NAME.trim_end_matches(".exe").to_lowercase()
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
}
