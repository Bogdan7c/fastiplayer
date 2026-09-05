//! Единая политика concrete HTTP client-а для всех source-core путей.
//!
//! Модуль отделяет публичную identity приложения от secret request material.
//! Providers могут переопределить `User-Agent` явным request header-ом, но
//! обычный public URL никогда не уходит в сеть без идентифицируемого клиента.

use reqwest::blocking::{Client as BlockingClient, ClientBuilder as BlockingClientBuilder};
use reqwest::{Client as AsyncClient, ClientBuilder as AsyncClientBuilder};

use crate::SourceRuntimeConfig;

/// Публичная identity Fastiplayer и контактный URL проекта для HTTP-серверов.
const FASTIPLAYER_HTTP_USER_AGENT: &str = concat!(
    "fastiplayer/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/Bogdan7c/fastiplayer)"
);

/// Создаёт общий Reqwest builder с source-owned timeout и identity policy.
///
/// Redirect и cookie policy остаются у конкретного session owner-а: этот
/// helper задаёт только инварианты, общие для initial, Range и adaptive I/O.
pub(crate) fn blocking_http_client_builder(
    source_config: &SourceRuntimeConfig,
) -> BlockingClientBuilder {
    BlockingClient::builder()
        .user_agent(FASTIPLAYER_HTTP_USER_AGENT)
        .connect_timeout(source_config.connect_timeout())
        .timeout(source_config.read_timeout())
}

/// Создаёт async Reqwest builder с source-owned identity/connect policy.
///
/// Отдельный transport frontend нужен только там, где lifecycle request-а должен
/// завершаться через drop future, а не ждать конца blocking socket read-а.
/// Body timeout намеренно остаётся у конкретной async операции: reqwest total
/// timeout продолжает тикать, даже когда bounded consumer законно не poll-ит
/// streaming body из-за backpressure.
pub(crate) fn async_http_client_builder(source_config: &SourceRuntimeConfig) -> AsyncClientBuilder {
    AsyncClient::builder()
        .user_agent(FASTIPLAYER_HTTP_USER_AGENT)
        .connect_timeout(source_config.connect_timeout())
}

#[cfg(test)]
mod tests {
    use super::FASTIPLAYER_HTTP_USER_AGENT;

    /// Identity должна содержать product, version и стабильный contact URL.
    #[test]
    fn default_user_agent_is_descriptive_and_contactable() {
        assert!(FASTIPLAYER_HTTP_USER_AGENT.starts_with("fastiplayer/"));
        assert!(FASTIPLAYER_HTTP_USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(FASTIPLAYER_HTTP_USER_AGENT.contains("https://github.com/Bogdan7c/fastiplayer"));
        assert!(!FASTIPLAYER_HTTP_USER_AGENT.contains('\n'));
        assert!(!FASTIPLAYER_HTTP_USER_AGENT.contains('\r'));
    }
}
