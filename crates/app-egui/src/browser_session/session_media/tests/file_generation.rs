//! Поколение-файл в медиа-части (03c): настоящий источник файла (`pushed_file`) на
//! настоящих файлах-фикстурах, «страница» отвечает на `fetch_range` байтами; плеер —
//! подделка, поэтому проверяется всё до загрузки в плеер и ответы странице.
//!
//! Фикстуры файлов (`file-*.mp4`, 4 с, 100 кадров) лежат в `pushed-media-source`:
//! это единственный источник правды о том, какие файлы источник файла умеет играть, и
//! копия в app-egui разошлась бы с ним.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use browser_bridge_protocol::session::player::StreamRejection;
use media_core::DemuxSeekability;
use media_prefetch::PrefetchConfig;
use pushed_media_source::{
    PushedFilePolicy, PushedFileWriter, RangeOutcome, RangeRequestId, RangeRequestIds,
    RangeRequestNotifier, RangeRetryPolicy, ResourceSizeReport, pushed_file,
};

use super::*;

/// Файл с оглавлением в конце и файл с оглавлением в начале (оба 4 с, H.264 + AAC).
const MOOV_AT_END: &str = "file-h264-aac-moov-end.mp4";
const FASTSTART: &str = "file-h264-aac-faststart.mp4";
/// Зашифрованный файл.
const CENC: &str = "file-h264-cenc.mp4";

/// Самая большая рамка байтов, как у протокола (≤ 1 МиБ).
const MAX_FRAME: usize = 1024 * 1024;

fn file_fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pushed-media-source/fixtures")
        .join(name);
    std::fs::read(path).expect("file fixture")
}

/// Будильник-счётчик: сколько раз источник просил разбудить главный цикл.
#[derive(Default)]
struct CountingNotifier(AtomicUsize);

impl RangeRequestNotifier for CountingNotifier {
    fn range_requested(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Окно предзагрузки маленькое: файл (~100 КиБ) не помещается в него целиком.
fn small_policy() -> PushedFilePolicy {
    let prefetch = PrefetchConfig::new(2 * 1024, 4 * 1024, 8 * 1024).expect("prefetch");
    PushedFilePolicy::new(
        prefetch,
        RangeRetryPolicy::with_budget(Duration::from_secs(5)),
    )
    .expect("policy")
}

/// Вид размера файла, о котором «страница» сообщает в итоге запроса.
#[derive(Clone, Copy)]
enum SizeReport {
    Known,
    Unknown,
}

/// «Страница» файла: отвечает на `fetch_range` байтами фикстуры, как экстеншен.
struct FilePage {
    writer: PushedFileWriter,
    bytes: Vec<u8>,
    size: SizeReport,
    /// Страница молчит: запросы записывает, но не отвечает.
    silent: bool,
    /// Все запросы, которые страница получила, по порядку: (номер, смещение, длина).
    requests: Vec<(u32, u64, u64)>,
}

impl FilePage {
    /// Отвечает на каждый `fetch_range` из `messages`.
    fn answer(&mut self, messages: &[PlayerSessionMessage]) {
        for message in messages {
            let PlayerSessionMessage::FetchRange {
                request_id,
                offset,
                length,
            } = message
            else {
                continue;
            };
            self.requests
                .push((request_id.0, offset.as_bytes(), length.as_bytes()));
            if self.silent {
                continue;
            }
            let start = usize::try_from(offset.as_bytes()).expect("offset");
            let end =
                (start + usize::try_from(length.as_bytes()).expect("length")).min(self.bytes.len());
            let id = RangeRequestId::new(request_id.0);
            let mut position = start;
            while position < end {
                let frame_end = (position + MAX_FRAME).min(end);
                self.writer
                    .accept_range_bytes(id, position as u64, &self.bytes[position..frame_end])
                    .expect("the source accepts the frame");
                position = frame_end;
            }
            let resource_size = match self.size {
                SizeReport::Known => ResourceSizeReport::Known(self.bytes.len() as u64),
                SizeReport::Unknown => ResourceSizeReport::Unknown,
            };
            self.writer
                .finish_request(id, RangeOutcome::Completed { resource_size })
                .expect("the source accepts the result");
        }
    }
}

/// Файл поколения `number` на сеансе: страница и уведомление главному циклу.
fn file_generation(
    session: &SessionRef,
    number: u32,
    fixture: &str,
    size: SizeReport,
    notifier: Arc<CountingNotifier>,
) -> (FilePage, IntakeNotice) {
    let (writer, opener, requests) = pushed_file(small_policy(), RangeRequestIds::new(), notifier);
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: session.clone(),
        generation: generation(number),
        source: GenerationSource::File {
            opener,
            requests,
            prefetch: small_policy().prefetch(),
        },
    }));
    let page = FilePage {
        writer,
        bytes: file_fixture(fixture),
        size,
        silent: false,
        requests: Vec::new(),
    };
    (page, notice)
}

