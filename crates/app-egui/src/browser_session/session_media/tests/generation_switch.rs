//! Смена поколения в плеере (ревью 03b, пакет I): отказ прежнего медиа, перемотка
//! страницы во время открытия нового поколения, скорость и «играть/пауза» страницы
//! после установки, пробуждение ради сводки журнала.

use super::*;
use crate::browser_session::journal::BROWSER_PLAYBACK_SUMMARY_INTERVAL;
use crate::browser_session::outbound::CLOCK_INTERVAL;

/// Отказы `stream_rejected` из того, что ушло странице.
fn rejections(messages: &[PlayerSessionMessage]) -> Vec<PlayerSessionMessage> {
    messages
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. }))
        .cloned()
        .collect()
}

/// Сеанс 1: поколение 1 загружено и показано; поколение 2 загружено, но плеер
/// его ещё ставит. Писатели держатся тестом.
fn second_generation_installing() -> (Harness, SessionRef, [PushedStreamWriter; 2]) {
    let (mut harness, current, first_writer) = loaded_harness();
    let (second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    harness.drive_until_loaded();
    harness.page.take_sent();
    (harness, current, [first_writer, second_writer])
}

/// Отказ прежнего медиа (ещё виден в наблюдении, пока новое ставится) не
/// отвергает новое поколение; отказ загрузки нового — отвергает, ровно один раз.
#[test]
fn a_fault_of_the_previous_media_never_rejects_the_next_generation() {
    let (mut harness, _current, _writers) = second_generation_installing();
    let first_load = harness.player.load_ticket(0);
    let second_load = harness.player.load_ticket(1);
    harness.player.observation.borrow_mut().fault = Some(LoadFault {
        load: first_load,
        fault: BrowserPlaybackFault::UnsupportedVideo,
        announcement: FaultAnnouncement::AnnouncedByPlayer,
    });
    let start = Instant::now();
    harness.media.drive(start);
    // Плеер поставил поколение 2, а отказ в наблюдении всё ещё про загрузку 1.
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(start + Duration::from_secs(1));
    assert_eq!(
        rejections(&harness.page.take_sent()),
        Vec::new(),
        "the second generation is not rejected for the first one's fault"
    );

    // Настоящий отказ поколения 2 после этого не проглатывается.
    harness.player.observation.borrow_mut().fault = Some(LoadFault {
        load: second_load,
        fault: BrowserPlaybackFault::UnsupportedVideo,
        announcement: FaultAnnouncement::AnnouncedByPlayer,
    });
    harness.media.drive(start + Duration::from_secs(2));
    harness.media.drive(start + Duration::from_secs(3));
    assert_eq!(
        rejections(&harness.page.take_sent()),
        vec![PlayerSessionMessage::StreamRejected {
            generation: generation(2),
            reason: StreamRejection::UnsupportedCodec,
        }]
    );
}

/// Перемотка страницы, пришедшая, пока новое поколение открывается, относится к
/// нему (источник нового поколения о ней уже знает): видео прежнего поколения в
/// плеере не двигается.
#[test]
fn a_page_seek_while_a_newer_generation_opens_does_not_move_the_old_video() {
    let (mut harness, current, _writer) = loaded_harness();
    // Поколение 2 без байтов: оно не откроется и остаётся ждущим.
    let (_second_writer, second) = stream_generation(&current, 2, false);
    let target = MediaTime::from_micros(5_000_000).expect("time");
    harness.media.on_intake(
        vec![second, intent(&current, PageIntent::FollowPageSeek(target))],
        None,
    );
    harness.media.drive(Instant::now());
    let calls = harness.player.take_calls();
    assert!(
        !calls.iter().any(|call| matches!(call, PlayerCall::Seek(_))),
        "the old video was moved: {calls:?}"
    );
}

/// Перемотка страницы ПОСЛЕ загрузки нового поколения, но ДО его показа: команда
/// сейчас досталась бы прежнему медиа (плеер ставит новое асинхронно). Старое видео
/// не двигается; новое перематывается ровно один раз — когда плеер его показал.
///
/// С 04d (путь C) этот путь безопасен: поколение отдаётся плееру только с данными у
/// своего старта, поэтому показ наступает, и отложенная перемотка не ждёт вечно (живая
/// находка: поколение, поставленное на клик без данных, не показывалось никогда, и
/// перемотка страницы ждала его до `VideoPreflightTimeout`). Отданное поколение уже не
/// держится воротами: перемотка во время установки — обычная перемотка, не новая цель.
#[test]
fn a_page_seek_while_the_next_generation_installs_reaches_it_once_it_is_shown() {
    let (mut harness, current, _writers) = second_generation_installing();
    // Отдано с данными у старта (с начала: данные поколения лежат с нуля).
    assert_eq!(
        harness.player.loaded_media.borrow()[1].initial_position,
        player_core::PreparedInitialPosition::Beginning
    );
    let target = Duration::from_millis(300);
    let page_target = MediaTime::from_micros(300_000).expect("time");
    harness.media.on_intake(
        vec![intent(&current, PageIntent::FollowPageSeek(page_target))],
        None,
    );
    let start = Instant::now();
    harness.media.drive(start);
    let calls = harness.player.take_calls();
    assert!(
        !calls.iter().any(|call| matches!(call, PlayerCall::Seek(_))),
        "the old video was moved: {calls:?}"
    );
    assert_eq!(
        harness.media.generation_wait(),
        super::super::GenerationWait::Idle,
        "a handed-over generation is never held again"
    );

    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(start + Duration::from_millis(10));
    harness.media.drive(start + Duration::from_millis(20));
    let seeks: Vec<PlayerCall> = harness
        .player
        .take_calls()
        .into_iter()
        .filter(|call| matches!(call, PlayerCall::Seek(_)))
        .collect();
    assert_eq!(seeks, vec![PlayerCall::Seek(target)]);
}

/// Плеер сбрасывает скорость при каждой загрузке: скорость страницы
/// восстанавливается, как только плеер поставил новое поколение, — один раз.
#[test]
fn the_page_rate_is_restored_once_the_next_generation_is_installed() {
    let (mut harness, current, _first_writer) = loaded_harness();
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::SetRate(PagePlaybackRate::from_permille(1_500).expect("rate")),
        )],
        None,
    );
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::SetRate(1.5)]);

    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    harness.drive_until_loaded();
    // Пока ставится — команды без толку: установка всё равно сбросит скорость.
    let start = Instant::now();
    harness.media.drive(start);
    assert!(harness.player.take_calls().is_empty());

    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(start + Duration::from_secs(1));
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::SetRate(1.5)]);
    harness.media.drive(start + Duration::from_secs(2));
    assert!(
        harness.player.take_calls().is_empty(),
        "the rate is restored once per generation"
    );
}

