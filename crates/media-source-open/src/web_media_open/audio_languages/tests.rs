//! Языки звука yt-dlp: каталог, действия и сквозное «звучит выбранный язык».
//!
//! Подставной yt-dlp отдаёт одно немое видео и три аудио (en-US оригинал 440 Гц,
//! ru 880 Гц, de 1320 Гц), настоящий loopback HTTP-сервер раздаёт крошечные файлы
//! (`tests/fixtures/yt-dlp-audio-languages/`), а `prepare_yt_dlp_web_media` проходит весь
//! боевой путь: снимок, раздельная пара, demux, каталог. Проверяются наблюдаемые вещи:
//! список языков, какое намерение открытия получилось, и частота декодированного звука.

use std::time::{Duration, Instant};

use audio::{
    AudioDecodeCapabilityProvider, AudioDecoderConfig, EncodedAudioPacket,
    ProductionAudioDecoderFactory, create_audio_decoder,
};
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use fastiplayer_config::{
    NetworkConfig, PlayerDemuxConfig, VideoCodec, WebMediaConfig, YtDlpConfig,
};
use media_core::{DemuxReadEvent, TrackKind};
use serde_json::{Value, json};
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;
use web_media_core::{ExtractorInvocationReason, StreamLayout};

use super::{WebAudioLanguageChoice, WebAudioLanguageResolution};
use crate::web_media_open::{
    PreparedYtDlpWebMedia, YtDlpCandidateOpenIntent, prepare_yt_dlp_web_media,
};
use crate::web_subtitle_fetch::tests::origin::{LoopbackOrigin, Reply};
use crate::web_subtitle_fetch::tests::yt_dlp_script::FakeYtDlp;

const VIDEO_MP4: &[u8] =
    include_bytes!("../../../tests/fixtures/yt-dlp-audio-languages/video-only.mp4");
const AUDIO_ORIGINAL: &[u8] =
    include_bytes!("../../../tests/fixtures/yt-dlp-audio-languages/audio-original.mp3");
const AUDIO_RU: &[u8] =
    include_bytes!("../../../tests/fixtures/yt-dlp-audio-languages/audio-dub-a.mp3");
const AUDIO_DE: &[u8] =
    include_bytes!("../../../tests/fixtures/yt-dlp-audio-languages/audio-dub-b.mp3");

/// Максимальное время ожидания demux-а в тесте.
const DEMUX_DEADLINE: Duration = Duration::from_secs(20);

/// Capability snapshot ровно под H.264 фикстуру (64x64 Baseline, 10 к/с).
fn h264_fixture_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("dub_fixture").expect("valid backend ID");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::H264,
            profile: VideoProfile::H264(H264Profile::Baseline),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(64),
            max_height: Some(64),
            max_fps: Some(10.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "Dub fixture backend".to_owned(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: vec![output.clone()],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: vec![output],
    }
}

/// Какие аудио-форматы попадут в JSON yt-dlp.
struct DubbedVideo {
    origin: LoopbackOrigin,
    fake: FakeYtDlp,
}

/// Одно аудио-описание: тег языка и предпочтение извлекателя.
struct AudioFormat {
    format_id: &'static str,
    language: &'static str,
    language_preference: i64,
    path: &'static str,
}

const ORIGINAL: AudioFormat = AudioFormat {
    format_id: "audio-en",
    language: "en-US",
    language_preference: 10,
    path: "/audio-en.mp3",
};
const RUSSIAN: AudioFormat = AudioFormat {
    format_id: "audio-ru",
    language: "ru",
    language_preference: -1,
    path: "/audio-ru.mp3",
};
const GERMAN: AudioFormat = AudioFormat {
    format_id: "audio-de",
    language: "de",
    language_preference: -1,
    path: "/audio-de.mp3",
};

