//! TOML-конфиг сервера. Минимальная схема под Phase 0; в Phase 1+ добавятся
//! поля для кеша индекса и других опций.

use std::path::{Path, PathBuf};

use bsl_validator::Profile;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Адрес для bind. По умолчанию loopback — наружу не торчим.
    pub host: String,

    /// Порт MCP-сервера. 8007 свободен после декомиссии bsl-platform-context (карточка #252).
    pub port: u16,

    /// Каталог установки 1С с файлом shcntx_ru.hbk внутри.
    /// На корпоративных машинах часто стоит несколько версий платформы
    /// (`C:\Program Files\1cv8\8.3.25.1257`, `\8.3.27.1786`, ...). Сервер
    /// автоматически НЕ выбирает — пользователь обязан явно указать каталог
    /// нужной версии, иначе на загрузке индекса будет понятная ошибка.
    pub platform_path: Option<PathBuf>,

    /// Каталог логов (service.YYYY-MM-DD.log + stdout/stderr — последние пишет run.bat).
    pub log_dir: PathBuf,

    /// Фильтр tracing — `info`, `debug`, или полный EnvFilter-выражение.
    pub log_level: String,

    /// Дефолтный уровень для `validate_module`, если клиент не передал параметр.
    ///
    /// `1` — статический анализ ссылок с явным именем типа в исходнике (низкий шум,
    /// безопасный дефолт). `2` — дополнительно локальный type inference в пределах
    /// процедуры (Phase 8 MVP — `Новый ТипX`, `ТипY.ЗначениеZ`, `// @type ТипX`).
    /// `3` — дополнительно return-type tracking (Уровень 2.5): тип переменной из
    /// возвращаемого типа метода/свойства, цепочки `Запрос.Выполнить().Выбрать()`,
    /// и реквизиты справочников/документов из метаданных конфигурации (при заданном
    /// `base`). Чем выше уровень — тем больше находок и потенциальных false-positive.
    ///
    /// Значение клампится в `[1..=3]` на чтении.
    pub default_validation_level: u8,

    /// Дефолтный профиль потребителя для `validate_module`, если клиент не
    /// передал параметр `profile` (карточка-decision #1230).
    ///
    /// `full` (дефолт) — все находки, `level` из параметра/конфига; рассчитан на
    /// сильную модель, которая сама отбросит сомнительные. `strict` — только
    /// high-confidence находки и форсированный `level=1`; для слабых моделей
    /// (LibreChat/DeepSeek), чтобы ложное срабатывание не приводило к зацикливанию.
    pub default_profile: Profile,

    /// Разрешённые значения заголовка `Host` для входящих запросов к `/mcp`
    /// (защита rmcp от DNS-rebinding). По умолчанию — только loopback.
    ///
    /// При сетевом деплое (`host = "0.0.0.0"`) сюда нужно добавить адрес, по
    /// которому клиенты обращаются к серверу (например, IP/имя хоста сервера),
    /// иначе rmcp вернёт `403 Forbidden: Host header is not allowed`. Запись без
    /// порта разрешает любой порт этого хоста.
    pub allowed_hosts: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8007,
            platform_path: None,
            log_dir: PathBuf::from(r"C:\bsl-context-rs\logs"),
            log_level: "info".to_string(),
            default_validation_level: 1,
            default_profile: Profile::Full,
            allowed_hosts: vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string(),
            ],
        }
    }
}

impl Config {
    /// Загрузить конфиг из файла, либо вернуть дефолт.
    pub fn load_or_default(path: Option<&Path>) -> anyhow::Result<Self> {
        let Some(path) = path else { return Ok(Self::default()) };
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read config {}: {}", path.display(), e))?;
        let mut cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse config {}: {}", path.display(), e))?;
        // Кламп уровня в безопасный диапазон, чтобы конфиг с опечаткой
        // (`level = 5`) не валил сервер и не приводил к скрытым ошибкам.
        cfg.default_validation_level = cfg.default_validation_level.clamp(1, 3);
        Ok(cfg)
    }
}
