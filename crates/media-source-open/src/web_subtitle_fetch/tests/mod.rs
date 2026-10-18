//! Сквозные тесты загрузки субтитров: подставной yt-dlp, настоящий HTTP по loopback.
//!
//! Проверяются наблюдаемые результаты (байты, формат, тип ошибки, число запросов
//! к серверу и запусков yt-dlp), а не вызовы внутренних функций.

pub(crate) mod origin;
mod prepare;
pub(crate) mod yt_dlp_script;

use std::num::NonZeroUsize;
use std::time::Duration;

use fastiplayer_config::{NetworkConfig, YtDlpConfig};
use serde_json::{Value, json};
use service_ytdlp::parse_yt_dlp_media_locator;
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_core::{
    ExtractionGeneration, ExtractorInvocationReason, LanguageTag, SourceIdentity, SubtitleKind,
};

use self::origin::{LoopbackOrigin, Reply};
use self::yt_dlp_script::FakeYtDlp;
use super::{
    FetchedWebSubtitle, WebSubtitleFetchError, WebSubtitleFetchHandle, WebSubtitleFetchLimits,
    WebSubtitleFormat, WebSubtitleTrackId, YtDlpSubtitleRefresher,
};

/// Подпись, которая "живёт" до 2100 года.
const FAR_EXPIRE: u64 = 4_102_444_800;
/// Подпись, давно протухшая по локальным часам.
const PAST_EXPIRE: u64 = 1;

const SRV3_BODY: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3"><body><p t="0" d="1000">Hi</p></body></timedtext>"#;
const VTT_BODY: &str = "WEBVTT\n\n00:00.000 --> 00:01.000\nHi\n";

/// Одна запись `{ext, url, name}` с подписанной ссылкой на loopback origin.
fn entry(base: &str, path: &str, ext: &str, name: &str, query: &str, expire: u64) -> Value {
    json!({
        "ext": ext,
        "name": name,
        "url": format!("{base}{path}.{ext}?expire={expire}{query}&signature=SECRET"),
    })
}

/// JSON yt-dlp с тремя дорожками: ручная en, распознанная en, автоперевод ru.
/// `tag` входит в путь, поэтому "свежие" ссылки отличаются от протухших.
fn document(base: &str, tag: &str, expire: u64) -> String {
    let both = |path: String, name: &str, query: &str| {
        vec![
            entry(base, &path, "vtt", name, query, expire),
            entry(base, &path, "srv3", name, query, expire),
            entry(base, &path, "ttml", name, query, expire),
        ]
    };
    json!({
        "id": "x",
        "title": "t",
        "subtitles": {
            "en": both(format!("/{tag}/manual-en"), "English", ""),
            "live_chat": [{"ext": "json", "protocol": "youtube_live_chat", "url": format!("{base}/chat")}],
        },
        "automatic_captions": {
            "en": both(format!("/{tag}/asr-en"), "English", "&kind=asr"),
            "ru": both(format!("/{tag}/tr-ru"), "Russian", "&kind=asr&lang=en&tlang=ru"),
        },
    })
    .to_string()
}

fn manual_en() -> WebSubtitleTrackId {
    track(SubtitleKind::Manual, "en")
}

fn translated_ru() -> WebSubtitleTrackId {
    track(SubtitleKind::AutoTranslated, "ru")
}

fn track(kind: SubtitleKind, language: &str) -> WebSubtitleTrackId {
    WebSubtitleTrackId::new(kind, LanguageTag::new(language).expect("language"))
}

fn limits(max_bytes: usize) -> WebSubtitleFetchLimits {
    WebSubtitleFetchLimits::with_max_bytes(NonZeroUsize::new(max_bytes).expect("non zero"))
}

fn roomy_limits() -> WebSubtitleFetchLimits {
    limits(64 * 1024)
}

fn yt_dlp_config() -> YtDlpConfig {
    YtDlpConfig {
        resolve_timeout_ms: 10_000,
        ..YtDlpConfig::default()
    }
}

