//! Отказ поколения (04b, решения 128–129): странице `stream_rejected{reason}` ровно один
//! раз, зрителю окна — сообщение, если он об отказе ещё не знает.
//!
//! Отказы плеера здесь даёт НАСТОЯЩИЙ учёт загрузок (`LoadLedger`, общий у окна и
//! безоконного плеера): по нему видно главное правило — отмена установки, которую
//! заменило более новое поколение, не отказ, и странице ложный `stream_rejected` не идёт.
//! Плеер — подделка медиа-части; потоки поколений — настоящие.

use player_core::{PlayerErrorKind, PlayerSnapshot};

use super::*;
use crate::browser_session::playback::load_ledger::{CancelledInstall, LoadLedger};
use crate::browser_session::rejection_notice::GenerationRejectionNotice;

/// Отказы, которые получила страница: (поколение, причина).
fn rejections(messages: &[PlayerSessionMessage]) -> Vec<(u32, StreamRejection)> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::StreamRejected { generation, reason } => {
                Some((generation.get(), *reason))
            }
            _ => None,
        })
        .collect()
}

/// Учёт загрузок плеера-подделки: тест сообщает ему, что делает плеер, а медиа-часть
/// видит отказ так же, как от настоящего плеера (`PlaybackObservation::fault`).
struct LedgerScript {
    ledger: LoadLedger,
}

impl LedgerScript {
    fn new() -> Self {
        Self {
            ledger: LoadLedger::default(),
        }
    }

    /// Плеер начал загрузку `index` (порядок загрузок медиа-части).
    fn begin(&mut self, harness: &Harness, index: usize) -> MediaLoadTicket {
        let ticket = harness.player.load_ticket(index);
        self.ledger.begin_load(ticket);
        ticket
    }

    /// Отказ (или его отсутствие) — в наблюдение плеера-подделки.
    fn publish(&self, harness: &Harness) {
        harness.player.observation.borrow_mut().fault =
            self.ledger.observation(&PlayerSnapshot::default()).fault;
    }
}

/// Сеанс 1, поколение 1 загружено в плеер (ещё ставится: не показано).
fn installing_harness() -> (Harness, SessionRef, PushedStreamWriter) {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (writer, notice) = stream_generation(&current, 1, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    harness.page.take_sent();
    (harness, current, writer)
}

/// Несколько проходов с разницей во времени: отказ не повторяется ни на одном.
fn drive_a_while(harness: &mut Harness) -> Vec<PlayerSessionMessage> {
    let start = Instant::now();
    for offset_ms in [0, 300, 1_200] {
        harness
            .media
            .drive(start + Duration::from_millis(offset_ms));
    }
    harness.page.take_sent()
}

/// Плеер отказал загрузке поколения 1 так, как сам об отказе (не) сообщил.
fn fault_the_first_load(
    harness: &Harness,
    fault: BrowserPlaybackFault,
    announcement: FaultAnnouncement,
) {
    harness.player.observation.borrow_mut().fault = Some(LoadFault {
        load: harness.player.load_ticket(0),
        fault,
        announcement,
    });
}

#[test]
fn an_aborted_install_rejects_its_generation_once_and_tells_the_viewer() {
    let (mut harness, _current, _writer) = installing_harness();
    let mut ledger = LedgerScript::new();
    let first = ledger.begin(&harness, 0);
    harness.player.take_calls();

    // Установку текущей загрузки отменили, и никто её не заменял: это её отказ.
    assert_eq!(
        ledger.ledger.note_install_cancelled(first),
        CancelledInstall::CurrentLoadAborted
    );
    ledger.publish(&harness);

    assert_eq!(
        rejections(&drive_a_while(&mut harness)),
        [(1, StreamRejection::PlaybackFailed)]
    );
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Rejected(StreamRejection::PlaybackFailed)
    );
    // Окно показывает отказ один раз.
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Nothing
    );
    // Отказ странице не командует плеером: ни паузы, ни новой загрузки.
    assert_eq!(harness.player.take_calls(), []);
}

