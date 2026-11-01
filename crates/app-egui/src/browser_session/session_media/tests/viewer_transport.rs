//! Транспорт зрителя (04a): действие человека в окне → плеер И страница; эхо страницы
//! не дёргает плеер второй раз и не откатывает более позднее действие зрителя.
//!
//! Плеер и страница подделаны (видно каждую команду и каждое сообщение), поток
//! поколения — настоящий `pushed-media-source` на фикстурах протокола.

use super::*;
use crate::browser_session::session_media::viewer_transport::VIEWER_WILL_CONFIRMATION_WINDOW;
use crate::viewer_transport::ViewerTransportIntent;

/// Сообщения странице без потоковых показаний (часов и буфера).
fn page_commands(messages: Vec<PlayerSessionMessage>) -> Vec<PlayerSessionMessage> {
    messages
        .into_iter()
        .filter(|message| {
            !matches!(
                message,
                PlayerSessionMessage::Clock { .. } | PlayerSessionMessage::BufferState { .. }
            )
        })
        .collect()
}

fn page_rate(permille: u32) -> PagePlaybackRate {
    PagePlaybackRate::from_permille(permille).expect("rate")
}

/// Перемотки, о которых попросили страницу.
fn page_seeks(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Seek { target_us, .. } => Some(target_us.as_micros()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_viewer_pause_pauses_the_player_at_once_and_tells_the_page() {
    let (mut harness, _current, _writer) = loaded_harness();
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, now);
    // У нас — сразу, без ожидания страницы.
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);
    harness.media.drive(now);
    assert_eq!(
        page_commands(harness.page.take_sent()),
        vec![PlayerSessionMessage::Pause {}]
    );
    // Воля зрителя — теперь воля страницы: доигрывание и загрузки читают её.
    assert!(!harness.media.page_wants_play);
}

#[test]
fn the_page_echo_of_a_viewer_command_does_not_command_the_player_again() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, now);
    harness.player.take_calls();
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    assert!(
        harness.player.take_calls().is_empty(),
        "the echo is a confirmation, not a second pause"
    );
}

/// Гонка: зритель нажал паузу, а от страницы ещё летит прежний `play`. Побеждает
/// зритель; после подтверждения страница снова ведёт сама.
#[test]
fn a_late_page_play_never_undoes_the_viewer_pause() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, now);
    harness.player.take_calls();

    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Play)], None);
    harness.media.drive(now + Duration::from_millis(100));
    assert!(harness.player.take_calls().is_empty(), "stale play ignored");
    assert!(!harness.media.page_wants_play, "the viewer's pause stands");

    // Подтверждение страницы снимает волю зрителя …
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);
    assert!(harness.player.take_calls().is_empty());
    // … и следующий `play` страницы — уже её новая воля (автопереход, реклама).
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Play)], None);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
}

#[test]
fn an_unconfirmed_viewer_will_lets_the_page_lead_after_the_window() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, now);
    harness.player.take_calls();
    harness.media.drive(now + VIEWER_WILL_CONFIRMATION_WINDOW);
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Play)], None);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Play]);
}

#[test]
fn a_viewer_rate_reaches_the_player_and_the_page_and_its_echo_is_silent() {
    let (mut harness, current, _writer) = loaded_harness();
    let now = Instant::now();
    let rate = PlayerPlaybackRate::new(1.5).expect("valid rate");
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::SetRate(rate), now);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::SetRate(1.5)]);
    harness.media.drive(now);
    assert_eq!(
        page_commands(harness.page.take_sent()),
        vec![PlayerSessionMessage::SetRate {
            rate: page_rate(1_500)
        }]
    );
    assert_eq!(harness.media.page_rate, rate);

    // Устаревшая скорость страницы и затем эхо — плеер не трогают.
    harness.media.on_intake(
        vec![
            intent(&current, PageIntent::SetRate(page_rate(1_000))),
            intent(&current, PageIntent::SetRate(page_rate(1_500))),
        ],
        None,
    );
    assert!(harness.player.take_calls().is_empty());
    assert_eq!(harness.media.page_rate, rate);
}

/// Перемотка показанного поколения: плеер перематывается точно и возвращается к
/// «играть» (перемотка после конца кончается паузой). Своей перемотки странице медиа-
/// часть не шлёт: нужна ли она, решает источник (вне полки — его единственная просьба).
#[test]
fn a_viewer_seek_moves_the_player_without_a_second_seek_to_the_page() {
    let (mut harness, _current, _writer) = loaded_harness();
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(Duration::from_millis(200)), now);
    assert_eq!(
        harness.player.take_calls(),
        vec![
            PlayerCall::Seek(Duration::from_millis(200)),
            PlayerCall::Play
        ]
    );
    harness.media.drive(now);
    assert!(page_seeks(&harness.page.take_sent()).is_empty());
}

