use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::candidate::raw::YtDlpSerializedFormat;
use crate::candidate::request_material::{
    MAX_REQUEST_SECRET_UTF8_BYTES, YtDlpRequestMaterialSummary, normalize_fragments,
    normalize_request_material,
};

/// Test helper создаёт bounded secret без публичного API.
fn secret(value: &str) -> SecretText {
    SecretText::bounded(value.to_owned(), MAX_REQUEST_SECRET_UTF8_BYTES)
        .expect("test secret bounded")
}

/// Собирает service-private material с explicit DASH fields.
fn request(
    url: Option<&str>,
    manifest_url: Option<&str>,
    fragments: Vec<YtDlpRequestFragment>,
    fragment_base_url: Option<&str>,
    is_dash_periods: bool,
) -> YtDlpRequestMaterial {
    let mut http_headers = BTreeMap::new();
    http_headers.insert("Authorization".to_owned(), secret("Bearer top-secret"));
    YtDlpRequestMaterial::V1(YtDlpRequestMaterialV1 {
        url: url.map(secret),
        manifest_url: manifest_url.map(secret),
        fragments: fragments.into_boxed_slice(),
        fragment_base_url: fragment_base_url.map(secret),
        is_dash_periods,
        hls_media_playlist_data: None,
        http_headers,
        http_range_request_limit: None,
        cookies: Some(super::super::YtDlpCookieMaterial::RequestHeader(secret(
            "session=top-secret",
        ))),
        extra_param_to_segment_url: Some(secret("token=top-secret")),
        extra_param_to_key_url: None,
        hls_aes: None,
        rtmp: None,
    })
}

/// Создаёт fragment с обеими upstream locator формами для precedence proof.
fn fragment(
    url: Option<&str>,
    path: Option<&str>,
    duration_seconds: Option<f64>,
    byte_length: Option<u64>,
) -> YtDlpRequestFragment {
    YtDlpRequestFragment {
        url: url.map(secret),
        path: path.map(secret),
        duration_seconds,
        byte_length,
    }
}

#[test]
fn non_empty_fragments_are_authoritative_and_absolute_url_wins_over_path() {
    let request = request(
        Some("https://cdn.invalid/selected.mpd"),
        Some("https://cdn.invalid/manifest.mpd"),
        vec![
            fragment(
                Some("https://cdn.invalid/absolute.m4s?secret=1"),
                Some("ignored.m4s"),
                None,
                Some(123),
            ),
            fragment(None, Some("relative.m4s"), Some(2.5), None),
        ],
        Some("https://cdn.invalid/base/"),
        false,
    );
    let dash = request.dash_request_material().expect("valid fragments");
    assert_eq!(dash.input().kind(), YtDlpDashInputKind::SerializedFragments);
    assert_eq!(dash.input().manifest_url_for_fetch(), None);
    let fragments = dash.input().fragments().collect::<Vec<_>>();
    assert_eq!(
        fragments[0].locator_kind(),
        YtDlpDashFragmentLocatorKind::AbsoluteUrl
    );
    assert_eq!(
        fragments[0].locator_for_transport(),
        "https://cdn.invalid/absolute.m4s?secret=1"
    );
    assert_eq!(fragments[0].base_url_for_relative_resolution(), None);
    assert_eq!(fragments[0].role(), YtDlpDashFragmentRole::Initialization);
    assert_eq!(fragments[0].duration_seconds(), None);
    assert_eq!(fragments[0].byte_length(), Some(123));
    assert_eq!(fragments[1].role(), YtDlpDashFragmentRole::Media);
    assert_eq!(
        fragments[1].base_url_for_relative_resolution(),
        Some("https://cdn.invalid/base/")
    );
    assert_eq!(
        dash.segment_query_parameters_for_projection(),
        Some("token=top-secret")
    );
    assert_eq!(dash.request_context().headers().count(), 1);
    assert_eq!(
        dash.request_context().serialized_cookies(),
        Some("session=top-secret")
    );
}

#[test]
fn multi_period_marker_forces_manifest_and_missing_manifest_is_typed_reject() {
    let fragments = vec![fragment(None, Some("period-unknown.m4s"), None, None)];
    let with_manifest = request(
        Some("https://cdn.invalid/selected.mpd"),
        Some("https://cdn.invalid/authoritative.mpd"),
        fragments.clone(),
        Some("https://cdn.invalid/base/"),
        true,
    );
    let dash = with_manifest
        .dash_request_material()
        .expect("MPD owns multi-period");
    assert_eq!(dash.input().kind(), YtDlpDashInputKind::Manifest);
    assert_eq!(
        dash.input().manifest_url_for_fetch(),
        Some("https://cdn.invalid/authoritative.mpd")
    );
    assert_eq!(dash.input().fragments().len(), 0);

    let without_manifest = request(
        Some("https://cdn.invalid/selected.mpd"),
        None,
        fragments,
        Some("https://cdn.invalid/base/"),
        true,
    );
    assert_eq!(
        without_manifest.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::MultiPeriodRequiresManifest
    );
}