impl DubbedVideo {
    /// Немое видео плюс перечисленные аудио-форматы.
    fn start(audio_formats: &[&AudioFormat]) -> Self {
        let origin = LoopbackOrigin::start();
        origin.route("/video.mp4", Reply::ok(VIDEO_MP4));
        origin.route("/audio-en.mp3", Reply::ok(AUDIO_ORIGINAL));
        origin.route("/audio-ru.mp3", Reply::ok(AUDIO_RU));
        origin.route("/audio-de.mp3", Reply::ok(AUDIO_DE));
        let base = origin.base_url();
        let mut formats = vec![json!({
            "format_id": "video-h264", "url": format!("{base}/video.mp4"), "protocol": "http",
            "ext": "mp4", "container": "mp4_dash", "vcodec": "avc1.42000a", "acodec": "none",
            "width": 64, "height": 64, "fps": 10, "dynamic_range": "SDR"
        })];
        formats.extend(audio_formats.iter().map(|audio| audio_entry(&base, audio)));
        // Как у YouTube: yt-dlp по умолчанию выбирает пару «видео + оригинал» (`requested_formats`).
        let default_pair = vec![formats[0].clone(), formats[1].clone()];
        let default_pair_id = format!(
            "{}+{}",
            default_pair[0]["format_id"].as_str().unwrap_or_default(),
            default_pair[1]["format_id"].as_str().unwrap_or_default()
        );
        let document = json!({
            "id": "dub", "title": "Dubbed", "formats": formats,
            "requested_formats": default_pair, "format_id": default_pair_id
        })
        .to_string();
        Self {
            fake: FakeYtDlp::new(&document),
            origin,
        }
    }

    /// Боевой `prepare_yt_dlp_web_media` с заданным намерением.
    fn prepare(&self, intent: YtDlpCandidateOpenIntent) -> PreparedYtDlpWebMedia {
        let locator = service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/dub")
            .expect("locator");
        let reason = match &intent {
            YtDlpCandidateOpenIntent::BestPlayable => {
                ExtractorInvocationReason::PageMediaResolution
            }
            _ => ExtractorInvocationReason::ExtractorBackedRecovery,
        };
        prepare_yt_dlp_web_media(
            &locator,
            &NetworkConfig::default(),
            &WebMediaConfig::default(),
            &YtDlpConfig {
                resolve_timeout_ms: 10_000,
                ..YtDlpConfig::default()
            },
            &self.fake.adapter(),
            &PlayerDemuxConfig::default(),
            &[VideoCodec::H264],
            &h264_fixture_capabilities(),
            ProductionAudioDecoderFactory::default().audio_decode_capability_snapshot(),
            intent,
            reason,
            CancellationToken::new(),
            || false,
        )
        .expect("dubbed video must prepare")
    }

    /// Сколько раз клиент запросил путь (доказывает, что играл именно этот файл).
    fn hits(&self, path: &str) -> usize {
        self.origin.hits(path)
    }
}

fn audio_entry(base: &str, audio: &AudioFormat) -> Value {
    json!({
        "format_id": audio.format_id, "url": format!("{base}{}", audio.path), "protocol": "http",
        "ext": "mp3", "container": "mp3", "vcodec": "none", "acodec": "mp3", "abr": 32,
        "language": audio.language, "language_preference": audio.language_preference
    })
}

fn languages(choices: &[WebAudioLanguageChoice]) -> Vec<(String, bool, bool)> {
    let mut summary: Vec<_> = choices
        .iter()
        .map(|choice| {
            (
                choice.language.as_str().to_owned(),
                choice.is_original,
                choice.is_active,
            )
        })
        .collect();
    summary.sort();
    summary
}

fn choice_for<'a>(
    choices: &'a [WebAudioLanguageChoice],
    language: &str,
) -> &'a WebAudioLanguageChoice {
    choices
        .iter()
        .find(|choice| choice.language.as_str() == language)
        .unwrap_or_else(|| panic!("language {language} must be offered"))
}