/// Перемотка, пока плеер ставит поколение: команда досталась бы прежнему медиа, поэтому
/// она ждёт показа (правило 03e) и исполняется ровно один раз.
#[test]
fn a_viewer_seek_during_install_runs_when_the_generation_is_shown() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(Duration::from_millis(300)), now);
    assert!(
        harness.player.take_calls().is_empty(),
        "not to the old media"
    );

    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(now);
    let calls = harness.player.take_calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| **call == PlayerCall::Seek(Duration::from_millis(300)))
            .count(),
        1,
        "{calls:?}"
    );
    harness.media.drive(now);
    assert!(
        !harness
            .player
            .take_calls()
            .iter()
            .any(|call| matches!(call, PlayerCall::Seek(_)))
    );
}

/// Перемотка, пока открывается НОВОЕ поколение: прежнее видео не двигается, цель
/// становится стартом нового (03e), а страницу просят перемотаться ровно один раз.
#[test]
fn a_viewer_seek_while_a_new_generation_opens_starts_it_there_and_asks_the_page_once() {
    let (mut harness, current, _writer) = loaded_harness();
    // Поколение 2 без байтов: открывается, но открыться не может.
    let (_second_writer, second) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![second], None);
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(Duration::from_secs(5)), now);
    assert!(
        harness.player.take_calls().is_empty(),
        "the old video stays"
    );
    assert_eq!(
        harness
            .media
            .pending
            .as_ref()
            .and_then(|pending| pending.placement.awaited_target()),
        Some(Duration::from_secs(5))
    );
    harness.media.drive(now);
    harness.media.drive(now + PENDING_RETRY_INTERVAL);
    assert_eq!(page_seeks(&harness.page.take_sent()), vec![5_000_000]);
}

/// Страница переподключается: команда зрителя не теряется и уходит, когда она вернётся.
#[test]
fn a_viewer_command_waits_for_a_reconnecting_page() {
    let (mut harness, _current, _writer) = loaded_harness();
    let now = Instant::now();
    harness
        .page
        .refusals
        .borrow_mut()
        .extend([SendToPageError::NoAttachedSession; 4]);
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, now);
    harness.media.drive(now);
    assert!(page_commands(harness.page.take_sent()).is_empty());
    harness.page.refusals.borrow_mut().clear();
    harness.media.drive(now + PENDING_RETRY_INTERVAL);
    assert_eq!(
        page_commands(harness.page.take_sent()),
        vec![PlayerSessionMessage::Pause {}]
    );
}

/// Страница ушла (доигрываем): зритель по-прежнему ведёт плеер, а странице слать нечего.
#[test]
fn after_the_page_left_the_viewer_still_controls_the_player_only() {
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.on_session_ended(&current, TAB_GONE_END);
    let now = Instant::now();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, now);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);
    harness.media.drive(now + PENDING_RETRY_INTERVAL);
    assert!(page_commands(harness.page.take_sent()).is_empty());
}

/// Воля зрителя живёт дальше текущего видео: следующее поколение загружается на паузе.
#[test]
fn after_a_viewer_pause_the_next_generation_loads_paused() {
    let (mut harness, current, _writer) = loaded_harness();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, Instant::now());
    harness.player.take_calls();
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![second], None);
    assert_eq!(
        harness.drive_until_loaded(),
        vec![PlayerCall::Load(
            BROWSER_STREAM_LABEL.to_owned(),
            LoadStart::Paused
        )]
    );
}

/// Новая страница (другой экземпляр сеанса) — воля, сказанная прежней, не мешает ей.
#[test]
fn a_new_session_does_not_inherit_the_unconfirmed_viewer_will() {
    let (mut harness, _current, _writer) = loaded_harness();
    harness
        .media
        .on_viewer_transport(ViewerTransportIntent::Pause, Instant::now());
    let next = session(2);
    harness.media.on_session_opened(&next);
    let (_second_writer, notice) = stream_generation(&next, 1, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let ticket = harness.player.load_ticket(1);
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now());
    harness.player.take_calls();
    harness
        .media
        .on_intake(vec![intent(&next, PageIntent::Pause)], None);
    assert_eq!(harness.player.take_calls(), vec![PlayerCall::Pause]);
}