#[test]
fn relative_fragment_requires_base_and_empty_input_uses_manifest_precedence() {
    let missing_base = request(
        None,
        None,
        vec![fragment(None, Some("relative.m4s"), None, None)],
        None,
        false,
    );
    assert_eq!(
        missing_base.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::RelativeFragmentMissingBase
    );

    let manifest = request(
        Some("https://cdn.invalid/selected.mpd"),
        Some("https://cdn.invalid/upstream.mpd"),
        Vec::new(),
        None,
        false,
    );
    assert_eq!(
        manifest
            .dash_request_material()
            .expect("manifest")
            .input()
            .manifest_url_for_fetch(),
        Some("https://cdn.invalid/upstream.mpd")
    );

    let relative_url_field = request(
        None,
        None,
        vec![fragment(
            Some("relative-in-url-field.m4s"),
            None,
            None,
            None,
        )],
        None,
        false,
    );
    assert_eq!(
        relative_url_field.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::InvalidFragmentLocator
    );

    let invalid_base = request(
        None,
        None,
        vec![fragment(None, Some("relative.m4s"), None, None)],
        Some("relative-base/"),
        false,
    );
    assert_eq!(
        invalid_base.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::InvalidFragmentBaseUrl
    );

    let invalid_manifest = request(Some("relative.mpd"), None, Vec::new(), None, false);
    assert_eq!(
        invalid_manifest.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::InvalidManifestUrl
    );

    let root_relative = request(
        None,
        None,
        vec![
            fragment(None, Some("/init.webm"), None, None),
            fragment(None, Some("/root-relative.webm"), Some(1.0), None),
        ],
        Some("https://cdn.invalid/base/"),
        false,
    );
    root_relative
        .dash_request_material()
        .expect("root-relative reference сохраняет origin base URL");

    for unsafe_path in [
        "//evil.invalid/network-path.m4s",
        "//fastiplayer.invalid/same-origin-network-path.m4s",
        r"\\evil.invalid\backslash-network-path.m4s",
        "https://evil.invalid/cross-origin.m4s",
        "http://[invalid-ipv6",
    ] {
        let unsafe_request = request(
            None,
            None,
            vec![fragment(None, Some(unsafe_path), None, None)],
            Some("https://cdn.invalid/base/"),
            false,
        );
        assert_eq!(
            unsafe_request.dash_request_material().unwrap_err(),
            YtDlpDashRequestMaterialViolation::InvalidFragmentLocator
        );
    }
}

#[test]
fn serialized_fragment_roles_require_one_init_then_positive_media_durations() {
    let leading_media = request(
        None,
        None,
        vec![
            fragment(None, Some("first.m4s"), Some(1.0), None),
            fragment(None, Some("second.m4s"), Some(1.0), None),
        ],
        Some("https://cdn.invalid/base/"),
        false,
    );
    assert_eq!(
        leading_media.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::MissingLeadingInitialization
    );

    let init_only = request(
        None,
        None,
        vec![fragment(None, Some("init.mp4"), None, None)],
        Some("https://cdn.invalid/base/"),
        false,
    );
    assert_eq!(
        init_only.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::MissingMediaFragments
    );

    let second_missing_duration = request(
        None,
        None,
        vec![
            fragment(None, Some("init.webm"), None, None),
            fragment(None, Some("ambiguous.webm"), None, None),
        ],
        Some("https://cdn.invalid/base/"),
        false,
    );
    assert_eq!(
        second_missing_duration.dash_request_material().unwrap_err(),
        YtDlpDashRequestMaterialViolation::AmbiguousFragmentRole
    );
}

#[test]
fn generator_shape_and_invalid_duration_or_filesize_are_rejected_during_normalization() {
    assert_eq!(
        normalize_fragments(Some(&json!("generator repr"))).err(),
        Some(YtDlpRequestMaterialViolation::InvalidFragments)
    );
    assert_eq!(
        normalize_fragments(Some(&json!([{"path":"a.m4s","duration":-1.0}]))).err(),
        Some(YtDlpRequestMaterialViolation::InvalidFragments)
    );
    assert_eq!(
        normalize_fragments(Some(&json!([{"path":"a.m4s","filesize":"large"}]))).err(),
        Some(YtDlpRequestMaterialViolation::InvalidFragments)
    );
    assert_eq!(
        normalize_fragments(Some(&json!([{"path":"a.m4s","duration":86401.0}]))).err(),
        Some(YtDlpRequestMaterialViolation::InvalidFragments)
    );
    assert_eq!(
        normalize_fragments(Some(&json!([{"path":"a.m4s","filesize":68719476737_u64}]))).err(),
        Some(YtDlpRequestMaterialViolation::InvalidFragments)
    );
}

#[test]
fn raw_is_dash_periods_mapping_and_debug_are_secret_safe() {
    let raw = YtDlpSerializedFormat {
        url: Some("https://cdn.invalid/selected.mpd?token=raw".to_owned()),
        manifest_url: Some("https://cdn.invalid/manifest.mpd?token=raw".to_owned()),
        is_dash_periods: Some(true),
        ..YtDlpSerializedFormat::default()
    };
    let normalized = normalize_request_material(&raw).expect("valid raw request");
    let YtDlpRequestMaterialSummary {
        is_dash_periods, ..
    } = normalized.summary();
    assert!(is_dash_periods);
    let debug = format!("{normalized:?}");
    assert!(!debug.contains("cdn.invalid"));
    assert!(!debug.contains("token=raw"));
    assert!(debug.contains("is_dash_periods: true"));
}