/// Частота (Гц) моно-синуса по числу переходов через ноль снизу вверх.
fn dominant_frequency(prepared: &mut PreparedYtDlpWebMedia) -> f32 {
    let track = prepared
        .demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("composed source must carry an audio track")
        .clone();
    let mut decoder = create_audio_decoder(AudioDecoderConfig::from_track_metadata(
        track.id.get(),
        track.codec_id.clone(),
        track.sample_rate,
        track.channels,
    ))
    .expect("production decoder for mp3");
    let mut samples = Vec::new();
    let deadline = Instant::now() + DEMUX_DEADLINE;
    loop {
        assert!(Instant::now() < deadline, "demux must reach EOS in time");
        match prepared.demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => {
                let decoded = decoder
                    .decode(&EncodedAudioPacket::without_timing(
                        track.id.get(),
                        &packet.data,
                    ))
                    .expect("decode mp3 packet");
                samples.extend_from_slice(&decoded);
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    let spec = decoder.decoded_output_spec().expect("decoded spec");
    let channels = spec.channel_layout.channel_count() as usize;
    let mono: Vec<f32> = samples.chunks(channels).map(|frame| frame[0]).collect();
    assert!(mono.len() > 1_000, "decoded audio must not be empty");
    let rising_crossings = mono
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    rising_crossings as f32 * spec.sample_rate as f32 / mono.len() as f32
}

fn assert_hertz_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < expected * 0.05,
        "expected about {expected} Hz, got {actual} Hz"
    );
}

#[test]
fn three_languages_are_offered_with_original_active_by_default() {
    let video = DubbedVideo::start(&[&ORIGINAL, &RUSSIAN, &GERMAN]);
    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    let choices = prepared.source_state.audio_language_choices();

    assert_eq!(
        languages(&choices),
        [
            ("de".to_owned(), false, false),
            ("en-US".to_owned(), true, true),
            ("ru".to_owned(), false, false),
        ]
    );
}

#[test]
fn choosing_russian_builds_a_composed_open_intent_with_russian_audio() {
    let video = DubbedVideo::start(&[&ORIGINAL, &RUSSIAN, &GERMAN]);
    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);
    let choices = prepared.source_state.audio_language_choices();

    let resolution = prepared
        .source_state
        .audio_language_switch_intent(choice_for(&choices, "ru").action);

    let WebAudioLanguageResolution::Ready(YtDlpCandidateOpenIntent::Composed(composed)) =
        resolution
    else {
        panic!("a different language must reopen as a composed pair");
    };
    let StreamLayout::Separate { audio, .. } = composed.selection().descriptor().layout() else {
        panic!("composed selection must be a separate video + audio pair");
    };
    assert_eq!(audio.audio().language().map(|tag| tag.as_str()), Some("ru"));
    // Видео остаётся тем же: parent — это video-only строка пары.
    assert_eq!(
        composed.parent_preference(),
        composed.selection().video_parent_selection()
    );
}

#[test]
fn choosing_the_active_language_is_no_change() {
    let video = DubbedVideo::start(&[&ORIGINAL, &RUSSIAN]);
    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);
    let choices = prepared.source_state.audio_language_choices();
    let active = choices
        .iter()
        .find(|choice| choice.is_active)
        .expect("active");

    assert_eq!(
        prepared
            .source_state
            .audio_language_switch_intent(active.action),
        WebAudioLanguageResolution::NoChange
    );
}

#[test]
fn an_action_from_a_previous_open_is_stale() {
    let video = DubbedVideo::start(&[&ORIGINAL, &RUSSIAN]);
    let first = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);
    let old_action = choice_for(&first.source_state.audio_language_choices(), "ru").action;
    let second = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    assert_eq!(
        second.source_state.audio_language_switch_intent(old_action),
        WebAudioLanguageResolution::Stale
    );
}

#[test]
fn a_single_language_offers_no_choice() {
    let video = DubbedVideo::start(&[&ORIGINAL]);
    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    assert!(prepared.source_state.audio_language_choices().is_empty());
}