/// Origin + подставной yt-dlp + дескриптор, собранный из настоящего снимка.
struct Fixture {
    origin: LoopbackOrigin,
    fake: FakeYtDlp,
    handle: WebSubtitleFetchHandle,
}

impl Fixture {
    fn start(initial_tag: &str, initial_expire: u64) -> Self {
        let origin = LoopbackOrigin::start();
        let fake = FakeYtDlp::new(&document(&origin.base_url(), initial_tag, initial_expire));
        let locator = parse_yt_dlp_media_locator("https://page.example.test/watch?v=1")
            .expect("page locator");
        let adapter = fake.adapter();
        let snapshot = adapter
            .resolve_candidate_snapshot_with_cancellation(
                &locator,
                SourceIdentity::new(1),
                ExtractionGeneration::new(1),
                &yt_dlp_config(),
                ExtractorInvocationReason::PageMediaResolution,
                &|| false,
            )
            .expect("fake yt-dlp snapshot");
        let handle = WebSubtitleFetchHandle::for_yt_dlp_extraction(
            snapshot.subtitle_inventory().clone(),
            YtDlpSubtitleRefresher::new(locator, adapter, yt_dlp_config()),
            SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
                .expect("source config"),
        );
        Self {
            origin,
            fake,
            handle,
        }
    }

    fn fetch(&self, id: &WebSubtitleTrackId) -> Result<FetchedWebSubtitle, WebSubtitleFetchError> {
        self.handle
            .fetch(id, roomy_limits(), &CancellationToken::new())
    }

    /// Следующий ответ yt-dlp: ссылки с другим тегом пути и сроком.
    fn script_refresh(&self, tag: &str, expire: u64) {
        self.fake
            .set_json(&document(&self.origin.base_url(), tag, expire));
    }
}

/// Список дорожек безопасен и упорядочен: форматы srv3 -> vtt, чат отфильтрован.
#[test]
fn handle_lists_tracks_with_kinds_names_and_format_order() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    let tracks = fixture.handle.tracks();
    let summary: Vec<_> = tracks
        .iter()
        .map(|track| (track.kind(), track.language().as_str().to_owned()))
        .collect();
    assert_eq!(
        summary,
        [
            (SubtitleKind::Manual, "en".to_owned()),
            (SubtitleKind::AutoGenerated, "en".to_owned()),
            (SubtitleKind::AutoTranslated, "ru".to_owned()),
        ]
    );
    assert_eq!(tracks[0].display_name(), Some("English"));
    assert_eq!(
        tracks[2].formats(),
        [WebSubtitleFormat::Srv3, WebSubtitleFormat::WebVtt]
    );
}

/// srv3 отдан сервером: приходят его байты и формат Srv3, vtt не запрашивается.
#[test]
fn srv3_is_preferred_and_vtt_is_not_requested() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.srv3", Reply::ok(SRV3_BODY));
    fixture
        .origin
        .route("/g1/manual-en.vtt", Reply::ok(VTT_BODY));

    let fetched = fixture.fetch(&manual_en()).expect("srv3 fetch");

    assert_eq!(fetched.format(), WebSubtitleFormat::Srv3);
    assert_eq!(fetched.bytes(), SRV3_BODY.as_bytes());
    assert_eq!(fixture.origin.hits("/g1/manual-en.vtt"), 0);
}

/// 404 на srv3 -> запасной WebVTT.
#[test]
fn srv3_not_found_falls_back_to_vtt() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.vtt", Reply::ok(VTT_BODY));

    let fetched = fixture.fetch(&manual_en()).expect("vtt fallback");

    assert_eq!(fetched.format(), WebSubtitleFormat::WebVtt);
    assert_eq!(fetched.bytes(), VTT_BODY.as_bytes());
    assert_eq!(fixture.origin.hits("/g1/manual-en.srv3"), 1);
}