#[test]
fn replacing_a_generation_on_purpose_rejects_nothing() {
    let (mut harness, current, _first_writer) = installing_harness();
    let mut ledger = LedgerScript::new();
    let first = ledger.begin(&harness, 0);

    // Страница сменила MediaSource, пока поколение 1 ставится: поколение 2 заменяет его.
    let (_second_writer, notice) = stream_generation(&current, 2, true);
    harness.media.on_intake(vec![notice], None);
    harness.drive_until_loaded();
    let second = ledger.begin(&harness, 1);
    // Плеер отменил установку заменённого поколения — норма «новее заменяет».
    assert_eq!(
        ledger.ledger.note_install_cancelled(first),
        CancelledInstall::ReplacedByNewerLoad
    );
    ledger.publish(&harness);

    assert_eq!(rejections(&drive_a_while(&mut harness)), []);
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Nothing
    );

    // То же правило не глухо: если отменят уже текущее поколение 2 — это его отказ.
    assert_eq!(
        ledger.ledger.note_install_cancelled(second),
        CancelledInstall::CurrentLoadAborted
    );
    ledger.publish(&harness);
    assert_eq!(
        rejections(&drive_a_while(&mut harness)),
        [(2, StreamRejection::PlaybackFailed)]
    );
}

#[test]
fn each_player_fault_is_rejected_with_its_protocol_reason() {
    let cases = [
        (
            BrowserPlaybackFault::UnsupportedVideo,
            StreamRejection::UnsupportedCodec,
        ),
        (
            BrowserPlaybackFault::VideoDecoderStartFailed,
            StreamRejection::PlaybackFailed,
        ),
        (
            BrowserPlaybackFault::PlaybackFailed(PlayerErrorKind::DemuxError),
            StreamRejection::PlaybackFailed,
        ),
        (
            BrowserPlaybackFault::InstallAborted,
            StreamRejection::PlaybackFailed,
        ),
    ];
    for (fault, reason) in cases {
        let (mut harness, _current, _writer) = installing_harness();
        fault_the_first_load(&harness, fault.clone(), FaultAnnouncement::NotAnnounced);
        assert_eq!(
            rejections(&drive_a_while(&mut harness)),
            [(1, reason)],
            "{fault:?}"
        );
        assert_eq!(
            harness.media.take_generation_rejection_notice(),
            GenerationRejectionNotice::Rejected(reason),
            "{fault:?}"
        );
    }
}

#[test]
fn a_fault_the_player_announced_itself_is_not_repeated_to_the_viewer() {
    let (mut harness, _current, _writer) = installing_harness();
    // Плеер упал в `Failed`: окно уже показало его ошибку в центре (`player_feed`).
    fault_the_first_load(
        &harness,
        BrowserPlaybackFault::PlaybackFailed(PlayerErrorKind::DemuxError),
        FaultAnnouncement::AnnouncedByPlayer,
    );
    // Странице отказ уходит всё равно: ей плеер ничего не говорил.
    assert_eq!(
        rejections(&drive_a_while(&mut harness)),
        [(1, StreamRejection::PlaybackFailed)]
    );
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Nothing
    );
}

#[test]
fn a_rejection_by_the_stream_intake_tells_the_viewer_once() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (_writer, notice) = stream_generation(&current, 1, false);
    let reject = || IntakeNotice::Reject {
        session: current.clone(),
        generation: generation(1),
        reason: StreamRejection::ProtectedContent,
    };
    harness.media.on_intake(vec![notice, reject()], None);
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Rejected(StreamRejection::ProtectedContent)
    );
    // Повтор отказа того же поколения странице не уходит — и зрителю тоже.
    harness.media.on_intake(vec![reject()], None);
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Nothing
    );
    assert_eq!(
        rejections(&drive_a_while(&mut harness)),
        [(1, StreamRejection::ProtectedContent)]
    );
}

#[test]
fn after_the_session_ended_a_fault_is_told_to_nobody() {
    let (mut harness, current, _writer) = installing_harness();
    // Вкладка ушла: доигрываем, странице больше некому сказать.
    harness.media.on_session_ended(&current, TAB_GONE_END);
    fault_the_first_load(
        &harness,
        BrowserPlaybackFault::InstallAborted,
        FaultAnnouncement::NotAnnounced,
    );
    assert_eq!(rejections(&drive_a_while(&mut harness)), []);
    assert_eq!(
        harness.media.take_generation_rejection_notice(),
        GenerationRejectionNotice::Nothing
    );
}
