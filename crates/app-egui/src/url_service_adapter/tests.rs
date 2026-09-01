use capability_core::SystemCapabilities;
use playlist_core::SecretUrlLocator;
use rustiplayer_config::AppConfig;

use super::{
    ImplementedYtDlpInputProviderCapability, StartupUrlClassification, StartupUrlServiceRegistry,
    StartupUrlUnsupportedReason, classify_playlist_url, classify_startup_url,
};

fn persistence_identity(locator: &super::StartupUrlLocator) -> String {
    locator
        .to_playlist_locator()
        .expect("service locator должен создавать непустую domain identity")
        .expose_secret_for_persistence()
        .to_string()
}

#[test]
fn registry_keeps_exact_generic_identity_in_service_owner() {
    let exact_url = "https://youtu.be/video-id?v=keep&si=preserve&unknown=preserve";
    let classified = classify_startup_url(exact_url);
    let StartupUrlClassification::Supported(locator) = classified else {
        panic!("generic yt-dlp service должен принять HTTP(S) URL");
    };

    assert_eq!(persistence_identity(&locator), exact_url);
}

#[test]
fn direct_signed_url_keeps_exact_identity() {
    let secret = "https://cdn.example.test/video.mp4?signature=a%2Bb&part=1+2";
    let classified = classify_startup_url(secret);
    let StartupUrlClassification::Supported(locator) = classified else {
        panic!("generic direct service должен принять signed media URL");
    };

    assert_eq!(persistence_identity(&locator), secret);
}