/// 200 с HTML вместо srv3 — тоже провал формата, уходим на vtt.
#[test]
fn non_xml_srv3_body_falls_back_to_vtt() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.srv3", Reply::ok("<html>Sorry...</html>"));
    fixture
        .origin
        .route("/g1/manual-en.vtt", Reply::ok(VTT_BODY));

    let fetched = fixture.fetch(&manual_en()).expect("vtt fallback");

    assert_eq!(fetched.format(), WebSubtitleFormat::WebVtt);
}

/// Пустой srv3 и пустой vtt -> InvalidResponse (ответ есть, но это не субтитры).
#[test]
fn empty_bodies_in_every_format_are_invalid_response() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture.origin.route("/g1/manual-en.srv3", Reply::ok(""));
    fixture.origin.route("/g1/manual-en.vtt", Reply::ok(""));

    assert_eq!(
        fixture.fetch(&manual_en()),
        Err(WebSubtitleFetchError::InvalidResponse)
    );
}

/// Нет ни одного формата на сервере -> NotFound, оба формата опрошены по разу.
#[test]
fn every_format_missing_is_not_found() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);

    assert_eq!(
        fixture.fetch(&manual_en()),
        Err(WebSubtitleFetchError::NotFound)
    );
    assert_eq!(fixture.origin.hits("/g1/manual-en.srv3"), 1);
    assert_eq!(fixture.origin.hits("/g1/manual-en.vtt"), 1);
}

/// 429: возвращаем RateLimited с Retry-After и НЕ идём за vtt.
#[test]
fn rate_limit_is_typed_and_does_not_trigger_vtt_fallback() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture.origin.route(
        "/g1/tr-ru.srv3",
        Reply::status(429).with_header("Retry-After", "7"),
    );
    fixture.origin.route("/g1/tr-ru.vtt", Reply::ok(VTT_BODY));

    assert_eq!(
        fixture.fetch(&translated_ru()),
        Err(WebSubtitleFetchError::RateLimited {
            retry_after: Some(Duration::from_secs(7))
        })
    );
    assert_eq!(fixture.origin.hits("/g1/tr-ru.srv3"), 1);
    assert_eq!(fixture.origin.hits("/g1/tr-ru.vtt"), 0);
}

/// 429 без Retry-After — подсказки нет.
#[test]
fn rate_limit_without_hint_has_no_retry_after() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture.origin.route("/g1/tr-ru.srv3", Reply::status(429));

    assert_eq!(
        fixture.fetch(&translated_ru()),
        Err(WebSubtitleFetchError::RateLimited { retry_after: None })
    );
}

/// 503 — отдельная ошибка статуса, запасной формат не пробуем.
#[test]
fn server_error_is_reported_without_fallback() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.srv3", Reply::status(503));
    fixture
        .origin
        .route("/g1/manual-en.vtt", Reply::ok(VTT_BODY));

    assert_eq!(
        fixture.fetch(&manual_en()),
        Err(WebSubtitleFetchError::UnexpectedStatus { status: 503 })
    );
    assert_eq!(fixture.origin.hits("/g1/manual-en.vtt"), 0);
}

/// Дорожки нет в списке видео: сеть и yt-dlp не трогаются.
#[test]
fn unknown_track_touches_neither_network_nor_yt_dlp() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    let runs_before = fixture.fake.run_count();

    assert_eq!(
        fixture.fetch(&track(SubtitleKind::Manual, "xx")),
        Err(WebSubtitleFetchError::UnknownTrack)
    );
    assert_eq!(fixture.fake.run_count(), runs_before);
    assert_eq!(fixture.origin.hits("/g1/manual-en.srv3"), 0);
}

/// Отмена до начала: ни одного запроса.
#[test]
fn pre_cancelled_fetch_makes_no_request() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.srv3", Reply::ok(SRV3_BODY));
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert_eq!(
        fixture
            .handle
            .fetch(&manual_en(), roomy_limits(), &cancellation),
        Err(WebSubtitleFetchError::Cancelled)
    );
    assert_eq!(fixture.origin.hits("/g1/manual-en.srv3"), 0);
}