impl Harness {
    /// Крутит медиа-часть, отвечая странице-файлу, пока `done` не станет истиной.
    /// Возвращает всё, что ушло странице.
    fn drive_with_page(
        &mut self,
        page: &mut FilePage,
        mut done: impl FnMut(&Self, &[PlayerSessionMessage]) -> bool,
    ) -> Vec<PlayerSessionMessage> {
        let deadline = Instant::now() + WAIT;
        let mut all_sent = Vec::new();
        loop {
            let now = Instant::now();
            let wake_at = self.media.drive(now);
            let sent = self.page.take_sent();
            page.answer(&sent);
            all_sent.extend(sent);
            if done(self, &all_sent) {
                return all_sent;
            }
            assert!(now < deadline, "the awaited state never came: {all_sent:?}");
            let pause = wake_at.map_or(Duration::from_millis(5), |wake_at| {
                wake_at.saturating_duration_since(now)
            });
            std::thread::sleep(pause.clamp(Duration::from_millis(1), Duration::from_millis(20)));
        }
    }

    fn player_loaded(&self) -> bool {
        !self.player.loaded_media.borrow().is_empty()
    }
}

fn fetch_ranges(messages: &[PlayerSessionMessage]) -> Vec<(u32, u64, u64)> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::FetchRange {
                request_id,
                offset,
                length,
            } => Some((request_id.0, offset.as_bytes(), length.as_bytes())),
            _ => None,
        })
        .collect()
}

fn rejection_reasons(messages: &[PlayerSessionMessage]) -> Vec<StreamRejection> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::StreamRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect()
}

/// Главное: файл поколения доходит до плеера. Источник просит байты, главный цикл
/// отправляет их странице как `fetch_range`, страница отвечает, файл открывается и
/// плеер получает медиа с дорожками, длительностью и перемоткой. Странице при этом не
/// уходит ни одного `buffer_state` (она ничего не качает сама).
#[test]
fn a_file_generation_is_opened_by_range_requests_and_loaded_into_the_player() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let notifier = Arc::new(CountingNotifier::default());
    let (mut page, notice) = file_generation(
        &current,
        1,
        MOOV_AT_END,
        SizeReport::Known,
        Arc::clone(&notifier),
    );
    harness.media.on_intake(vec![notice], None);

    let sent = harness.drive_with_page(&mut page, |harness, _| harness.player_loaded());

    // Первый запрос — начало файла, номер 1; страница получила его от медиа-части.
    let requests = fetch_ranges(&sent);
    assert_eq!(requests.first().map(|r| (r.0, r.1)), Some((1, 0)));
    assert_eq!(page.requests, requests);
    assert!(
        notifier.0.load(Ordering::SeqCst) >= requests.len(),
        "every request woke the loop"
    );
    // Плеер получил играбельное медиа: две дорожки, 4 с, перематывается.
    assert_eq!(
        harness.player.take_calls(),
        vec![PlayerCall::Load(
            BROWSER_STREAM_LABEL.to_owned(),
            LoadStart::Playing
        )]
    );
    let media = harness.player.loaded_media.borrow()[0].clone();
    assert_eq!(media.track_count, 2);
    assert_eq!(media.duration, Some(Duration::from_secs(4)));
    assert_eq!(media.seekability, DemuxSeekability::Seekable);
    assert!(
        buffer_generations(&sent).is_empty(),
        "a file generation never reports a buffer to the page"
    );
}