/// Пауза страницы во время установки досталась прежнему медиа; новое медиа
/// (загружено «играть») приводится к паузе, как только плеер его показал.
#[test]
fn a_page_pause_during_the_install_reaches_the_installed_media() {
    let (mut harness, current, _writers) = second_generation_installing();
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);

    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now());
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);
}

/// Совпадающее с намерением страницы состояние после установки команд не требует.
#[test]
fn an_installed_generation_that_matches_the_page_gets_no_extra_commands() {
    let (mut harness, _current, _writers) = second_generation_installing();
    let second_load = harness.player.load_ticket(1);
    harness
        .player
        .show_load(second_load, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now());
    assert!(harness.player.take_calls().is_empty());
}

/// Сводка журнала плеера пишется во время шага плеера: пока поколение загружено,
/// цикл просыпается к тику часов, а он много чаще сводки — своего срока ей не нужно.
#[test]
fn a_loaded_generation_wakes_the_loop_more_often_than_the_journal_summary() {
    let (mut harness, _current, _writer) = loaded_harness();
    let start = Instant::now() + Duration::from_secs(1);
    let wake_at = harness
        .media
        .drive(start)
        .expect("a loaded generation asks to wake");
    assert!(wake_at <= start + CLOCK_INTERVAL);
    assert!(CLOCK_INTERVAL < BROWSER_PLAYBACK_SUMMARY_INTERVAL);
}

/// Конец текущего сеанса забывает отложенное его странице: цикл не крутится ради
/// повторов к странице, которой больше нет.
#[test]
fn the_end_of_the_current_session_stops_retries_to_its_page() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    // Страница переподключается: каждая попытка отправки получает отказ.
    harness
        .page
        .refusals
        .borrow_mut()
        .extend([SendToPageError::NoAttachedSession; 2]);
    harness.media.on_intake(
        vec![IntakeNotice::Reject {
            session: current.clone(),
            generation: generation(1),
            reason: StreamRejection::ProtectedContent,
        }],
        None,
    );
    let start = Instant::now();
    assert_eq!(
        harness.media.drive(start),
        Some(start + PENDING_RETRY_INTERVAL),
        "the rejection waits for the reconnecting page"
    );

    // Конец чужого экземпляра ничего не трогает.
    harness.media.on_session_ended(&session(0), TAB_GONE_END);
    assert!(harness.media.drive(start).is_some());

    harness.media.on_session_ended(&current, TAB_GONE_END);
    assert_eq!(harness.media.drive(start), None);
    assert!(harness.page.take_sent().is_empty());
}
