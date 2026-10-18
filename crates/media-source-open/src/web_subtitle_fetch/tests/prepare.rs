//! Сквозной тест: yt-dlp со списком субтитров -> `prepare_yt_dlp_web_media`
//! -> дорожки в модели потока -> скачивание через дескриптор источника.

use std::num::NonZeroUsize;

use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig, VideoCodec, WebMediaConfig};
use serde_json::json;
use source_core::CancellationToken;
use web_media_core::{ExtractorInvocationReason, LanguageTag, SubtitleKind};

use super::origin::{LoopbackOrigin, Reply};
use super::yt_dlp_script::FakeYtDlp;
use super::{
    SRV3_BODY, WebSubtitleFetchLimits, WebSubtitleFormat, WebSubtitleTrackId, yt_dlp_config,
};
use crate::web_media_open::{YtDlpCandidateOpenIntent, prepare_yt_dlp_web_media};

/// Настоящий (крошечный) MP3: demuxer должен реально открыть источник.
const MP3_FIXTURE: &[u8] =
    include_bytes!("../../../../web-media-hls/tests/fixtures/packed_audio/mp30.mp3");

#[test]
fn prepared_media_exposes_subtitle_tracks_and_downloads_them() {
    let origin = LoopbackOrigin::start();
    let base = origin.base_url();
    origin.route("/media.mp3", Reply::ok(MP3_FIXTURE));
    origin.route("/subs/en.srv3", Reply::ok(SRV3_BODY));
    let document = json!({
        "id": "e2e",
        "title": "Subtitle proof",
        "formats": [{
            "format_id": "audio", "url": format!("{base}/media.mp3"), "protocol": "http",
            "ext": "mp3", "container": "mp3", "vcodec": null, "acodec": null
        }],
        "subtitles": {"en": [
            {"ext": "srv3", "name": "English", "url": format!("{base}/subs/en.srv3?expire=4102444800&signature=SECRET")}
        ]},
        "automatic_captions": {"ru": [
            {"ext": "vtt", "name": "Russian", "url": format!("{base}/subs/ru.vtt?kind=asr&lang=en&tlang=ru&expire=4102444800")}
        ]}
    })
    .to_string();
    let fake = FakeYtDlp::new(&document);
    let locator = service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/e2e")
        .expect("locator");

    let prepared = prepare_yt_dlp_web_media(
        &locator,
        &NetworkConfig::default(),
        &WebMediaConfig::default(),
        &yt_dlp_config(),
        &fake.adapter(),
        &PlayerDemuxConfig::default(),
        &[VideoCodec::Vp9],
        &capability_core::SystemCapabilities::empty(1),
        audio::AudioDecodeCapabilityProvider::audio_decode_capability_snapshot(
            &audio::ProductionAudioDecoderFactory::default(),
        ),
        YtDlpCandidateOpenIntent::BestPlayable,
        ExtractorInvocationReason::PageMediaResolution,
        CancellationToken::new(),
        || false,
    )
    .expect("media with subtitles must prepare");

    let tracks = prepared.stream_configuration().web_subtitle_tracks();
    let summary: Vec<_> = tracks
        .iter()
        .map(|track| {
            (
                track.kind(),
                track.language().as_str(),
                track.display_name(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (SubtitleKind::Manual, "en", Some("English")),
            (SubtitleKind::AutoTranslated, "ru", Some("Russian")),
        ]
    );

    let handle = prepared.source_state.web_subtitle_fetch_handle();
    let fetched = handle
        .fetch(
            &WebSubtitleTrackId::new(SubtitleKind::Manual, LanguageTag::new("en").expect("tag")),
            WebSubtitleFetchLimits::with_max_bytes(NonZeroUsize::new(4096).expect("limit")),
            &CancellationToken::new(),
        )
        .expect("subtitle download through prepared source state");
    assert_eq!(fetched.format(), WebSubtitleFormat::Srv3);
    assert_eq!(fetched.bytes(), SRV3_BODY.as_bytes());
}