#[test]
fn a_muxed_active_candidate_offers_no_choice() {
    // Один формат без раздельных дорожек: звук нельзя менять отдельно от видео.
    let origin = LoopbackOrigin::start();
    origin.route("/media.mp3", Reply::ok(AUDIO_ORIGINAL));
    let document = json!({"id": "m", "title": "Muxed", "formats": [{
        "format_id": "muxed", "url": format!("{}/media.mp3", origin.base_url()),
        "protocol": "http", "ext": "mp3", "container": "mp3", "vcodec": null, "acodec": null,
        "language": "en-US"
    }]})
    .to_string();
    let fake = FakeYtDlp::new(&document);
    let video = DubbedVideo { origin, fake };

    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    assert!(prepared.source_state.audio_language_choices().is_empty());
}

#[test]
fn equal_language_preferences_do_not_mark_an_original() {
    let tied_russian = AudioFormat {
        language_preference: 10,
        ..RUSSIAN
    };
    let video = DubbedVideo::start(&[&ORIGINAL, &tied_russian]);
    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    let marked: Vec<bool> = prepared
        .source_state
        .audio_language_choices()
        .iter()
        .map(|choice| choice.is_original)
        .collect();

    assert_eq!(marked, [false, false]);
}

#[test]
fn a_tied_top_preference_does_not_mark_an_original_even_if_a_lower_language_exists() {
    let tied_russian = AudioFormat {
        language_preference: 10,
        ..RUSSIAN
    };
    let video = DubbedVideo::start(&[&ORIGINAL, &tied_russian, &GERMAN]);
    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    let choices = prepared.source_state.audio_language_choices();

    assert_eq!(choices.len(), 3);
    assert!(choices.iter().all(|choice| !choice.is_original));
}

/// Вертикаль «звучит выбранный язык»: оригинал 440 Гц -> действие ru -> переоткрытие -> 880 Гц.
#[test]
fn selected_language_is_what_the_decoder_produces() {
    let video = DubbedVideo::start(&[&ORIGINAL, &RUSSIAN, &GERMAN]);
    let mut original = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);
    assert_hertz_close(dominant_frequency(&mut original), 440.0);
    assert!(
        video.hits("/audio-en.mp3") > 0,
        "original audio was fetched"
    );
    assert_eq!(
        video.hits("/audio-ru.mp3"),
        0,
        "dub is not fetched before the switch"
    );

    let choices = original.source_state.audio_language_choices();
    let WebAudioLanguageResolution::Ready(intent) = original
        .source_state
        .audio_language_switch_intent(choice_for(&choices, "ru").action)
    else {
        panic!("Russian must be a real switch");
    };
    let mut dubbed = video.prepare(intent);

    assert_hertz_close(dominant_frequency(&mut dubbed), 880.0);
    assert!(video.hits("/audio-ru.mp3") > 0, "Russian audio was fetched");
    // После переоткрытия активным стал русский, а оригинал остался помеченным.
    assert_eq!(
        languages(&dubbed.source_state.audio_language_choices()),
        [
            ("de".to_owned(), false, false),
            ("en-US".to_owned(), true, false),
            ("ru".to_owned(), false, true),
        ]
    );
}

/// Языки авто-дубляжа YouTube (по числу и форме похожи на реальный ролик с 20 языками).
const AUTO_DUB_LANGUAGES: [&str; 20] = [
    "en-US", "ar", "bn", "de-DE", "es-US", "fr-FR", "hi", "it", "iw", "ja", "ko", "ml", "nl-NL",
    "pa", "pl", "pt-BR", "ru", "ta", "te", "uk",
];

/// Число целевых языков автоперевода на каждый исходный язык (у YouTube их около 160).
const TRANSLATION_TARGETS_PER_SOURCE: usize = 60;

/// Запись `automatic_captions` в форме YouTube: распознанная речь с `variant`, либо перевод.
fn youtube_caption_entry(base: &str, extension: &str, query: &str) -> Value {
    json!({
        "ext": extension, "protocol": "https", "name": "caption",
        "url": format!("{base}/timedtext?v=dub&expire=4102444800&{query}")
    })
}

