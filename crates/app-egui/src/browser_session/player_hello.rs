//! `hello` плеера для посредника и экстеншена (решение 86: договорились — значит строго).

use std::collections::BTreeSet;

use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, Hello, PeerCapabilities, SoftwareVersion, ValueViolation,
    VideoCodecFamily,
};

/// Версия программы, которую видит экстеншен (диагностика и подсказки «обновите»).
const PLAYER_SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Собирает `hello` плеера.
///
/// Возможности: «дешёвые» кодеки (`cheap_codec.*`, решение 43) — готовая метка из
/// `codec_label` (настройка приоритета кодеков и профиль рендера); геометрия режима 3 —
/// сессии 17–19. Пустая метка честно говорит экстеншену «ограничений нет».
///
/// # Errors
/// Версия пакета не прошла проверку протокола. Это ошибка сборки (закреплено
/// тестом ниже), поэтому процесс не стартует, а не подставляет выдуманную версию.
pub(super) fn player_hello(
    cheap_video_codecs: BTreeSet<VideoCodecFamily>,
) -> Result<Hello, ValueViolation> {
    let capabilities = PeerCapabilities {
        cheap_video_codecs,
        ..PeerCapabilities::default()
    };
    Ok(Hello {
        protocol: CURRENT_PROTOCOL_VERSION,
        software_version: SoftwareVersion::new(PLAYER_SOFTWARE_VERSION)?,
        capabilities: capabilities.to_tokens(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_hello_carries_package_version_and_current_protocol() {
        let hello = player_hello(BTreeSet::new())
            .expect("package version must be a valid protocol version");
        assert_eq!(hello.software_version.as_str(), PLAYER_SOFTWARE_VERSION);
        assert_eq!(hello.protocol, CURRENT_PROTOCOL_VERSION);
    }

    #[test]
    fn empty_label_declares_no_capabilities() {
        let hello = player_hello(BTreeSet::new()).expect("valid hello");
        assert!(hello.capabilities.is_empty());
    }

    #[test]
    fn declared_cheap_codecs_travel_as_tokens_and_survive_the_extension_side_parse() {
        let label = BTreeSet::from([VideoCodecFamily::H264, VideoCodecFamily::Vp9]);
        let hello = player_hello(label.clone()).expect("valid hello");
        let tokens: Vec<&str> = hello
            .capabilities
            .iter()
            .map(|token| token.as_str())
            .collect();
        assert_eq!(tokens, vec!["cheap_codec.h264", "cheap_codec.vp9"]);
        let parsed = PeerCapabilities::from_tokens(&hello.capabilities);
        assert_eq!(parsed.capabilities.cheap_video_codecs, label);
        assert_eq!(parsed.ignored_unknown_tokens, 0);
    }
}
