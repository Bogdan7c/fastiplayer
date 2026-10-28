//! Такт декодера по проходам показа рендера (gles2-renderer 12a.2).
//!
//! Тесты политики без VA-API: подписка — настоящий нейтральный `PresentPassSignal`,
//! control-канал — тот же bounded crossbeam-канал, что у decoder thread-а.

use std::thread;

use video_core::PresentPassSignal;

use super::super::present_pacing::{
    DecodedFramesAhead, PRESENT_PACING_MIN_DECODED_FRAMES_AHEAD, PacingPermitSource,
    PacketAdmission, PresentPacing, PresentPacingCounters, PresentPulseWake,
};
use super::*;

/// Запас, при котором такт обязан работать.
const COMFORTABLE_LEAD: DecodedFramesAhead =
    DecodedFramesAhead(PRESENT_PACING_MIN_DECODED_FRAMES_AHEAD + 6);

/// Запас ниже порога: декодер догоняет без такта.
const LOW_LEAD: DecodedFramesAhead =
    DecodedFramesAhead(PRESENT_PACING_MIN_DECODED_FRAMES_AHEAD - 1);

/// Предел ожидания в тестах: короче production, но с запасом на загруженный CI.
const TEST_WAIT_LIMIT: Duration = Duration::from_millis(40);

/// Задержка, с которой фоновый поток имитирует проход показа рендера.
const BACKGROUND_PRESENT_DELAY: Duration = Duration::from_millis(20);

/// Такт с подпиской на свежий сигнал; источник возвращается, чтобы тест звонил в него.
fn paced() -> (PresentPassSignal, PresentPacing) {
    let signal = PresentPassSignal::new();
    let subscription = signal.subscribe().expect("fresh signal is not poisoned");
    (signal, PresentPacing::paced(subscription))
}

/// Такт после первой подачи при хорошем запасе: следующая подача ждёт показа.
fn paced_after_first_submission() -> (PresentPassSignal, PresentPacing) {
    let (signal, mut pacing) = paced();
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::Admit
    );
    pacing.note_packet_submitted();
    (signal, pacing)
}

/// Без подписки (рендер wgpu) декодер не ждёт никогда — поведение до 12a.2.
#[test]
fn unpaced_decoder_never_waits_for_present() {
    let mut pacing = PresentPacing::unpaced();

    for _ in 0..5 {
        assert_eq!(
            pacing.admission_for_next_packet(COMFORTABLE_LEAD),
            PacketAdmission::Admit
        );
        pacing.note_packet_submitted();
    }
    assert!(!pacing.is_paced());
    assert_eq!(pacing.counters(), PresentPacingCounters::default());
}

/// Главное правило: при запасе одна подача на один показ.
#[test]
fn comfortable_lead_allows_one_submission_per_present_pass() {
    let (signal, mut pacing) = paced_after_first_submission();

    // Показа ещё не было: следующий пакет ждёт.
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::AwaitPresentPulse
    );

    // Показ был (даже трижды) — разрешена ровно одна подача.
    for _ in 0..3 {
        signal.notify_present_pass_submitted();
    }
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::Admit
    );
    pacing.note_packet_submitted();
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::AwaitPresentPulse
    );
    assert_eq!(pacing.counters().ready_pulse_admits, 1);
}

/// При малом запасе декодер догоняет без ожидания, а старый сигнал не переносится в такт.
#[test]
fn low_lead_admits_without_waiting_and_discards_stale_pulse() {
    let (signal, mut pacing) = paced_after_first_submission();
    signal.notify_present_pass_submitted();

    assert_eq!(
        pacing.admission_for_next_packet(LOW_LEAD),
        PacketAdmission::Admit
    );
    pacing.note_packet_submitted();

    // Запас набран, но показ, случившийся до этого, разрешением не считается.
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::AwaitPresentPulse
    );
    assert_eq!(pacing.counters().low_lead_admits, 1);
    assert_eq!(pacing.counters().ready_pulse_admits, 0);
}

