//! Доигрывание после конца сеанса (03c) на медиа-части с подделкой плеера:
//! каждый вариант [`DrainEnd`], тишина по отношению к странице и забывание
//! доигрывания при новом сеансе. Чистые правила (сроки, застой) — в `drain.rs`.

use super::*;
use crate::browser_session::playback::LoadFault;

/// Короткие сроки, чтобы тест застоя не ждал секундами.
const SHORT_POLICY: DrainPolicy = DrainPolicy {
    stall_timeout: Duration::from_millis(400),
    poll_interval: Duration::from_millis(250),
    paused: PausedDrain::ExitAtOnce,
};

fn drained(progress: DrainProgress) -> DrainEnd {
    match progress {
        DrainProgress::Drained(end) => end,
        other @ (DrainProgress::Draining { .. } | DrainProgress::AwaitingViewer) => {
            panic!("still draining: {other:?}")
        }
    }
}

/// Конец сеанса, как его видит главный цикл: событие конца и затем уведомление потока
/// приёма «вход разобран и запечатан» (идёт в порядке данных, после них).
fn end_session(harness: &mut Harness, session: &SessionRef) {
    harness.media.on_session_ended(session, TAB_GONE_END);
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: session.clone(),
        }],
        None,
    );
}

fn show_playing(harness: &Harness, position: Duration) {
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, position, PlaybackState::Playing);
}

/// Вне цикла доигрывания (сеанс не кончился) «доиграно?» отвечает «нечего»:
/// главный цикл в этот момент и не спрашивает, но ответ определён.
#[test]
fn without_an_ended_session_there_is_nothing_to_drain() {
    let (mut harness, _current, _writer) = loaded_harness();

    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::NothingToPlay
    );
}

#[test]
fn nothing_loaded_and_nothing_opening_ends_the_drain_at_once() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    end_session(&mut harness, &current);

    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::NothingToPlay
    );
}

#[test]
fn a_playing_video_is_drained_until_the_player_reaches_the_end() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing(&harness, Duration::from_secs(1));
    harness.media.drive(Instant::now());
    end_session(&mut harness, &current);

    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));

    harness.player.observation.borrow_mut().ended = true;
    harness.player.observation.borrow_mut().state = PlaybackState::Ended;
    harness.media.drive(Instant::now());
    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::PlayedToEnd
    );
}

/// «Дошёл до конца» относится к НАШЕМУ поколению: `ended` прежнего медиа (плеер ещё
/// показывает его, пока ставит новое) доигрывание не заканчивает.
#[test]
fn the_end_of_the_previous_media_does_not_end_the_drain() {
    let (mut harness, current, _writer) = loaded_harness();
    // Плеер ещё не показывает наше медиа (`shown_load` пуст), но `ended` стоит.
    {
        let mut observation = harness.player.observation.borrow_mut();
        observation.shown_load = None;
        observation.ended = true;
    }
    harness.media.drive(Instant::now());
    end_session(&mut harness, &current);

    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));
}

#[test]
fn a_player_fault_of_the_loaded_generation_ends_the_drain_at_once() {
    let (mut harness, current, _writer) = loaded_harness();
    let ticket = harness.player.load_ticket(0);
    harness.player.observation.borrow_mut().fault = Some(LoadFault {
        load: ticket,
        fault: BrowserPlaybackFault::PlaybackFailed(player_core::PlayerErrorKind::RuntimeError),
    });
    harness.media.drive(Instant::now());
    end_session(&mut harness, &current);

    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::PlayerFault
    );
}

#[test]
fn a_page_that_paused_ends_the_drain_at_once_even_if_the_pause_came_late() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing(&harness, Duration::from_secs(1));
    harness.media.drive(Instant::now());
    end_session(&mut harness, &current);
    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));

    // `page_playback{pause}` шло по проводу раньше конца, но дошло до цикла позже.
    harness
        .media
        .on_intake(vec![intent(&current, PageIntent::Pause)], None);

    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::PagePaused
    );
}