/// Отмена во время загрузки srv3 не даёт перейти к запасному формату.
#[test]
fn cancel_during_download_stops_before_fallback_format() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    let cancellation = CancellationToken::new();
    let hook_token = cancellation.clone();
    fixture.origin.route(
        "/g1/manual-en.srv3",
        Reply::status(404).on_hit(move || hook_token.cancel()),
    );
    fixture
        .origin
        .route("/g1/manual-en.vtt", Reply::ok(VTT_BODY));

    assert_eq!(
        fixture
            .handle
            .fetch(&manual_en(), roomy_limits(), &cancellation),
        Err(WebSubtitleFetchError::Cancelled)
    );
    assert_eq!(fixture.origin.hits("/g1/manual-en.vtt"), 0);
}

/// Ответ больше лимита вызывающего -> TooLarge с этим лимитом.
#[test]
fn oversized_body_is_too_large() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.srv3", Reply::ok(SRV3_BODY));

    assert_eq!(
        fixture
            .handle
            .fetch(&manual_en(), limits(16), &CancellationToken::new()),
        Err(WebSubtitleFetchError::TooLarge { limit_bytes: 16 })
    );
}

/// Тело ровно в лимит проходит (граница не off-by-one).
#[test]
fn body_exactly_at_limit_is_accepted() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture
        .origin
        .route("/g1/manual-en.srv3", Reply::ok(SRV3_BODY));

    let fetched = fixture
        .handle
        .fetch(
            &manual_en(),
            limits(SRV3_BODY.len()),
            &CancellationToken::new(),
        )
        .expect("exact limit");
    assert_eq!(fetched.bytes().len(), SRV3_BODY.len());
}

/// Редирект того же origin-а проходит, итоговое тело доставляется.
#[test]
fn redirect_is_followed() {
    let fixture = Fixture::start("g1", FAR_EXPIRE);
    fixture.origin.route(
        "/g1/manual-en.srv3",
        Reply::status(302).with_header("Location", "/moved/en.srv3"),
    );
    fixture.origin.route("/moved/en.srv3", Reply::ok(SRV3_BODY));

    let fetched = fixture.fetch(&manual_en()).expect("redirected fetch");

    assert_eq!(fetched.bytes(), SRV3_BODY.as_bytes());
    assert_eq!(fixture.origin.hits("/moved/en.srv3"), 1);
}

/// Подпись протухла по часам: сеть не трогаем, один раз спрашиваем yt-dlp,
/// берём свежие ссылки и успешно скачиваем.
#[test]
fn locally_expired_links_are_refreshed_once_and_succeed() {
    let fixture = Fixture::start("old", PAST_EXPIRE);
    fixture.script_refresh("new", FAR_EXPIRE);
    fixture
        .origin
        .route("/new/manual-en.srv3", Reply::ok(SRV3_BODY));
    let runs_before = fixture.fake.run_count();

    let fetched = fixture.fetch(&manual_en()).expect("fetch after refresh");

    assert_eq!(fetched.bytes(), SRV3_BODY.as_bytes());
    assert_eq!(fixture.fake.run_count(), runs_before + 1);
    assert_eq!(fixture.origin.hits("/old/manual-en.srv3"), 0);

    // Свежие ссылки сохранены в дескрипторе: следующая загрузка yt-dlp не зовёт.
    fixture.fetch(&manual_en()).expect("second fetch");
    assert_eq!(fixture.fake.run_count(), runs_before + 1);
}

/// 403 от сервера = протухла подпись: тот же сценарий обновления.
#[test]
fn forbidden_response_triggers_one_refresh() {
    let fixture = Fixture::start("old", FAR_EXPIRE);
    fixture
        .origin
        .route("/old/manual-en.srv3", Reply::status(403));
    fixture.script_refresh("new", FAR_EXPIRE);
    fixture
        .origin
        .route("/new/manual-en.srv3", Reply::ok(SRV3_BODY));
    let runs_before = fixture.fake.run_count();

    let fetched = fixture.fetch(&manual_en()).expect("fetch after refresh");

    assert_eq!(fetched.format(), WebSubtitleFormat::Srv3);
    assert_eq!(fixture.origin.hits("/old/manual-en.srv3"), 1);
    assert_eq!(fixture.fake.run_count(), runs_before + 1);
}

