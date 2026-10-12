use capability_core::SystemCapabilities;
use fastiplayer_config::AppConfig;
use playlist_core::SecretUrlLocator;

use super::{
    ImplementedYtDlpInputProviderCapability, ServiceClassifierResult, StartupUrlClassification,
    StartupUrlServiceRegistry, StartupUrlUnsupportedReason, classify_native_hds_startup_url,
    classify_playlist_url, classify_startup_url,
};

fn persistence_identity(locator: &super::StartupUrlLocator) -> String {
    locator
        .to_playlist_locator()
        .expect("Service locator must create a non-empty domain identity.")
        .expose_secret_for_persistence()
        .to_string()
}

#[test]
fn registry_keeps_exact_generic_identity_in_service_owner() {
    let exact_url = "https://youtu.be/video-id?v=keep&si=preserve&unknown=preserve";
    let classified = classify_startup_url(exact_url);
    let StartupUrlClassification::Supported(locator) = classified else {
        panic!("generic yt-dlp service must accept HTTP(S) URL");
    };

    assert_eq!(persistence_identity(&locator), exact_url);
}

#[test]
fn direct_signed_url_keeps_exact_identity() {
    let secret = "https://cdn.example.test/video.mp4?signature=a%2Bb&part=1+2";
    let classified = classify_startup_url(secret);
    let StartupUrlClassification::Supported(locator) = classified else {
        panic!("Generic direct service must accept signed media URL");
    };

    assert_eq!(persistence_identity(&locator), secret);
}