/// Пока файл играет, запросы байтов продолжают уходить странице (окно предзагрузки
/// дочитывается), а часы и монитор позиции — как у любого загруженного поколения, но
/// без `buffer_state` и без просьб перемотать страницу.
#[test]
fn a_loaded_file_keeps_asking_the_page_for_bytes_and_reports_no_buffer() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut page, notice) = file_generation(
        &current,
        1,
        FASTSTART,
        SizeReport::Known,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![notice], None);
    harness.drive_with_page(&mut page, |harness, _| harness.player_loaded());
    let before_load = page.requests.len();
    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::from_secs(1), PlaybackState::Playing);

    let sent = harness.drive_with_page(&mut page, |_, sent| {
        !fetch_ranges(sent).is_empty() && !clocks(sent).is_empty()
    });

    assert!(
        page.requests.len() > before_load,
        "the loaded file's background reader asks for more bytes: {:?}",
        page.requests
    );
    assert!(
        clocks(&sent)
            .iter()
            .any(|(position, state)| *position == 1_000_000 && *state == ClockState::Playing),
        "the page clock follows the player: {sent:?}"
    );
    assert!(buffer_generations(&sent).is_empty());
    assert!(
        !sent
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::Seek { .. })),
        "a file never asks the page to seek its element"
    );
}

/// Файл без известного размера играет без перемотки (решение владельца).
#[test]
fn a_file_of_unknown_size_is_loaded_without_seeking() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut page, notice) = file_generation(
        &current,
        1,
        FASTSTART,
        SizeReport::Unknown,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![notice], None);

    harness.drive_with_page(&mut page, |harness, _| harness.player_loaded());

    let media = harness.player.loaded_media.borrow()[0].clone();
    assert!(
        matches!(media.seekability, DemuxSeekability::NotSeekable { .. }),
        "no known size means forward-only playback: {media:?}"
    );
}

/// Страница назвала позицию, пока файл открывался (03e): файл перематывается туда,
/// но только когда плеер его показал — до показа команда досталась бы прежнему медиа.
#[test]
fn a_page_seek_while_a_file_opens_seeks_the_file_once_the_player_shows_it() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut page, notice) = file_generation(
        &current,
        1,
        FASTSTART,
        SizeReport::Known,
        Arc::new(CountingNotifier::default()),
    );
    let target = Duration::from_secs(2);
    let page_target = MediaTime::from_micros(2_000_000).expect("time");
    harness.media.on_intake(
        vec![
            notice,
            intent(&current, PageIntent::FollowPageSeek(page_target)),
        ],
        None,
    );
    harness.drive_with_page(&mut page, |harness, _| harness.player_loaded());
    let calls = harness.player.take_calls();
    assert!(
        !calls.iter().any(|call| matches!(call, PlayerCall::Seek(_))),
        "seek before the player shows the file: {calls:?}"
    );

    let load = harness.player.load_ticket(0);
    harness
        .player
        .show_load(load, Duration::ZERO, PlaybackState::Playing);
    harness.drive_with_page(&mut page, |harness, _| {
        harness
            .player
            .calls
            .borrow()
            .iter()
            .any(|call| matches!(call, PlayerCall::Seek(_)))
    });
    let seeks: Vec<PlayerCall> = harness
        .player
        .take_calls()
        .into_iter()
        .filter(|call| matches!(call, PlayerCall::Seek(_)))
        .collect();
    assert_eq!(seeks, vec![PlayerCall::Seek(target)]);
}

/// Оглавление MP4 в конце без известного размера — честный отказ странице
/// (`unsupported_codec`), а не вечное ожидание.
#[test]
fn a_movie_at_the_end_without_a_known_size_is_rejected_as_unsupported() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut page, notice) = file_generation(
        &current,
        1,
        MOOV_AT_END,
        SizeReport::Unknown,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![notice], None);

    let sent = harness.drive_with_page(&mut page, |_, sent| !rejection_reasons(sent).is_empty());

    assert_eq!(
        rejection_reasons(&sent),
        vec![StreamRejection::UnsupportedCodec]
    );
    assert!(!harness.player_loaded());
}

