//! Качество страницы для окна (13c): просьба «показать страницу» уходит странице и даёт
//! окну одноразовое слово; факты меню — битрейт только показанного поколения сегментов и
//! причина, по которой страницу нельзя попросить.

use browser_bridge_protocol::ProtocolVersion;
use pushed_media_source::{
    PushedFilePolicy, RangeRequestIds, RangeRequestNotifier, RangeRetryPolicy, pushed_file,
};

use super::*;
use crate::browser_session::session_media::generation_feed::LoadedFeed;
use crate::browser_session::session_media::{PageViewNotice, page_quality::SHOW_PAGE_SENT_LINE};
use crate::page_quality::{PageQualityFacts, PageViewOffer};
use crate::viewer_transport::ViewerTransportIntent;

/// Политика окна: пауза ждёт зрителя (как в тестах досмотра).
const WINDOW_POLICY: DrainPolicy = DrainPolicy {
    stall_timeout: Duration::from_millis(400),
    poll_interval: Duration::from_millis(250),
    paused: PausedDrain::WaitForViewer,
};

/// Загруженное и показанное поколение на протоколе `version`, позиция плеера 100 мс.
fn shown_harness(version: ProtocolVersion) -> (Harness, SessionRef, PushedStreamWriter) {
    let (mut harness, current, writer) = loaded_harness();
    harness.media.on_session_attached(&current, version);
    harness.player.show_load(
        harness.player.load_ticket(0),
        Duration::from_millis(100),
        PlaybackState::Playing,
    );
    harness.media.drive(Instant::now());
    harness.page.take_sent();
    (harness, current, writer)
}

fn ask_to_show_page(harness: &mut Harness) {
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::ShowPage, Instant::now());
    // Проход, на котором очередь уходит странице.
    harness.media.drive(Instant::now());
}

fn show_page_count(harness: &Harness) -> usize {
    harness
        .page
        .take_sent()
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::ShowPage {}))
        .count()
}

/// Главное: на 1.6 просьба зрителя доходит странице ровно один раз, окну — слово один раз.
#[test]
fn at_1_6_the_viewer_request_reaches_the_page_and_the_window_is_told_once() {
    let (mut harness, _current, _writer) = shown_harness(ProtocolVersion::V1_6);
    ask_to_show_page(&mut harness);
    assert_eq!(show_page_count(&harness), 1);
    assert_eq!(
        harness.media.take_page_view_notice(),
        PageViewNotice::Requested
    );
    assert_eq!(
        harness.media.take_page_view_notice(),
        PageViewNotice::Nothing
    );
    assert!(SHOW_PAGE_SENT_LINE.starts_with("Show page"));
}

/// Экстеншен старее 1.6: ничего не уходит, тоста нет (иначе зритель ждал бы вкладку зря).
#[test]
fn with_an_old_extension_nothing_is_sent_and_no_notice_is_given() {
    let (mut harness, _current, _writer) = shown_harness(ProtocolVersion::V1_5);
    ask_to_show_page(&mut harness);
    assert_eq!(show_page_count(&harness), 0);
    assert_eq!(
        harness.media.take_page_view_notice(),
        PageViewNotice::Nothing
    );
}

/// Страница ушла (досмотр): просить некого — ничего не уходит и тоста нет.
#[test]
fn when_the_page_is_gone_nothing_is_sent_and_no_notice_is_given() {
    let (mut harness, current, _writer) = shown_harness(ProtocolVersion::V1_6);
    harness.media.drain_policy = WINDOW_POLICY;
    harness.media.on_session_ended(&current, TAB_GONE_END);
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: current.clone(),
        }],
        None,
    );
    harness.media.drive(Instant::now());
    harness.page.take_sent();
    ask_to_show_page(&mut harness);
    assert_eq!(show_page_count(&harness), 0);
    assert_eq!(
        harness.media.take_page_view_notice(),
        PageViewNotice::Nothing
    );
}

/// Просьба не трогает чужое: плееру не уходит ни одной команды.
#[test]
fn the_request_gives_the_player_no_command() {
    let (mut harness, _current, _writer) = shown_harness(ProtocolVersion::V1_6);
    harness.player.take_calls();
    ask_to_show_page(&mut harness);
    assert_eq!(harness.player.take_calls(), vec![]);
}

/// Битрейт показанного, приведённого к странице поколения сегментов — число; пока плеер
/// ещё показывает прежнее медиа — «неизвестно».
#[test]
fn bitrate_is_known_only_for_the_shown_reconciled_segments_generation() {
    let (mut harness, current, _writer) = loaded_harness();
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_6);
    // Поколение загружено и согласовано, но позиция плеера ещё не доходила: нет и веса.
    let before = harness.media.page_quality_facts();
    assert_eq!(before.page_view, PageViewOffer::Available);

    harness.player.show_load(
        harness.player.load_ticket(0),
        Duration::from_millis(100),
        PlaybackState::Playing,
    );
    harness.media.drive(Instant::now());
    let shown = harness.media.page_quality_facts();
    let bitrate = shown.bitrate.expect("the shown segments have a weight");
    assert!(bitrate.bits_per_second() > 0);

    // Открывается новое поколение: цифра прежнего ролика не показывается.
    let (_second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    harness.media.drive(Instant::now());
    assert_eq!(harness.media.page_quality_facts().bitrate, None);
}

/// Файл `<video src>` и HLS веса не имеют, даже когда показаны и приведены к странице.
#[test]
fn a_file_generation_has_no_bitrate() {
    struct Silent;
    impl RangeRequestNotifier for Silent {
        fn range_requested(&self) {}
    }
    let (mut harness, _current, _writer) = shown_harness(ProtocolVersion::V1_6);
    assert!(harness.media.page_quality_facts().bitrate.is_some());

    let prefetch =
        media_prefetch::PrefetchConfig::new(2 * 1024, 4 * 1024, 8 * 1024).expect("prefetch");
    let policy = PushedFilePolicy::new(
        prefetch,
        RangeRetryPolicy::with_budget(Duration::from_secs(5)),
    )
    .expect("policy");
    let (_writer, _opener, requests) =
        pushed_file(policy, RangeRequestIds::new(), std::sync::Arc::new(Silent));
    harness
        .media
        .loaded
        .as_mut()
        .expect("loaded generation")
        .feed = LoadedFeed::File { requests };
    assert_eq!(harness.media.page_quality_facts().bitrate, None);
}

/// Предложение страницы: до `Attached` — не подключена; 1.5 — экстеншен устарел; 1.6 —
/// можно; страница ушла — ушла.
#[test]
fn the_page_view_offer_tells_why_the_page_cannot_be_asked() {
    let mut harness = Harness::new();
    assert_eq!(
        harness.media.page_quality_facts().page_view,
        PageViewOffer::PageAway,
        "no session at all"
    );
    let current = session(1);
    harness.media.on_session_opened(&current);
    assert_eq!(
        harness.media.page_quality_facts().page_view,
        PageViewOffer::NotAttached
    );
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_5);
    assert_eq!(
        harness.media.page_quality_facts().page_view,
        PageViewOffer::ExtensionTooOld
    );
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_6);
    assert_eq!(
        harness.media.page_quality_facts(),
        PageQualityFacts {
            bitrate: None,
            page_view: PageViewOffer::Available
        }
    );
    harness.media.drain_policy = WINDOW_POLICY;
    harness.media.on_session_ended(&current, TAB_GONE_END);
    harness.media.drive(Instant::now());
    assert_eq!(
        harness.media.page_quality_facts().page_view,
        PageViewOffer::PageAway
    );
}