#[test]
fn m3u8_hint_builds_native_admission_with_one_typed_fallback() {
    let exact_url = "https://cdn.example.test/master.m3u8?signature=native-secret";
    let StartupUrlClassification::Supported(locator) = classify_startup_url(exact_url) else {
        panic!("m3u8 hint должен выбрать native admission adapter");
    };
    assert_eq!(persistence_identity(&locator), exact_url);

    let request = locator
        .into_media_open_source_request(
            &AppConfig::default(),
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("native request");
    assert!(matches!(
        request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));
}

#[test]
fn mpd_hint_builds_content_probed_native_admission_when_extractor_is_disabled() {
    let exact_url = "https://cdn.example.test/video.mpd?signature=native-secret";
    let StartupUrlClassification::Supported(locator) = classify_startup_url(exact_url) else {
        panic!("mpd hint должен выбрать native DASH admission adapter");
    };
    assert_eq!(persistence_identity(&locator), exact_url);

    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    locator
        .validate_config(&app_config)
        .expect("native DASH classification не зависит от extractor availability");
    let request = locator
        .into_media_open_source_request(
            &app_config,
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("native DASH request");
    assert!(matches!(
        request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));
}

#[test]
fn m3u8_text_outside_url_path_remains_generic() {
    let StartupUrlClassification::Supported(locator) =
        classify_startup_url("https://example.test/watch?next=movie.m3u8")
    else {
        panic!("generic HTTP URL должен остаться поддержан");
    };
    let request = locator
        .into_media_open_source_request(
            &AppConfig::default(),
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("generic request");
    assert!(matches!(
        request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));
}

#[test]
fn playlist_locator_reopens_through_same_service_registry() {
    let domain_locator =
        SecretUrlLocator::from_reopenable_url("https://youtu.be/video-id?si=drop&future=preserve")
            .expect("domain URL identity должна быть непустой");
    let StartupUrlClassification::Supported(service_locator) =
        classify_playlist_url(&domain_locator)
    else {
        panic!("persisted locator должен быть принят service registry");
    };

    assert_eq!(
        persistence_identity(&service_locator),
        "https://youtu.be/video-id?si=drop&future=preserve"
    );
}

#[test]
fn unsupported_error_does_not_reflect_secret_input() {
    let classified = classify_startup_url("https://user:password@[invalid-host]?token=secret");
    let StartupUrlClassification::Unsupported { reason } = classified else {
        panic!("синтаксически невалидный URL должен быть rejected");
    };

    assert_eq!(reason, StartupUrlUnsupportedReason::InvalidSyntax);
    let safe_error = reason.safe_error();
    assert!(!safe_error.contains("password"));
    assert!(!safe_error.contains("secret"));
    assert!(!safe_error.contains("private"));
}

#[test]
fn registry_prioritizes_direct_media_and_freezes_chosen_adapter_without_open_fallback() {
    let StartupUrlClassification::Supported(direct_locator) =
        classify_startup_url("https://cdn.example.test/video.mp4?token=direct")
    else {
        panic!("direct media URL должен быть принят");
    };
    let direct_request = direct_locator
        .into_media_open_source_request(
            &AppConfig::default(),
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("direct request");
    assert!(matches!(
        direct_request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));

    let exact_generic_url =
        "https://user:password@media.example.test/article?token=generic#chapter";
    let StartupUrlClassification::Supported(generic_locator) =
        classify_startup_url(exact_generic_url)
    else {
        panic!("оставшийся HTTP(S) URL должен попасть в yt-dlp fallback");
    };
    assert_eq!(persistence_identity(&generic_locator), exact_generic_url);
    assert!(
        generic_locator.requires_sensitive_persistence_acknowledgement(),
        "exact query/userinfo обязаны пройти aggregated durable-locator acknowledgement"
    );
    let generic_request = generic_locator
        .into_media_open_source_request(
            &AppConfig::default(),
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("yt-dlp request");
    assert!(matches!(
        generic_request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));
}

/// HTTP Ogg/WebM и FTP Ogg обходят extractor даже при disabled process policy.
#[test]
fn direct_progressive_extensions_route_to_native_adapter_when_ytdlp_is_disabled() {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;

    for raw_locator in [
        "https://media.example.test/audio.ogg",
        "https://media.example.test/video.webm",
        "ftp://media.example.test/audio.ogg",
    ] {
        let StartupUrlClassification::Supported(locator) = classify_startup_url(raw_locator) else {
            panic!("{raw_locator} должен быть принят direct classifier-ом");
        };
        assert!(
            locator.safe_label().starts_with("direct media "),
            "startup registry должен зафиксировать direct service adapter"
        );
        let request = locator
            .into_media_open_source_request(
                &app_config,
                &SystemCapabilities::empty(1),
                audio::AudioDecodeCapabilitySnapshot::empty(),
            )
            .expect("disabled yt-dlp не должен блокировать direct request");
        let crate::media_open::MediaOpenSourceRequest::Web(_) = request else {
            panic!("direct progressive locator должен построить neutral web request");
        };
    }
}

#[test]
fn extended_s00_schemes_follow_implemented_or_excluded_profile_disposition() {
    for raw_locator in [
        "ftp://media.example.test/video.webm",
        "ftps://media.example.test/video.webm",
    ] {
        let StartupUrlClassification::Supported(locator) = classify_startup_url(raw_locator) else {
            panic!("production registry должен включать S37 FTP provider");
        };
        assert_eq!(persistence_identity(&locator), raw_locator);

        let missing_registry = StartupUrlServiceRegistry {
            implemented_yt_dlp_input_providers: &[],
        };
        let StartupUrlClassification::Supported(native_without_extractor) =
            missing_registry.classify(raw_locator)
        else {
            panic!("direct FTP не должен зависеть от extractor provider capabilities");
        };
        assert_eq!(persistence_identity(&native_without_extractor), raw_locator);

        let implemented_capabilities = [ImplementedYtDlpInputProviderCapability::exact(
            service_ytdlp::YtDlpInputScheme::Ftp,
        )];
        let active_registry = StartupUrlServiceRegistry {
            implemented_yt_dlp_input_providers: &implemented_capabilities,
        };
        let StartupUrlClassification::Supported(active_locator) =
            active_registry.classify(raw_locator)
        else {
            panic!("extractor capability не должна менять direct FTP classification");
        };
        assert_eq!(persistence_identity(&active_locator), raw_locator);
    }

    for (raw_locator, input_scheme) in [
        (
            "rtmp://media.example.test/live",
            service_ytdlp::YtDlpInputScheme::Rtmp,
        ),
        (
            "rtmpe://media.example.test/live",
            service_ytdlp::YtDlpInputScheme::Rtmpe,
        ),
    ] {
        let StartupUrlClassification::Unsupported { reason } = classify_startup_url(raw_locator)
        else {
            panic!("production registry не должен включать исключённый S39 RTMP provider");
        };
        assert_eq!(
            reason,
            StartupUrlUnsupportedReason::ProfileExcludedInputScheme { input_scheme }
        );

        let implemented_capabilities =
            [ImplementedYtDlpInputProviderCapability::exact(input_scheme)];
        let active_registry = StartupUrlServiceRegistry {
            implemented_yt_dlp_input_providers: &implemented_capabilities,
        };
        let StartupUrlClassification::Unsupported {
            reason: active_reason,
        } = active_registry.classify(raw_locator)
        else {
            panic!("registration capability не должна обходить profile exclusion");
        };
        assert_eq!(
            active_reason,
            StartupUrlUnsupportedReason::ProfileExcludedInputScheme { input_scheme }
        );
        assert!(active_reason.safe_error().contains("исключён"));
    }
}

#[test]
fn profile_exclusion_does_not_normalize_unapproved_rtmp_aliases() {
    for (raw_locator, expected_reason) in [
        (
            "rtmps://media.example.test/live",
            StartupUrlUnsupportedReason::UnsupportedScheme,
        ),
        (
            "rtmpt://media.example.test/live",
            StartupUrlUnsupportedReason::UnsupportedScheme,
        ),
        (
            "rtmpte://media.example.test/live",
            StartupUrlUnsupportedReason::UnsupportedScheme,
        ),
        (
            "rtmp_ffmpeg://media.example.test/live",
            StartupUrlUnsupportedReason::InvalidSyntax,
        ),
    ] {
        let StartupUrlClassification::Unsupported { reason } = classify_startup_url(raw_locator)
        else {
            panic!("неутверждённый alias не должен наследовать RTMP profile identity");
        };
        assert_eq!(reason, expected_reason);
    }

    let rtmp_only_capabilities = [ImplementedYtDlpInputProviderCapability::exact(
        service_ytdlp::YtDlpInputScheme::Rtmp,
    )];
    let rtmp_only_registry = StartupUrlServiceRegistry {
        implemented_yt_dlp_input_providers: &rtmp_only_capabilities,
    };
    let StartupUrlClassification::Unsupported { reason } =
        rtmp_only_registry.classify("rtmpe://media.example.test/live")
    else {
        panic!("RTMP capability не должна автоматически включать RTMPE");
    };
    assert_eq!(
        reason,
        StartupUrlUnsupportedReason::ProfileExcludedInputScheme {
            input_scheme: service_ytdlp::YtDlpInputScheme::Rtmpe,
        }
    );
}

#[test]
fn excluded_and_unknown_schemes_are_typed_rejected() {
    for raw_locator in [
        "file:///home/user/video.mp4",
        "rtsp://media.example.test/live",
        "rtp://media.example.test/live",
        "mms://media.example.test/live",
        "unknown://media.example.test/live",
    ] {
        let StartupUrlClassification::Unsupported { reason } = classify_startup_url(raw_locator)
        else {
            panic!("scheme вне profile должна быть rejected");
        };
        assert_eq!(reason, StartupUrlUnsupportedReason::UnsupportedScheme);
        let safe_error = reason.safe_error();
        assert!(!safe_error.contains(raw_locator));
    }
}

#[test]
fn active_extended_locator_redacts_credentials_and_requires_acknowledgement() {
    let implemented_capabilities = [ImplementedYtDlpInputProviderCapability::exact(
        service_ytdlp::YtDlpInputScheme::Ftp,
    )];
    let active_registry = StartupUrlServiceRegistry {
        implemented_yt_dlp_input_providers: &implemented_capabilities,
    };
    let raw_locator = "ftp://user:password@media.example.test/private/video.webm?token=secret";
    let StartupUrlClassification::Supported(production_locator) = classify_startup_url(raw_locator)
    else {
        panic!("S37 production registry должен принять FTP");
    };
    let StartupUrlClassification::Supported(locator) = active_registry.classify(raw_locator) else {
        panic!("active FTP capability должна принять exact locator");
    };

    assert_eq!(persistence_identity(&production_locator), raw_locator);
    assert_eq!(persistence_identity(&locator), raw_locator);
    assert!(locator.requires_sensitive_persistence_acknowledgement());
    for secret in ["user", "password", "private", "token", "secret"] {
        assert!(!format!("{production_locator:?}").contains(secret));
        assert!(!production_locator.safe_label().contains(secret));
        assert!(!format!("{locator:?}").contains(secret));
        assert!(!locator.safe_label().contains(secret));
    }
}