/// Зашифрованный файл — `stream_rejected{protected_content}`, плееру он не уходит.
#[test]
fn an_encrypted_file_is_rejected_as_protected_content() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut page, notice) = file_generation(
        &current,
        1,
        CENC,
        SizeReport::Known,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![notice], None);

    let sent = harness.drive_with_page(&mut page, |_, sent| !rejection_reasons(sent).is_empty());

    assert_eq!(
        rejection_reasons(&sent),
        vec![StreamRejection::ProtectedContent]
    );
    assert!(!harness.player_loaded());
}

/// Страница, ушедшая до открытия (файл запечатан без размера), получает
/// `file_unavailable` — а не вечное ожидание открытия.
#[test]
fn a_file_sealed_before_it_opened_is_rejected_as_unavailable() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (mut page, notice) = file_generation(
        &current,
        1,
        FASTSTART,
        SizeReport::Known,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![notice], None);
    // Первый проход запускает открытие и выдаёт первый запрос; страница молчит и уходит.
    page.silent = true;
    let sent = harness.drive_with_page(&mut page, |_, sent| !fetch_ranges(sent).is_empty());
    assert_eq!(fetch_ranges(&sent).len(), 1);
    page.writer.seal();

    let sent = harness.drive_with_page(&mut page, |_, sent| !rejection_reasons(sent).is_empty());

    assert_eq!(
        rejection_reasons(&sent),
        vec![StreamRejection::FileUnavailable]
    );
}

/// Файл того же поколения заменяет ждущий поток сегментов: `generation_started`
/// создаёт поток сразу, `file_source_added` приходит следом. Наоборот не бывает:
/// поток сегментов того же поколения ждущий файл не вытесняет.
#[test]
fn a_file_replaces_the_pending_segments_of_the_same_generation_but_not_the_other_way_round() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    // Поток сегментов без медиа: ждёт открытия.
    let (_segments_writer, segments) = stream_generation(&current, 1, false);
    harness.media.on_intake(vec![segments], None);
    assert!(
        harness
            .media
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.feed.is_file())
    );

    let (mut page, file) = file_generation(
        &current,
        1,
        FASTSTART,
        SizeReport::Known,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![file], None);
    assert!(
        harness
            .media
            .pending
            .as_ref()
            .is_some_and(|pending| pending.feed.is_file())
    );

    let (_late_writer, late_segments) = stream_generation(&current, 1, false);
    harness.media.on_intake(vec![late_segments], None);
    assert!(
        harness
            .media
            .pending
            .as_ref()
            .is_some_and(|pending| pending.feed.is_file()),
        "segments of the same generation must not push the file out"
    );
    harness.drive_with_page(&mut page, |harness, _| harness.player_loaded());
    assert_eq!(harness.player.loaded_media.borrow().len(), 1);
}

/// Переполнение очереди запросов к странице — внутренняя ошибка (при честной работе
/// недостижимая): запрос не теряется молча, а сеанс закрывается как нарушение протокола.
#[test]
fn a_full_range_queue_closes_the_session_instead_of_losing_a_request() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    // Страница не принимает ничего: очередь неотправленных запросов набита до предела.
    for _ in 0..1_000 {
        harness
            .page
            .refusals
            .borrow_mut()
            .push_back(SendToPageError::QueueFull);
    }
    for id in 1..=crate::browser_session::outbound::MAX_PENDING_RANGE_REQUESTS {
        let id = u32::try_from(id).expect("small");
        let queued = harness
            .media
            .outbound
            .request_range(pushed_media_source::RangeRequest {
                id: RangeRequestId::new(1_000 + id),
                offset: 0,
                length: std::num::NonZeroU32::new(10).expect("non-zero"),
            });
        assert_eq!(
            queued,
            crate::browser_session::outbound::RangeQueueOutcome::Queued
        );
    }
    let (_page, notice) = file_generation(
        &current,
        1,
        FASTSTART,
        SizeReport::Known,
        Arc::new(CountingNotifier::default()),
    );
    harness.media.on_intake(vec![notice], None);

    let deadline = Instant::now() + WAIT;
    while harness.closer.closed.borrow().is_empty() {
        harness.media.drive(Instant::now());
        assert!(Instant::now() < deadline, "the session was never closed");
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(
        harness.closer.closed.borrow().first(),
        Some(&(current, SessionCloseReason::ProtocolViolation))
    );
    assert_eq!(
        harness.media.outbound.range_counters().refused_queue_full,
        1
    );
}
