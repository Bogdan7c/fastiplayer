//! `hello` плеера для посредника и экстеншена (решение 86: договорились — значит строго).

use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, Hello, PeerCapabilities, SoftwareVersion, ValueViolation,
};

/// Версия программы, которую видит экстеншен (диагностика и подсказки «обновите»).
const PLAYER_SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Собирает `hello` плеера.
///
/// Возможности пока пустые: дешёвые кодеки (`cheap_codec.*`, решение 43) зависят от
/// выбранного рендера и декодера, их заполнит сессия 13; геометрия режима 3 — 17–19.
/// Пустой список честно говорит экстеншену «ограничений нет, фич сверх базы нет».
///
/// # Errors
/// Версия пакета не прошла проверку протокола. Это ошибка сборки (закреплено
/// тестом ниже), поэтому процесс не стартует, а не подставляет выдуманную версию.
pub(super) fn player_hello() -> Result<Hello, ValueViolation> {
    Ok(Hello {
        protocol: CURRENT_PROTOCOL_VERSION,
        software_version: SoftwareVersion::new(PLAYER_SOFTWARE_VERSION)?,
        capabilities: PeerCapabilities::default().to_tokens(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_hello_carries_package_version_current_protocol_and_no_capabilities() {
        let hello = player_hello().expect("package version must be a valid protocol version");
        assert_eq!(hello.software_version.as_str(), PLAYER_SOFTWARE_VERSION);
        assert_eq!(hello.protocol, CURRENT_PROTOCOL_VERSION);
        assert!(hello.capabilities.is_empty());
    }
}