#[test]
fn an_opening_generation_is_awaited_and_a_frozen_player_is_given_up_on() {
    let mut harness = Harness::new();
    harness.media.drain_policy = SHORT_POLICY;
    let current = session(1);
    harness.media.on_session_opened(&current);
    // Поток без медиа: ждёт открытия, пока источник не запечатан.
    let (_writer, pending) = stream_generation(&current, 1, false);
    harness.media.on_intake(vec![pending], None);
    end_session(&mut harness, &current);
    let start = Instant::now();

    assert!(matches!(
        harness.media.playback_drain(start),
        DrainProgress::Draining { .. }
    ));
    assert_eq!(
        drained(
            harness
                .media
                .playback_drain(start + SHORT_POLICY.stall_timeout)
        ),
        DrainEnd::Stalled
    );
}

#[test]
fn a_loaded_video_whose_position_never_moves_is_stalled() {
    let (mut harness, current, _writer) = loaded_harness();
    harness.media.drain_policy = SHORT_POLICY;
    show_playing(&harness, Duration::from_secs(2));
    harness.media.drive(Instant::now());
    end_session(&mut harness, &current);
    let start = Instant::now();

    assert!(matches!(
        harness.media.playback_drain(start),
        DrainProgress::Draining { .. }
    ));
    assert!(matches!(
        harness
            .media
            .playback_drain(start + Duration::from_millis(300)),
        DrainProgress::Draining { .. }
    ));
    assert_eq!(
        drained(
            harness
                .media
                .playback_drain(start + Duration::from_millis(400))
        ),
        DrainEnd::Stalled
    );
}

/// Пока доигрываем, странице нет ни часов, ни `buffer_state`, ни повторов отказа;
/// цикл не просыпается впустую.
#[test]
fn nothing_is_sent_to_the_page_while_draining() {
    let (mut harness, current, _writer) = loaded_harness();
    show_playing(&harness, Duration::from_secs(1));
    harness.media.drive(Instant::now());
    harness.page.take_sent();
    end_session(&mut harness, &current);

    for step in 0..6 {
        let wake_at = harness
            .media
            .drive(Instant::now() + Duration::from_secs(step));
        assert_eq!(
            wake_at, None,
            "draining wakes the loop through playback_drain"
        );
    }

    assert!(harness.page.take_sent().is_empty());
}

/// Отказ плеера во время доигрывания никому не сообщается (страницы нет) и не
/// остаётся в очереди исходящих: иначе повтор отправки крутил бы цикл вечно.
#[test]
fn a_rejection_while_draining_is_not_queued_for_a_page_that_is_gone() {
    let (mut harness, current, _writer) = loaded_harness();
    end_session(&mut harness, &current);
    let ticket = harness.player.load_ticket(0);
    harness.player.observation.borrow_mut().fault = Some(LoadFault {
        load: ticket,
        fault: BrowserPlaybackFault::UnsupportedVideo,
    });

    let wake_at = harness.media.drive(Instant::now());

    assert_eq!(wake_at, None);
    assert!(harness.page.take_sent().is_empty());
    assert!(!harness.media.outbound.has_pending());
}

/// Новый сеанс во время доигрывания заменяет доигрываемый: страница нового сеанса
/// снова получает часы, а доигрывание забыто.
#[test]
fn a_new_session_ends_the_drain_and_the_page_gets_its_messages_again() {
    let (mut harness, old_session, _writer) = loaded_harness();
    harness.media.on_session_ended(&old_session, TAB_GONE_END);
    assert!(harness.media.drain.is_some());

    let new_session = session(2);
    harness.media.on_session_opened(&new_session);
    assert!(harness.media.drain.is_none(), "the drain is forgotten");

    let (_new_writer, notice) = stream_generation(&new_session, 1, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let ticket = harness.player.load_ticket(1);
    harness
        .player
        .show_load(ticket, Duration::from_secs(1), PlaybackState::Playing);
    harness.page.take_sent();
    // Следующий тик часов уже наступил (прошло больше шага).
    harness.media.drive(Instant::now() + Duration::from_secs(1));

    let sent = harness.page.take_sent();
    assert!(!clocks(&sent).is_empty(), "clock ticks resume: {sent:?}");
}

/// Конец ЧУЖОГО (заменённого) экземпляра доигрывание не запускает.
#[test]
fn the_end_of_a_replaced_instance_does_not_start_a_drain() {
    let (mut harness, old_session, _writer) = loaded_harness();
    harness.media.on_session_opened(&session(2));

    harness.media.on_session_ended(&old_session, TAB_GONE_END);

    assert!(harness.media.drain.is_none());
}

// ---- конец сеанса обгоняет данные ------------------------------------------------

/// Событие конца сеанса идёт в ящик напрямую и обгоняет поколение, лежащее в очереди
/// потока приёма: до запечатывания «нечего играть» не решается, а когда поколение
/// пришло, видео доигрывается.
#[test]
fn an_end_that_overtakes_the_generation_does_not_cancel_the_video() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);

    // Конец обогнал данные: поток приёма ещё не отдал ни поколения, ни запечатывания.
    harness.media.on_session_ended(&current, TAB_GONE_END);
    assert!(
        matches!(
            harness.media.playback_drain(Instant::now()),
            DrainProgress::Draining { .. }
        ),
        "the burst is still in the intake queue"
    );

    // Всплеск дошёл: поколение, затем запечатывание (в порядке очереди).
    let (_writer, generation_notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(
        vec![
            generation_notice,
            IntakeNotice::InstanceSealed {
                session: current.clone(),
            },
        ],
        None,
    );
    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));
    harness.drive_until_loaded();
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::from_secs(1), PlaybackState::Playing);
    harness.media.drive(Instant::now());
    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));
    {
        let mut observation = harness.player.observation.borrow_mut();
        observation.ended = true;
        observation.state = PlaybackState::Ended;
    }
    harness.media.drive(Instant::now());
    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::PlayedToEnd
    );
}

