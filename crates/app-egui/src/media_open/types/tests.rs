use super::*;
use crate::media_open::WebMediaSourceIntent;

#[test]
fn service_labels_and_source_debug_do_not_leak_url_secrets() {
    let direct_secret = "https://user:password@example.com/video.mp4?token=very-secret";
    let direct_locator =
        service_direct_media::parse_direct_media_url(direct_secret).expect("direct locator parsed");
    let direct_source =
        ActiveMediaSource::Web(WebMediaSourceIntent::direct(direct_locator.clone()));
    let direct_debug = format!("{direct_source:?}");
    let direct_label =
        SafeMediaLabel::from_service_safe_label(direct_locator.safe_label()).to_string();

    assert!(!direct_debug.contains("password"));
    assert!(!direct_debug.contains("very-secret"));
    assert!(!direct_label.contains("password"));
    assert!(!direct_label.contains("very-secret"));

    let yt_dlp_secret = "https://www.youtube.com/watch?v=dQw4w9WgXcQ&token=yt_dlp-secret";
    let yt_dlp_locator =
        service_ytdlp::parse_yt_dlp_media_locator(yt_dlp_secret).expect("YtDlp locator parsed");
    let yt_dlp_label =
        SafeMediaLabel::from_service_safe_label(yt_dlp_locator.safe_label()).to_string();
    assert!(!yt_dlp_label.contains("yt_dlp-secret"));
    assert!(!yt_dlp_label.contains('?'));
}

#[test]
fn safe_label_is_bounded_by_named_unicode_limit() {
    let raw_label = "я".repeat(SAFE_MEDIA_LABEL_MAX_CHARS + 25);
    let label = SafeMediaLabel::from_service_safe_label(&raw_label);

    assert_eq!(label.as_str().chars().count(), SAFE_MEDIA_LABEL_MAX_CHARS);
}

#[test]
fn playback_window_identity_wraps_reopen_request_without_source_specific_types() {
    let semantic_identity = player_core::MediaPlaybackWindow::new(
        media_core::MediaTime::from_secs(10),
        Some(media_core::MediaTime::from_secs(25)),
    )
    .expect("valid neutral window");
    let source = ActiveMediaSource::LocalFile(PathBuf::from("fixture.flac"))
        .with_playback_window(semantic_identity);
    let request = source.wrap_reopen_request(MediaOpenSourceRequest::Local {
        path: PathBuf::from("fixture.flac"),
        expected_fingerprint: None,
        demux_config: rustiplayer_config::PlayerDemuxConfig::default(),
    });

    assert_eq!(source.playback_window(), Some(semantic_identity));
    assert!(matches!(
        source.physical_source(),
        ActiveMediaSource::LocalFile(_)
    ));
    assert!(matches!(
        request,
        MediaOpenSourceRequest::PlaybackWindow {
            semantic_identity: actual,
            ..
        } if actual == semantic_identity
    ));
}