/// Ожидание заканчивается проходом показа, а не пределом времени.
#[test]
fn wait_ends_on_present_pass_from_renderer_thread() {
    let (signal, mut pacing) = paced_after_first_submission();
    let (_control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let renderer = thread::spawn(move || {
        thread::sleep(BACKGROUND_PRESENT_DELAY);
        signal.notify_present_pass_submitted();
        signal
    });

    let wake = pacing.wait_for_present_pulse_or_control(&control_rx, Duration::from_secs(5));

    assert!(matches!(
        wake,
        PresentPulseWake::PermitGranted(PacingPermitSource::PresentPulse)
    ));
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::Admit
    );
    assert_eq!(pacing.counters().present_pulse_waits, 1);
    drop(renderer.join().expect("renderer thread"));
}

/// Показа нет (окно скрыто) — декодер не застревает: подаёт пакет после предела ожидания.
#[test]
fn wait_limit_grants_submission_without_present() {
    let (_signal, mut pacing) = paced_after_first_submission();
    let (_control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let started = Instant::now();

    let wake = pacing.wait_for_present_pulse_or_control(&control_rx, TEST_WAIT_LIMIT);

    assert!(matches!(
        wake,
        PresentPulseWake::PermitGranted(PacingPermitSource::WaitLimitReached)
    ));
    assert!(started.elapsed() >= TEST_WAIT_LIMIT);
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::Admit
    );
    assert_eq!(pacing.counters().wait_limit_reached, 1);
}

/// Seek/flush не ждут показа: control message прерывает ожидание и разрешения не даёт.
#[test]
fn control_message_interrupts_wait_without_granting_submission() {
    let (_signal, mut pacing) = paced_after_first_submission();
    let (control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (flush_ack_tx, _flush_ack_rx) = bounded(1);
    control_tx
        .try_send(ThreadControlMsg::Flush(flush_ack_tx))
        .expect("control channel has room");

    let wake = pacing.wait_for_present_pulse_or_control(&control_rx, Duration::from_secs(5));

    assert!(matches!(
        wake,
        PresentPulseWake::Control(ThreadControlMsg::Flush(_))
    ));
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::AwaitPresentPulse
    );
    assert_eq!(
        pacing.counters().present_pulse_waits + pacing.counters().wait_limit_reached,
        0
    );
}

/// Закрытый control-канал завершает ожидание как завершение decoder loop-а.
#[test]
fn closed_control_channel_ends_wait() {
    let (_signal, mut pacing) = paced_after_first_submission();
    let (control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    drop(control_tx);

    let wake = pacing.wait_for_present_pulse_or_control(&control_rx, Duration::from_secs(5));

    assert!(matches!(wake, PresentPulseWake::ControlDisconnected));
}

/// Рендер уничтожен (пересоздание, выход) — такт выключается, декодер работает как раньше.
#[test]
fn dropped_signal_turns_pacing_off() {
    let (signal, mut pacing) = paced_after_first_submission();
    drop(signal);

    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::Admit
    );
    assert!(!pacing.is_paced());
    pacing.note_packet_submitted();
    assert_eq!(
        pacing.admission_for_next_packet(COMFORTABLE_LEAD),
        PacketAdmission::Admit
    );
}

/// Источник уничтожен прямо во время ожидания — ожидание заканчивается сразу, такт выключен.
#[test]
fn signal_dropped_during_wait_ends_wait_and_turns_pacing_off() {
    let (signal, mut pacing) = paced_after_first_submission();
    let (_control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let renderer = thread::spawn(move || {
        thread::sleep(BACKGROUND_PRESENT_DELAY);
        drop(signal);
    });

    let wake = pacing.wait_for_present_pulse_or_control(&control_rx, Duration::from_secs(5));

    assert!(matches!(
        wake,
        PresentPulseWake::PermitGranted(PacingPermitSource::SignalGone)
    ));
    assert!(!pacing.is_paced());
    renderer.join().expect("renderer thread");
}