/// 410 у всех форматов тоже считается протухшей подписью.
#[test]
fn gone_in_every_format_triggers_refresh() {
    let fixture = Fixture::start("old", FAR_EXPIRE);
    fixture
        .origin
        .route("/old/manual-en.srv3", Reply::status(410));
    fixture
        .origin
        .route("/old/manual-en.vtt", Reply::status(410));
    fixture.script_refresh("new", FAR_EXPIRE);
    fixture
        .origin
        .route("/new/manual-en.srv3", Reply::ok(SRV3_BODY));

    assert!(fixture.fetch(&manual_en()).is_ok());
}

/// Если и после обновления ссылки протухшие — ошибка, и ровно один запуск yt-dlp.
#[test]
fn refresh_happens_at_most_once_per_fetch() {
    let fixture = Fixture::start("old", PAST_EXPIRE);
    fixture.script_refresh("new", PAST_EXPIRE);
    let runs_before = fixture.fake.run_count();

    assert_eq!(
        fixture.fetch(&manual_en()),
        Err(WebSubtitleFetchError::LinkExpired)
    );
    assert_eq!(fixture.fake.run_count(), runs_before + 1);
    assert_eq!(fixture.origin.hits("/new/manual-en.srv3"), 0);
}

/// Сломанный ответ yt-dlp при обновлении — отдельная ошибка.
#[test]
fn broken_yt_dlp_output_is_refresh_failed() {
    let fixture = Fixture::start("old", PAST_EXPIRE);
    fixture.fake.set_json("this is not json");

    assert_eq!(
        fixture.fetch(&manual_en()),
        Err(WebSubtitleFetchError::RefreshFailed)
    );
}

/// После обновления дорожки могло не оказаться: это NotFound, а не UnknownTrack.
#[test]
fn track_missing_after_refresh_is_not_found() {
    let fixture = Fixture::start("old", PAST_EXPIRE);
    fixture
        .fake
        .set_json(r#"{"id":"x","title":"t","subtitles":{}}"#);

    assert_eq!(
        fixture.fetch(&manual_en()),
        Err(WebSubtitleFetchError::NotFound)
    );
}

/// Ни Debug, ни Display дескриптора, результатов и ошибок не содержат секретов.
#[test]
fn formatted_output_never_contains_link_material() {
    let fixture = Fixture::start("old", PAST_EXPIRE);
    fixture.script_refresh("new", FAR_EXPIRE);
    fixture
        .origin
        .route("/new/manual-en.srv3", Reply::ok(SRV3_BODY));
    let fetched = fixture.fetch(&manual_en()).expect("fetch");
    let errors = [
        WebSubtitleFetchError::UnknownTrack,
        WebSubtitleFetchError::RateLimited {
            retry_after: Some(Duration::from_secs(3)),
        },
        WebSubtitleFetchError::LinkExpired,
        WebSubtitleFetchError::UnexpectedStatus { status: 500 },
    ];
    let mut rendered = format!(
        "{:?} {:?} {:?}",
        fixture.handle,
        fetched,
        fixture.handle.tracks()
    );
    for error in &errors {
        rendered.push_str(&format!(" {error:?} {error}"));
    }
    for forbidden in [
        "SECRET",
        "signature",
        "expire=",
        "manual-en",
        "/new/",
        "127.0.0.1",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "formatted output leaked `{forbidden}`: {rendered}"
        );
    }
}

/// Клоны делят состояние: равны между собой, разные видео — нет.
#[test]
fn clones_are_equal_and_distinct_videos_are_not() {
    let first = Fixture::start("g1", FAR_EXPIRE);
    let second = Fixture::start("g1", FAR_EXPIRE);

    assert_eq!(first.handle.clone(), first.handle);
    assert_ne!(first.handle, second.handle);
}