#[test]
fn m3u8_hint_builds_native_admission_with_one_typed_fallback() {
    let exact_url = "https://cdn.example.test/master.m3u8?signature=native-secret";
    let StartupUrlClassification::Supported(locator) = classify_startup_url(exact_url) else {
        panic!("m3u8 hint selects native admission adapter");
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
        panic!("mpd hint must select native DASH admission adapter");
    };
    assert_eq!(persistence_identity(&locator), exact_url);

    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    locator
        .validate_config(&app_config)
        .expect("Native DASH classification is not dependent on extractor availability");
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
fn manifest_path_hint_builds_native_smooth_admission_when_extractor_is_disabled() {
    let exact_url = "https://cdn.example.test/vod/Manifest?signature=native-secret";
    let StartupUrlClassification::Supported(locator) = classify_startup_url(exact_url) else {
        panic!("/Manifest hint must select the native Smooth admission adapter");
    };
    assert_eq!(persistence_identity(&locator), exact_url);

    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    locator
        .validate_config(&app_config)
        .expect("Native Smooth classification is not dependent on extractor availability.");
    let request = locator
        .into_media_open_source_request(
            &app_config,
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("native Smooth request");
    assert!(matches!(
        request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));
}

#[test]
fn f4m_path_hint_builds_native_hds_admission_when_extractor_is_disabled() {
    let exact_url = "https://cdn.example.test/vod/ROOT.F4M?signature=native-secret";
    let StartupUrlClassification::Supported(locator) = classify_startup_url(exact_url) else {
        panic!(".f4m hint must select native HDS admission adapter");
    };
    assert_eq!(persistence_identity(&locator), exact_url);

    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    locator
        .validate_config(&app_config)
        .expect("Native HDS classification does not depend on extractor availability");
    let request = locator
        .into_media_open_source_request(
            &app_config,
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("native HDS request");
    assert!(matches!(
        request,
        crate::media_open::MediaOpenSourceRequest::Web(_)
    ));
}

#[test]
fn f4m_hint_uses_only_the_final_path_segment_not_query_or_fragment() {
    for url in [
        "https://cdn.example.test/video.mp4?redirect=root.f4m",
        "https://cdn.example.test/video.mp4#root.f4m",
        "ftp://cdn.example.test/root.f4m",
    ] {
        let classification = classify_native_hds_startup_url(url);
        assert!(
            matches!(classification, ServiceClassifierResult::NotUrl),
            "foreign hint shouldn't claimed: {url}"
        );
    }
}

#[test]
fn manifest_text_outside_final_path_segment_remains_generic() {
    let StartupUrlClassification::Supported(locator) =
        classify_startup_url("https://example.test/watch?next=/vod/Manifest")
    else {
        panic!("The generic HTTP URL should remain supported.");
    };
    let request = locator
        .into_media_open_source_request(
            &AppConfig::default(),
            &SystemCapabilities::empty(1),
            audio::AudioDecodeCapabilitySnapshot::empty(),
        )
        .expect("generic extractor request");
    let crate::media_open::MediaOpenSourceRequest::Web(_) = request else {
        panic!("Generic URLs must remain a web request.");
    };
}

#[test]
fn m3u8_text_outside_url_path_remains_generic() {
    let StartupUrlClassification::Supported(locator) =
        classify_startup_url("https://example.test/watch?next=movie.m3u8")
    else {
        panic!("The generic HTTP URL should remain supported.");
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
            .expect("Domain URL identity should not be empty.");
    let StartupUrlClassification::Supported(service_locator) =
        classify_playlist_url(&domain_locator)
    else {
        panic!("Persistent locator must be accepted by the service registry");
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
        panic!("A syntactically invalid URL must be rejected");
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
        panic!("Direct media URL must be accepted");
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
        panic!("The remaining HTTP(S) URL must go to yt-dlp fallback");
    };
    assert_eq!(persistence_identity(&generic_locator), exact_generic_url);
    assert!(
        generic_locator.requires_sensitive_persistence_acknowledgement(),
        "exact query/userinfo must pass aggregated durable-locator acknowledgement"
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
            panic!("{raw_locator} must be accepted by direct classifier");
        };
        assert!(
            locator.safe_label().starts_with("direct media "),
            "startup registry must be fixed direct service adapter"
        );
        let request = locator
            .into_media_open_source_request(
                &app_config,
                &SystemCapabilities::empty(1),
                audio::AudioDecodeCapabilitySnapshot::empty(),
            )
            .expect("Disabled yt-dlp must not block direct request");
        let crate::media_open::MediaOpenSourceRequest::Web(_) = request else {
            panic!("Direct Progressive locator: Neutral Web Request");
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
            panic!("Production registry includes S37 FTP provider");
        };
        assert_eq!(persistence_identity(&locator), raw_locator);

        let missing_registry = StartupUrlServiceRegistry {
            implemented_yt_dlp_input_providers: &[],
        };
        let StartupUrlClassification::Supported(native_without_extractor) =
            missing_registry.classify(raw_locator)
        else {
            panic!("Direct FTP does not depend on extractor provider capabilities.");
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
            panic!("Extractability does not change direct FTP classification");
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
            panic!("The production registry must not include an excluded S39 RTMP provider.");
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
            panic!("Registration capability should not bypass the Profile Exclusion");
        };
        assert_eq!(
            active_reason,
            StartupUrlUnsupportedReason::ProfileExcludedInputScheme { input_scheme }
        );
        assert!(active_reason.safe_error().contains("excluded"));
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
            panic!("An unapproved alias must not inherit an RTMP profile identity");
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
        panic!("RTMP capability does not automatically activate RTMPE");
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
            panic!("The scheme outside the profile must be rejected.");
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
        panic!("S37 Production Registry to Accept FTP");
    };
    let StartupUrlClassification::Supported(locator) = active_registry.classify(raw_locator) else {
        panic!("Active FTP Capacity to Accept the exact location");
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

/// Домен для текста ошибки не раскрывает логин, пароль, порт, путь и query ссылки.
#[test]
fn display_host_shows_only_domain_without_secrets() {
    let cases = [
        (
            "https://user:hunter2@www.example.test:8443/private/watch?v=secret&token=abc",
            "example.test",
        ),
        (
            "https://cdn.example.test/media/clip.mp4?sig=secret",
            "cdn.example.test",
        ),
        ("http://127.0.0.1:9000/clip.mp4", "127.0.0.1"),
    ];
    for (exact_url, expected_host) in cases {
        let StartupUrlClassification::Supported(locator) = classify_startup_url(exact_url) else {
            panic!("Test reference must be supported: {expected_host}");
        };

        let display_host = locator.display_host();

        assert_eq!(display_host.as_deref(), Some(expected_host));
        let shown = display_host.unwrap_or_default();
        for secret in [
            "hunter2", "user", "secret", "token", "8443", "private", "watch",
        ] {
            assert!(
                !shown.contains(secret),
                "The domain \"{shown}\" revealed \"{secret}\""
            );
        }
    }
}

/// CLI-ссылка при выключенном yt-dlp: понятный текст с доменом вместо «NetworkError: …».
#[test]
fn disabled_extractor_cli_link_explains_setting_in_plain_words() {
    let StartupUrlClassification::Supported(locator) =
        classify_startup_url("https://www.video.example.test/watch?v=secret")
    else {
        panic!("Generic link should be supported by yt-dlp adapter");
    };
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;

    let error = locator
        .validate_config(&app_config)
        .expect_err("switched off yt-dlp prohibits opening");

    assert_eq!(
        error,
        "Could not open the link (video.example.test): website downloads through yt-dlp are disabled in Settings"
    );
}

/// Все виды пробельных символов, которые встречаются вокруг вставленной ссылки.
const SURROUNDING_WHITESPACE: [&str; 6] = [" ", "\n", "\r\n", "\t", "\u{00A0}", " \t\n\u{3000}"];

#[test]
fn surrounding_whitespace_does_not_change_classified_identity() {
    let exact_url = "https://example.com/a.mp4";
    let StartupUrlClassification::Supported(reference) = classify_startup_url(exact_url) else {
        panic!("Direct media URL must be accepted.");
    };
    let reference_identity = persistence_identity(&reference);
    assert_eq!(reference_identity, exact_url);

    for padding in SURROUNDING_WHITESPACE {
        let padded = format!("{padding}{exact_url}{padding}");
        let StartupUrlClassification::Supported(locator) = classify_startup_url(&padded) else {
            panic!("A reference with spaces around must be accepted: {padding:?}");
        };
        assert_eq!(
            persistence_identity(&locator),
            reference_identity,
            "{padding:?}"
        );
    }
}

#[test]
fn whitespace_only_input_is_not_a_url_instead_of_panicking() {
    for blank in ["", " ", "\n", "\t \r\n", "\u{00A0}\u{00A0}"] {
        assert!(
            matches!(
                classify_startup_url(blank),
                StartupUrlClassification::NotUrl
            ),
            "{blank:?}"
        );
    }
}

/// A10: пользователь видит человеческий английский текст, без имени Rust-вида ошибки
/// (`NetworkError:`) и без внутренних терминов («media services», «provider», «profile»).
#[test]
fn unsupported_url_messages_are_plain_english_without_internal_jargon() {
    let ftp = service_ytdlp::YtDlpInputScheme::Ftp;
    let cases = [
        (
            StartupUrlUnsupportedReason::InvalidSyntax,
            "Incorrect reference",
        ),
        (
            StartupUrlUnsupportedReason::UnsupportedScheme,
            "This type of link is not supported.",
        ),
        (
            StartupUrlUnsupportedReason::ImplementedProviderUnavailable { input_scheme: ftp },
            "'ftp' links are not yet supported",
        ),
        (
            StartupUrlUnsupportedReason::ProfileExcludedInputScheme { input_scheme: ftp },
            "'ftp' links are excluded from those supported in this version",
        ),
        (
            StartupUrlUnsupportedReason::NoRegisteredService,
            "This link is not supported",
        ),
    ];
    for (reason, expected) in cases {
        let message = reason.safe_error();
        assert_eq!(message, expected);
        for jargon in ["NetworkError", "media services", "provider", "profile"] {
            assert!(!message.contains(jargon), "{message}");
        }
    }
}