/// Пауза страницы из того же всплеска не теряется: решение «пауза» принимается только
/// после запечатывания, когда намерение уже разобрано.
#[test]
fn a_pause_in_the_burst_is_seen_before_the_drain_decides() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    harness.media.on_session_ended(&current, TAB_GONE_END);
    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));

    let (_writer, generation_notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(
        vec![
            generation_notice,
            intent(&current, PageIntent::Pause),
            IntakeNotice::InstanceSealed {
                session: current.clone(),
            },
        ],
        None,
    );

    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::PagePaused
    );
}

/// Запечатывание чужого (другого) экземпляра доигрывание этого не разрешает.
#[test]
fn the_sealing_of_another_instance_does_not_open_the_verdict() {
    let mut harness = Harness::new();
    let current = session(2);
    harness.media.on_session_opened(&current);
    harness.media.on_session_ended(&current, TAB_GONE_END);
    harness.media.on_intake(
        vec![IntakeNotice::InstanceSealed {
            session: session(1),
        }],
        None,
    );

    assert!(matches!(
        harness.media.playback_drain(Instant::now()),
        DrainProgress::Draining { .. }
    ));
}

/// Поток приёма завис и запечатывания не будет: ожидание ограничено сроком застоя.
#[test]
fn a_missing_seal_is_bounded_by_the_stall_timeout() {
    let mut harness = Harness::new();
    harness.media.drain_policy = SHORT_POLICY;
    let current = session(1);
    harness.media.on_session_opened(&current);
    harness.media.on_session_ended(&current, TAB_GONE_END);
    let start = Instant::now();

    assert!(matches!(
        harness.media.playback_drain(start),
        DrainProgress::Draining { .. }
    ));
    assert_eq!(
        drained(
            harness
                .media
                .playback_drain(start + SHORT_POLICY.stall_timeout)
        ),
        DrainEnd::Stalled
    );
}

/// Ждущее поколение новее загруженного: пока оно открывается, конец, отказ и позиция
/// прежнего видео доигрывание не решают.
#[test]
fn an_opening_newer_generation_blocks_the_verdicts_of_the_previous_one() {
    let (mut harness, current, _writer) = loaded_harness();
    {
        let mut observation = harness.player.observation.borrow_mut();
        observation.ended = true;
        observation.state = PlaybackState::Ended;
    }
    harness.media.drive(Instant::now());
    // Новое поколение без медиа: открывается.
    let (_new_writer, newer) = stream_generation(&current, 2, false);
    harness.media.on_intake(vec![newer], None);
    end_session(&mut harness, &current);

    assert!(
        matches!(
            harness.media.playback_drain(Instant::now()),
            DrainProgress::Draining { .. }
        ),
        "the previous video ended, but a newer generation is still opening"
    );

    // Прежнее без нового: то же состояние доигрывается до конца сразу.
    harness.media.pending = None;
    assert_eq!(
        drained(harness.media.playback_drain(Instant::now())),
        DrainEnd::PlayedToEnd
    );
}
