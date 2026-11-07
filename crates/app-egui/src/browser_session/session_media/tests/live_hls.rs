//! Эфир HLS по номеркам (10a3, `session_media::live_hls`): окно эфира уходит только от
//! показанного эфира и только странице 1.2; перемотка страницы, пока она не получила
//! окно эфира, не исполняется (ни до показа, ни после), у ролика — ждёт показа, как
//! раньше.
//!
//! Открыватель native HLS здесь не запускается (плеер-подделка не знает своих
//! возможностей): тест ставит загруженное поколение сам, с настоящей очередью ресурсов.

use std::sync::Arc;

use browser_bridge_protocol::{LiveSeekable, ProtocolVersion};
use pushed_media_source::{
    RangeRequestIds, RangeRequestNotifier, ResourceFetchWriter, resource_fetch_queue,
};

use super::*;
use crate::browser_session::hls_bridge::factory::bridge_resource_policy;
use crate::browser_session::hls_bridge::opener::HlsPresentation;

struct Silent;
impl RangeRequestNotifier for Silent {
    fn range_requested(&self) {}
}

/// Окно эфира, которое «видит» плеер-подделка.
const WINDOW: LiveWindowObservation = LiveWindowObservation::LiveWithDvr {
    edge: Duration::from_secs(20),
    start: Duration::from_secs(8),
    end: Duration::from_secs(19),
};

/// Цель перемотки страницы в тестах.
const PAGE_TARGET: Duration = Duration::from_secs(3_600);

/// Харнесс с открытым сеансом (страница этой версии) и загруженным HLS-поколением 1
/// вида `presentation`, ещё не показанным. Возвращает и квитанцию загрузки, и писателя
/// очереди ресурсов (держать до конца теста: его уничтожение печатает очередь).
fn harness_with_loaded_hls(
    presentation: HlsPresentation,
    version: ProtocolVersion,
) -> (Harness, MediaLoadTicket, ResourceFetchWriter) {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    harness.media.on_session_attached(&current, version);
    let config = fastiplayer_config::AppConfig::default();
    let policy = bridge_resource_policy(&config.network).expect("policy");
    let (writer, _client, requests) =
        resource_fetch_queue(policy, RangeRequestIds::new(), Arc::new(Silent));
    let load_ticket = harness.player.tickets.borrow_mut().issue();
    harness.media.loaded = Some(LoadedGeneration {
        generation: generation(1),
        load_ticket,
        feed: LoadedFeed::Hls {
            requests,
            presentation,
        },
        fault_reported: false,
        page_state_reconciled: false,
        seek_when_shown: None,
        start: GenerationStart::FirstCommonData,
        positioned_at: None,
        unanswered_seek: None,
    });
    harness.player.observation.borrow_mut().live_window = WINDOW;
    (harness, load_ticket, writer)
}

/// Окна эфира среди ушедшего странице.
fn live_windows(messages: &[PlayerSessionMessage]) -> Vec<&PlayerSessionMessage> {
    messages
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::LiveWindow { .. }))
        .collect()
}

fn seeks(calls: &[PlayerCall]) -> Vec<Duration> {
    calls
        .iter()
        .filter_map(|call| match call {
            PlayerCall::Seek(target) => Some(*target),
            _ => None,
        })
        .collect()
}

/// Показанный эфир у страницы 1.2: окно уходит с номером загруженного поколения и в
/// том же времени, что часы; часы при этом идут как обычно.
#[test]
fn a_shown_live_generation_tells_a_1_2_page_its_window() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);

    harness.media.drive(Instant::now());

    let sent = harness.page.take_sent();
    assert_eq!(
        live_windows(&sent),
        vec![&PlayerSessionMessage::LiveWindow {
            generation: generation(1),
            edge_us: MediaTime::from_micros(20_000_000).expect("edge"),
            seekable: LiveSeekable::Range {
                start_us: MediaTime::from_micros(8_000_000).expect("start"),
                end_us: MediaTime::from_micros(19_000_000).expect("end"),
            },
        }]
    );
    assert!(
        sent.iter().any(|message| matches!(
            message,
            PlayerSessionMessage::Clock { position_us, .. } if position_us.as_micros() == 17_000_000
        )),
        "the clock of the shown generation still goes: {sent:?}"
    );
}

/// Страница 1.1 окна не получает: её декодер не знает `live_window`.
#[test]
fn a_1_1_page_gets_no_window_from_a_shown_live_generation() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_1);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);

    harness.media.drive(Instant::now());
    harness.media.drive(Instant::now() + Duration::from_secs(2));

    let sent = harness.page.take_sent();
    assert!(live_windows(&sent).is_empty(), "{sent:?}");
    assert!(
        sent.iter()
            .any(|message| matches!(message, PlayerSessionMessage::Clock { .. })),
        "the 1.1 page still hears the clock"
    );
}

