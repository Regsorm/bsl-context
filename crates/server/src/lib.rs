//! Публичная библиотека сервера: экспорт MCP-handler'а и конфига для тестов
//! и embedding-сценариев. Бинарь `bsl-context-rs` использует эти же модули.

pub mod config;
pub mod http;
pub mod mcp_server;
pub mod pid_lock;
pub mod sources;