/// JSON ролика с авто-дубляжем: `requested_formats` = видео `137` + аудио `251-19` (en-US),
/// каждый язык дубляжа имеет аудио-строку и распознанную речь `<язык>-orig`,
/// а переводов у каждого исходного языка десятки (документ получается очень большим).
fn youtube_auto_dub_document(base: &str) -> String {
    let video = json!({
        "format_id": "137", "url": format!("{base}/video.mp4"), "protocol": "https",
        "ext": "mp4", "container": "mp4_dash", "vcodec": "avc1.42000a", "acodec": "none",
        "width": 64, "height": 64, "fps": 10, "dynamic_range": "SDR", "language_preference": -1
    });
    let mut formats = vec![video.clone()];
    let mut original_audio = Value::Null;
    for (index, language) in AUTO_DUB_LANGUAGES.iter().enumerate() {
        let is_original = index == 0;
        let audio = json!({
            "format_id": format!("251-{index}"), "url": format!("{base}/audio-en.mp3"),
            "protocol": "https", "ext": "mp3", "container": "mp3", "vcodec": "none",
            "acodec": "mp3", "abr": 32, "language": language,
            "language_preference": if is_original { 10 } else { -1 }
        });
        if is_original {
            original_audio = audio.clone();
        }
        formats.push(audio);
    }
    let mut automatic_captions = serde_json::Map::new();
    for language in AUTO_DUB_LANGUAGES {
        let short = language.split('-').next().unwrap_or(language);
        automatic_captions.insert(
            format!("{short}-orig"),
            json!([
                youtube_caption_entry(
                    base,
                    "srv3",
                    &format!("kind=asr&lang={short}&variant=timing-optimized")
                ),
                youtube_caption_entry(
                    base,
                    "vtt",
                    &format!("kind=asr&lang={short}&variant=timing-optimized")
                ),
            ]),
        );
        for target in 0..TRANSLATION_TARGETS_PER_SOURCE {
            let query = format!("kind=asr&lang={short}&tlang=t{target:03}");
            automatic_captions
                .entry(format!("t{target:03}"))
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("array")
                .push(youtube_caption_entry(base, "vtt", &query));
        }
    }
    json!({
        "id": "dub", "title": "Auto dub", "formats": formats,
        "requested_formats": [video, original_audio], "format_id": "137+251-0",
        "subtitles": {}, "automatic_captions": automatic_captions
    })
    .to_string()
}

/// Регрессия живой приёмки: ролик YouTube с авто-дубляжем обязан показать и языки звука
/// (оригинал активен), и распознанную речь на каждом языке, и автопереводы.
#[test]
fn youtube_auto_dub_shape_offers_audio_languages_and_caption_tracks() {
    let origin = LoopbackOrigin::start();
    origin.route("/video.mp4", Reply::ok(VIDEO_MP4));
    origin.route("/audio-en.mp3", Reply::ok(AUDIO_ORIGINAL));
    let document = youtube_auto_dub_document(&origin.base_url());
    let video = DubbedVideo {
        fake: FakeYtDlp::new(&document),
        origin,
    };

    let prepared = video.prepare(YtDlpCandidateOpenIntent::BestPlayable);

    let choices = prepared.source_state.audio_language_choices();
    assert_eq!(choices.len(), AUTO_DUB_LANGUAGES.len());
    let original = choice_for(&choices, "en-US");
    assert!(original.is_original && original.is_active);
    assert_eq!(choices.iter().filter(|choice| choice.is_active).count(), 1);
    let tracks = prepared
        .source_state
        .stream_configuration()
        .web_subtitle_tracks();
    let recognized = tracks
        .iter()
        .filter(|track| track.kind() == web_media_core::SubtitleKind::AutoGenerated)
        .count();
    let translated = tracks
        .iter()
        .filter(|track| track.kind() == web_media_core::SubtitleKind::AutoTranslated)
        .count();
    assert_eq!(recognized, AUTO_DUB_LANGUAGES.len());
    assert_eq!(translated, TRANSLATION_TARGETS_PER_SOURCE);
}