/// Пока плеер ставит эфир, окна нет (наблюдение ещё про прежнее медиа); первое окно —
/// в проходе, где поколение показано.
#[test]
fn no_window_goes_before_the_live_generation_is_shown() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);

    harness.media.drive(Instant::now());
    assert!(live_windows(&harness.page.take_sent()).is_empty());

    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    assert_eq!(live_windows(&harness.page.take_sent()).len(), 1);
}

/// HLS-ролик окна не шлёт, даже если наблюдение вдруг говорит «эфир»: окно — только у
/// эфира по номеркам.
#[test]
fn a_vod_hls_generation_sends_no_window() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::OnDemand, ProtocolVersion::V1_2);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);

    harness.media.drive(Instant::now());

    assert!(live_windows(&harness.page.take_sent()).is_empty());
}

/// Перемотка страницы до показа эфира — в её времени, не в времени плеера: не копится
/// до показа и не исполняется после него. Часы и окно потом — как обычно.
#[test]
fn a_page_seek_before_the_live_generation_is_shown_is_dropped() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);

    harness.media.follow_page_seek(PAGE_TARGET);
    assert_eq!(
        harness
            .media
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.seek_when_shown),
        None
    );
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());

    assert!(seeks(&harness.player.take_calls()).is_empty());
    let sent = harness.page.take_sent();
    assert_eq!(live_windows(&sent).len(), 1, "{sent:?}");
}

/// Ролик HLS — по-старому: перемотка страницы до показа ждёт показа и исполняется.
#[test]
fn a_page_seek_before_a_vod_hls_generation_is_shown_waits_for_the_show() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::OnDemand, ProtocolVersion::V1_2);

    harness.media.follow_page_seek(Duration::from_secs(2));
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now());

    assert_eq!(
        seeks(&harness.player.take_calls()),
        vec![Duration::from_secs(2)]
    );
}

/// После показа эфира перемотка страницы (тень уже перевела её во время плеера) уходит
/// плееру сразу.
#[test]
fn a_page_seek_after_the_live_generation_is_shown_goes_to_the_player() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    harness.player.take_calls();

    harness.media.follow_page_seek(Duration::from_secs(10));

    assert_eq!(
        seeks(&harness.player.take_calls()),
        vec![Duration::from_secs(10)]
    );
}

/// Поколение показано, но окна ещё нет (край неизвестен): страница ещё в своём времени,
/// её перемотка отбрасывается. Как только окно доставлено — следующая перемотка уходит.
#[test]
fn a_page_seek_after_the_show_but_before_the_first_window_is_dropped() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);
    harness.player.observation.borrow_mut().live_window = LiveWindowObservation::LiveEdgeUnknown;
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    harness.player.take_calls();

    harness.media.follow_page_seek(PAGE_TARGET);
    assert!(seeks(&harness.player.take_calls()).is_empty());

    harness.player.observation.borrow_mut().live_window = WINDOW;
    harness.media.drive(Instant::now());
    assert_eq!(live_windows(&harness.page.take_sent()).len(), 1);
    harness.media.follow_page_seek(Duration::from_secs(10));
    assert_eq!(
        seeks(&harness.player.take_calls()),
        vec![Duration::from_secs(10)]
    );
}

/// Окно поставлено, но страница отключена и его не получила: перемотка — ещё в её
/// времени и отбрасывается.
#[test]
fn a_window_stuck_at_a_detached_page_does_not_count_as_told() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);
    harness.page.refusals.borrow_mut().extend([
        SendToPageError::NoAttachedSession,
        SendToPageError::NoAttachedSession,
    ]);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    harness.player.take_calls();

    harness.media.follow_page_seek(Duration::from_secs(10));

    assert!(seeks(&harness.player.take_calls()).is_empty());
}

/// Странице 1.1 окно не уходит никогда — и её перемотки эфира не исполняются: времени
/// плеера она не знает.
#[test]
fn a_1_1_page_seek_on_a_live_generation_is_never_followed() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_1);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    harness.player.take_calls();

    harness.media.follow_page_seek(Duration::from_secs(10));

    assert!(seeks(&harness.player.take_calls()).is_empty());
}

/// Страница переподключилась в том же сеансе (`Attached` снова): край не двигался, а
/// окно уходит заново — прежнее соединение могло его потерять.
#[test]
fn a_reattached_page_gets_the_live_window_again_even_with_a_frozen_edge() {
    let (mut harness, ticket, _queue) =
        harness_with_loaded_hls(HlsPresentation::Live, ProtocolVersion::V1_2);
    harness
        .player
        .show_load(ticket, Duration::from_secs(17), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    assert_eq!(live_windows(&harness.page.take_sent()).len(), 1);
    harness.media.drive(Instant::now());
    assert!(
        live_windows(&harness.page.take_sent()).is_empty(),
        "unchanged"
    );

    harness
        .media
        .on_session_attached(&session(1), ProtocolVersion::V1_2);
    harness.media.drive(Instant::now());

    assert_eq!(live_windows(&harness.page.take_sent()).len(), 1);
}
