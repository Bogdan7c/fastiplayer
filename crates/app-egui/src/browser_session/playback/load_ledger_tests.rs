//! Учёт загрузок: отмена текущей и заменённой установки, ошибка видео раньше итога
//! установки (общие для окна и безоконного плеера, ревью 04a пакет P3).

use std::num::NonZeroU64;

use player_core::{MediaInstanceId, PlaybackState, PlayerErrorKind, PlayerSnapshot};

use super::*;
use crate::browser_session::playback::MediaLoadTicketIssuer;

fn instance(raw: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(raw).expect("non-zero"))
}

/// Снимок плеера, показывающий экземпляр `raw` в состоянии `state`.
fn showing(raw: u64, state: PlaybackState) -> PlayerSnapshot {
    PlayerSnapshot {
        media_instance_id: Some(instance(raw)),
        playback_state: state,
        ..PlayerSnapshot::default()
    }
}

fn fault_of(ledger: &LoadLedger, ticket: MediaLoadTicket) -> Option<BrowserPlaybackFault> {
    ledger
        .observation(&PlayerSnapshot::default())
        .fault_of(ticket)
        .cloned()
}

#[test]
fn cancelling_the_current_load_is_its_fault() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let load = tickets.issue();
    ledger.begin_load(load);

    assert_eq!(
        ledger.note_install_cancelled(load),
        CancelledInstall::CurrentLoadAborted
    );
    assert_eq!(
        fault_of(&ledger, load),
        Some(BrowserPlaybackFault::InstallAborted)
    );
}

#[test]
fn cancelling_a_replaced_load_is_not_a_fault_of_anyone() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let older = tickets.issue();
    let newer = tickets.issue();
    ledger.begin_load(older);
    ledger.begin_load(newer);

    assert_eq!(
        ledger.note_install_cancelled(older),
        CancelledInstall::ReplacedByNewerLoad
    );
    assert_eq!(
        fault_of(&ledger, newer),
        None,
        "the newer load is untouched"
    );
    assert_eq!(fault_of(&ledger, older), None);
}

#[test]
fn a_video_error_that_overtakes_the_install_result_belongs_to_the_current_load() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let load = tickets.issue();
    ledger.begin_load(load);

    // Плеер уже поставил экземпляр 7 и сообщил об ошибке его видео, а итог установки
    // учёт ещё не получил: пока отказа нет (экземпляр не опознан)…
    ledger.note_player_error(Some(instance(7)), &PlayerErrorKind::UnsupportedVideoCodec);
    assert_eq!(fault_of(&ledger, load), None);

    // …а итог установки опознаёт экземпляр — ошибка становится отказом загрузки.
    ledger.note_installed(load, instance(7));
    assert_eq!(
        fault_of(&ledger, load),
        Some(BrowserPlaybackFault::UnsupportedVideo)
    );
}

#[test]
fn an_early_video_error_of_another_instance_is_not_attributed() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let load = tickets.issue();
    ledger.begin_load(load);

    ledger.note_player_error(Some(instance(3)), &PlayerErrorKind::UnsupportedVideoCodec);
    ledger.note_installed(load, instance(4));
    assert_eq!(fault_of(&ledger, load), None);
    // Опознанный экземпляр снимка — загрузки, и отказа у неё нет.
    let observation = ledger.observation(&showing(4, PlaybackState::Playing));
    assert!(observation.shows(load));
    assert_eq!(observation.fault, None);
}

#[test]
fn an_early_video_error_waits_past_a_late_install_of_a_replaced_load() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let older = tickets.issue();
    let newer = tickets.issue();
    ledger.begin_load(older);
    ledger.begin_load(newer);

    // Ошибка экземпляра новой загрузки, затем поздний итог заменённой (экземпляр 5),
    // и только потом — итог новой (экземпляр 6).
    ledger.note_player_error(
        Some(instance(6)),
        &PlayerErrorKind::UnsupportedVideoBitDepth,
    );
    ledger.note_installed(older, instance(5));
    assert_eq!(fault_of(&ledger, newer), None);
    ledger.note_installed(newer, instance(6));
    assert_eq!(
        fault_of(&ledger, newer),
        Some(BrowserPlaybackFault::UnsupportedVideo)
    );
}

#[test]
fn an_error_remembered_for_an_older_load_does_not_follow_into_a_new_one() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let older = tickets.issue();
    ledger.begin_load(older);
    ledger.note_player_error(Some(instance(9)), &PlayerErrorKind::UnsupportedVideoCodec);

    // Новая загрузка начата; плеер (теоретически) переиспользовал бы экземпляр 9 —
    // ошибка была до её начала и ей не принадлежит.
    let newer = tickets.issue();
    ledger.begin_load(newer);
    ledger.note_installed(newer, instance(9));
    assert_eq!(fault_of(&ledger, newer), None);
}

#[test]
fn errors_other_than_unsupported_video_are_not_remembered() {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let load = tickets.issue();
    ledger.begin_load(load);
    ledger.note_player_error(Some(instance(2)), &PlayerErrorKind::RuntimeError);
    ledger.note_player_error(None, &PlayerErrorKind::UnsupportedVideoCodec);
    ledger.note_installed(load, instance(2));
    assert_eq!(fault_of(&ledger, load), None);
}

/// Учёт, которому экземпляр 100 сообщил об ошибке первым, а за ним — столько других,
/// сколько вмещает память неопознанных.
fn ledger_after_overflowing_the_unclaimed_memory() -> (LoadLedger, MediaLoadTicket) {
    let mut tickets = MediaLoadTicketIssuer::default();
    let mut ledger = LoadLedger::default();
    let load = tickets.issue();
    ledger.begin_load(load);
    ledger.note_player_error(Some(instance(100)), &PlayerErrorKind::UnsupportedVideoCodec);
    for raw in 1..=u64::try_from(UNCLAIMED_VIDEO_ERRORS_CAPACITY).expect("small") {
        ledger.note_player_error(Some(instance(raw)), &PlayerErrorKind::UnsupportedVideoCodec);
    }
    (ledger, load)
}

#[test]
fn the_unclaimed_memory_is_bounded_and_forgets_the_oldest() {
    let (mut ledger, load) = ledger_after_overflowing_the_unclaimed_memory();
    ledger.note_installed(load, instance(100));
    assert_eq!(
        fault_of(&ledger, load),
        None,
        "the oldest unclaimed instance was evicted"
    );

    let (mut ledger, load) = ledger_after_overflowing_the_unclaimed_memory();
    let newest = u64::try_from(UNCLAIMED_VIDEO_ERRORS_CAPACITY).expect("small");
    ledger.note_installed(load, instance(newest));
    assert_eq!(
        fault_of(&ledger, load),
        Some(BrowserPlaybackFault::UnsupportedVideo),
        "the newest unclaimed instance is still remembered"
    );
}
