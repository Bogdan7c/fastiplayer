//! Поколение-файл в переходнике (03c): `file_source_added`, рамки вида 2, итоги
//! запросов, ответы на ошибки файла и запечатывание в конце сеанса.
//!
//! Писатель файла — подделка с записью вызовов и заданными ответами; настоящий
//! источник файла (с демуксером и плеером) проверяют тесты `session_media` и сквозной
//! тест процесса.

use std::collections::VecDeque;

use browser_bridge_protocol::session::page::{RangeFetchFailure, RangeFetchOutcome, ResourceSize};
use browser_bridge_protocol::{ByteOffset, RangeRequestId as WireRangeRequestId};
use pushed_media_source::{
    PushedFileError, PushedFilePolicy, RangeBytesVerdict, RangeFailure, RangeOutcome,
    RangeRequestId, RangeRequestIds, ResourceSizeReport, pushed_file,
};

use super::file_port::{FileFactory, FileWriterPort, StartedFile};
use super::*;
use crate::browser_session::stream_queue::OwnedRangeChunk;

/// Вызов писателя файла, как его увидел источник.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileCallRecord {
    Bytes(u32, u64, Vec<u8>),
    Finish(u32, RangeOutcome),
    Seal,
}

/// Общая запись вызовов и заданные ответы подделки.
#[derive(Clone, Default)]
struct FileScript {
    calls: Arc<Mutex<Vec<FileCallRecord>>>,
    /// Ответы на рамки по очереди; пусто — `Accepted`.
    bytes_answers: Arc<Mutex<VecDeque<Result<RangeBytesVerdict, PushedFileError>>>>,
    /// Ответы на итоги по очереди; пусто — успех.
    finish_answers: Arc<Mutex<VecDeque<Result<(), PushedFileError>>>>,
    /// Сколько файлов создано.
    started: Arc<Mutex<usize>>,
}

impl FileScript {
    fn calls(&self) -> Vec<FileCallRecord> {
        self.calls.lock().expect("calls").clone()
    }

    fn answer_bytes_with(&self, answer: Result<RangeBytesVerdict, PushedFileError>) {
        self.bytes_answers
            .lock()
            .expect("answers")
            .push_back(answer);
    }

    fn answer_finish_with(&self, answer: Result<(), PushedFileError>) {
        self.finish_answers
            .lock()
            .expect("answers")
            .push_back(answer);
    }

    fn started(&self) -> usize {
        *self.started.lock().expect("started")
    }
}

struct FakeFileWriter(FileScript);

impl FileWriterPort for FakeFileWriter {
    fn accept_range_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<RangeBytesVerdict, PushedFileError> {
        self.0
            .calls
            .lock()
            .expect("calls")
            .push(FileCallRecord::Bytes(id.get(), offset, payload.to_vec()));
        self.0
            .bytes_answers
            .lock()
            .expect("answers")
            .pop_front()
            .unwrap_or(Ok(RangeBytesVerdict::Accepted))
    }

    fn finish_request(
        &mut self,
        id: RangeRequestId,
        outcome: RangeOutcome,
    ) -> Result<(), PushedFileError> {
        self.0
            .calls
            .lock()
            .expect("calls")
            .push(FileCallRecord::Finish(id.get(), outcome));
        self.0
            .finish_answers
            .lock()
            .expect("answers")
            .pop_front()
            .unwrap_or(Ok(()))
    }

    fn seal(&mut self) {
        self.0
            .calls
            .lock()
            .expect("calls")
            .push(FileCallRecord::Seal);
    }
}

struct FakeFileFactory(FileScript);

impl FileFactory for FakeFileFactory {
    fn start_file(&mut self, ids: RangeRequestIds) -> StartedFile {
        *self.0.started.lock().expect("started") += 1;
        // Сторона открытия и очередь запросов — настоящие (писатель настоящего
        // источника тут же бросается), вызовы пишет подделка.
        struct Silent;
        impl pushed_media_source::RangeRequestNotifier for Silent {
            fn range_requested(&self) {}
        }
        let policy = PushedFilePolicy::recommended().expect("policy");
        let (_real_writer, mut opener, requests) = pushed_file(policy, ids, Arc::new(Silent));
        // Как в настоящем процессе, открытие выдаёт первый запрос (номер 1 сеанса):
        // без этого выдаватель номеров считал бы любой номер невыданным.
        let _progress = opener.poll_open();
        let deadline = Instant::now() + Duration::from_secs(10);
        while requests.take_requests().is_empty() {
            assert!(Instant::now() < deadline, "the first request never came");
            std::thread::sleep(Duration::from_millis(2));
        }
        StartedFile {
            writer: Box::new(FakeFileWriter(self.0.clone())),
            opener,
            requests,
            prefetch: policy.prefetch(),
        }
    }
}

fn file_translator() -> (ProtocolTranslator<FakeFactory>, CallLog, FileScript) {
    let factory = FakeFactory::default();
    let log = factory.log.clone();
    let script = FileScript::default();
    (
        ProtocolTranslator::new(factory, Box::new(FakeFileFactory(script.clone()))),
        log,
        script,
    )
}

fn file_source_added(instance: u64, number: u32) -> SessionInput {
    message(
        instance,
        PageSessionMessage::FileSourceAdded {
            generation: generation(number),
        },
    )
}

fn range_bytes(instance: u64, request: u32, offset: u64, payload: &[u8]) -> SessionInput {
    SessionInput::RangeBytes(
        session(instance),
        OwnedRangeChunk {
            request_id: WireRangeRequestId(request),
            offset,
            payload: payload.to_vec(),
        },
    )
}

fn fetch_finished(instance: u64, request: u32, outcome: RangeFetchOutcome) -> SessionInput {
    message(
        instance,
        PageSessionMessage::RangeFetchFinished {
            request_id: WireRangeRequestId(request),
            outcome,
        },
    )
}

fn completed_known(size: u64) -> RangeFetchOutcome {
    RangeFetchOutcome::Completed {
        resource_size: ResourceSize::Known {
            size_bytes: ByteOffset::new(size).expect("offset"),
        },
    }
}

/// Поколение, начатое как MSE, становится файлом: главный цикл получает сначала
/// ждущий поток сегментов, потом файл ТОГО ЖЕ поколения; писатель сегментов забыт, а
/// рамки байтов идут писателю файла.
#[test]
fn file_source_added_turns_the_generation_into_a_file() {
    let (mut translator, segments_log, script) = file_translator();

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            range_bytes(1, 1, 0, b"ftyp"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::NewFileGeneration(1, 1),
            Seen::BytesAppended(1),
        ]
    );
    assert_eq!(script.started(), 1);
    assert_eq!(
        script.calls(),
        vec![FileCallRecord::Bytes(1, 0, b"ftyp".to_vec())]
    );
    assert!(
        segments_log.calls().is_empty(),
        "the segments writer must not get anything"
    );
    let counters = translator.counters();
    assert_eq!(
        (
            counters.range_frames_accepted,
            counters.range_bytes_accepted
        ),
        (1, 4)
    );
}

/// Рамки, которые источник счёл запоздавшими, молча считаются устаревшими:
/// ни ответа странице, ни закрытия, ни подсказки главному циклу.
#[test]
fn stale_range_frames_are_counted_and_stay_silent() {
    let (mut translator, _log, script) = file_translator();
    script.answer_bytes_with(Ok(RangeBytesVerdict::StaleDropped));

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            range_bytes(1, 3, 0, b"late"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![Seen::NewGeneration(1, 1), Seen::NewFileGeneration(1, 1)]
    );
    let counters = translator.counters();
    assert_eq!(counters.range_frames_stale, 1);
    assert_eq!(counters.range_frames_accepted, 0);
}

/// Рамка, когда в сеансе файлов не было вовсе: номер никто не выдавал — выдумка
/// страницы, сеанс закрывается кодом 4003, и вход экземпляра дальше глохнет.
#[test]
fn a_range_frame_in_a_session_without_any_file_is_a_protocol_violation() {
    let (mut translator, _log, script) = file_translator();

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            added(1, 1, 0, "video/mp4"),
            range_bytes(1, 7, 0, b"made up"),
            range_bytes(1, 7, 7, b"more"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Close(1, SessionCloseReason::ProtocolViolation),
        ]
    );
    assert!(script.calls().is_empty());
    assert_eq!(translator.counters().closing_instance_inputs, 1);
}

/// Итог запроса в сеансе без файлов — тоже нарушение.
#[test]
fn a_fetch_result_in_a_session_without_any_file_is_a_protocol_violation() {
    let (mut translator, _log, _script) = file_translator();

    let notices = run(
        &mut translator,
        [started(1, 1), fetch_finished(1, 1, completed_known(10))],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Close(1, SessionCloseReason::ProtocolViolation),
        ]
    );
}

/// Файл был (его первый запрос выдал номер 1), потом пришло MSE-поколение: запоздавшая
/// рамка старого файла устарела, это не нарушение, а выдуманный номер выше последнего
/// выданного — нарушение даже после первого файла.
#[test]
fn a_late_frame_of_a_replaced_file_is_stale_not_a_violation() {
    let (mut translator, _log, script) = file_translator();

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            started(1, 2),
            range_bytes(1, 1, 0, b"late"),
            fetch_finished(1, 1, completed_known(4)),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::NewFileGeneration(1, 1),
            Seen::NewGeneration(1, 2),
        ]
    );
    assert!(script.calls().is_empty(), "the old file is already gone");
    let counters = translator.counters();
    assert_eq!(counters.range_frames_stale, 1);
    assert_eq!(counters.stale_inputs, 1);
}

/// Номер, которого сеанс не выдавал (выше последнего выданного), — выдумка страницы и
/// после первого файла: и рамка, и итог запроса закрывают сеанс кодом 4003.
#[test]
fn an_invented_request_number_is_a_violation_even_after_the_first_file() {
    for invented in [
        range_bytes(1, 99, 0, b"made up"),
        fetch_finished(1, 99, completed_known(7)),
    ] {
        let (mut translator, _log, script) = file_translator();

        let notices = run(
            &mut translator,
            [
                started(1, 1),
                file_source_added(1, 1),
                // Файл заменён MSE-поколением: текущего файла нет.
                started(1, 2),
                invented,
            ],
        );

        assert_eq!(
            seen(&notices).last(),
            Some(&Seen::Close(1, SessionCloseReason::ProtocolViolation))
        );
        assert!(script.calls().is_empty());
        assert_eq!(translator.counters().range_frames_stale, 0);
    }
}

/// Отвергнутое поколение не прячет выдуманный номер: рамка и итог с номером, которого
/// сеанс не выдавал, закрывают сеанс кодом 4003 и тогда, когда от файла уже отказались;
/// запоздавший выданный номер по-прежнему молча отвергается.
#[test]
fn a_rejected_generation_still_catches_an_invented_request_number() {
    for invented in [
        range_bytes(1, 99, 0, b"made up"),
        fetch_finished(1, 99, completed_known(7)),
    ] {
        let (mut translator, _log, script) = file_translator();
        script.answer_finish_with(Err(PushedFileError::ResourceChanged));

        let notices = run(
            &mut translator,
            [
                started(1, 1),
                file_source_added(1, 1),
                fetch_finished(1, 1, completed_known(7)),
                invented,
            ],
        );

        assert_eq!(
            seen(&notices)
                .iter()
                .filter(|n| matches!(n, Seen::Reject(..)))
                .count(),
            1,
            "the file is rejected once"
        );
        assert_eq!(
            seen(&notices).last(),
            Some(&Seen::Close(1, SessionCloseReason::ProtocolViolation))
        );
    }

    // Выданный номер (1) после отказа — не нарушение.
    let (mut translator, _log, script) = file_translator();
    script.answer_finish_with(Err(PushedFileError::ResourceChanged));
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            fetch_finished(1, 1, completed_known(7)),
            range_bytes(1, 1, 0, b"late"),
        ],
    );
    assert!(
        !seen(&notices)
            .iter()
            .any(|notice| matches!(notice, Seen::Close(..)))
    );
    assert_eq!(translator.counters().range_frames_rejected, 1);
}

/// Номера запросов сквозные между файлами сеанса: второй файл получает тот же
/// выдаватель (запоздалые рамки первого он узнает как устаревшие, а не как нарушение).
#[test]
fn every_file_of_a_session_shares_the_request_numbering() {
    let ids_seen = Arc::new(Mutex::new(Vec::<RangeRequestIds>::new()));
    struct RecordingFactory(Arc<Mutex<Vec<RangeRequestIds>>>);
    impl FileFactory for RecordingFactory {
        fn start_file(&mut self, ids: RangeRequestIds) -> StartedFile {
            self.0.lock().expect("ids").push(ids.clone());
            FakeFileFactory(FileScript::default()).start_file(ids)
        }
    }
    let mut translator = ProtocolTranslator::new(
        FakeFactory::default(),
        Box::new(RecordingFactory(ids_seen.clone())),
    );

    run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            started(1, 2),
            file_source_added(1, 2),
        ],
    );

    let ids = ids_seen.lock().expect("ids");
    assert_eq!(ids.len(), 2);
    // Один и тот же счётчик: номер, выданный через один клон, виден через другой.
    let first = ids[0].clone();
    let second = ids[1].clone();
    drop(ids);
    let first_id = first_request_id(first);
    let second_id = first_request_id(second);
    assert!(
        second_id > first_id,
        "numbers continue across files: {first_id:?} then {second_id:?}"
    );
}

/// Открывает настоящий файл на выдавателе `ids` и ждёт номер его первого запроса.
fn first_request_id(ids: RangeRequestIds) -> RangeRequestId {
    struct Silent;
    impl pushed_media_source::RangeRequestNotifier for Silent {
        fn range_requested(&self) {}
    }
    let (_writer, mut opener, requests) = pushed_file(
        PushedFilePolicy::recommended().expect("policy"),
        ids,
        Arc::new(Silent),
    );
    let _progress = opener.poll_open();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(request) = requests.take_requests().first() {
            return request.id;
        }
        assert!(Instant::now() < deadline, "the first request never came");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Итог запроса переводится в зеркальные типы источника: все исходы.
#[test]
fn fetch_results_reach_the_file_in_mirror_types() {
    let (mut translator, _log, script) = file_translator();
    let outcomes = [
        (
            completed_known(1_000),
            RangeOutcome::Completed {
                resource_size: ResourceSizeReport::Known(1_000),
            },
        ),
        (
            RangeFetchOutcome::Completed {
                resource_size: ResourceSize::Unknown {},
            },
            RangeOutcome::Completed {
                resource_size: ResourceSizeReport::Unknown,
            },
        ),
        (
            RangeFetchOutcome::Failed {
                failure: RangeFetchFailure::NetworkError,
            },
            RangeOutcome::Failed(RangeFailure::Network),
        ),
        (
            RangeFetchOutcome::Failed {
                failure: RangeFetchFailure::HttpError,
            },
            RangeOutcome::Failed(RangeFailure::Http),
        ),
        (
            RangeFetchOutcome::Failed {
                failure: RangeFetchFailure::ResourceChanged,
            },
            RangeOutcome::Failed(RangeFailure::ResourceChanged),
        ),
        (
            RangeFetchOutcome::Failed {
                failure: RangeFetchFailure::OutOfRange,
            },
            RangeOutcome::Failed(RangeFailure::OutOfRange),
        ),
    ];
    let mut inputs = vec![started(1, 1), file_source_added(1, 1)];
    for (index, (wire, _mirror)) in outcomes.iter().enumerate() {
        inputs.push(fetch_finished(
            1,
            u32::try_from(index).expect("index") + 1,
            *wire,
        ));
    }

    let notices = run(&mut translator, inputs);

    let expected: Vec<FileCallRecord> = outcomes
        .iter()
        .enumerate()
        .map(|(index, (_wire, mirror))| {
            FileCallRecord::Finish(u32::try_from(index).expect("index") + 1, *mirror)
        })
        .collect();
    assert_eq!(script.calls(), expected);
    assert!(
        seen(&notices)
            .iter()
            .all(|notice| !matches!(notice, Seen::Close(..) | Seen::Reject(..))),
        "successful results answer nothing to the page: {notices:?}"
    );
}

/// Класс ошибки файла → верный ответ странице, ровно один раз на поколение; дальше
/// вход этого поколения глохнет со счётом.
#[test]
fn file_failures_answer_the_page_by_their_class_once() {
    let cases = [
        (
            PushedFileError::RetryBudgetExhausted { attempts: 4 },
            Seen::Reject(1, 1, StreamRejection::FileUnavailable),
        ),
        (
            PushedFileError::ProtectedContent,
            Seen::Reject(1, 1, StreamRejection::ProtectedContent),
        ),
        (
            PushedFileError::UnparsableMedia {
                reason: "broken".to_owned(),
            },
            Seen::Reject(1, 1, StreamRejection::UnparsableMedia),
        ),
        (
            PushedFileError::NonContiguousBytes {
                id: 1,
                expected: 0,
                actual: 9,
            },
            Seen::Close(1, SessionCloseReason::ProtocolViolation),
        ),
    ];
    for (error, expected) in cases {
        let (mut translator, _log, script) = file_translator();
        script.answer_finish_with(Err(error.clone()));

        let notices = run(
            &mut translator,
            [
                started(1, 1),
                file_source_added(1, 1),
                fetch_finished(
                    1,
                    1,
                    RangeFetchOutcome::Failed {
                        failure: RangeFetchFailure::NetworkError,
                    },
                ),
                range_bytes(1, 1, 0, b"after"),
            ],
        );

        let seen_notices = seen(&notices);
        assert_eq!(
            seen_notices
                .iter()
                .filter(|notice| matches!(notice, Seen::Reject(..) | Seen::Close(..)))
                .collect::<Vec<_>>(),
            vec![&expected],
            "{error}"
        );
        if matches!(expected, Seen::Reject(..)) {
            assert_eq!(translator.counters().range_frames_rejected, 1, "{error}");
            assert_eq!(script.calls().len(), 1, "the dead file gets no more input");
        }
    }
}

/// После `seal` рамка — не нарушение страницы (сеанс уже кончился): писатель
/// отвечает `WriterSealed`, переходник только считает.
#[test]
fn bytes_after_the_file_was_sealed_are_not_a_violation() {
    let (mut translator, _log, script) = file_translator();
    script.answer_bytes_with(Err(PushedFileError::WriterSealed));

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            range_bytes(1, 1, 0, b"x"),
        ],
    );

    assert_eq!(
        seen(&notices),
        vec![Seen::NewGeneration(1, 1), Seen::NewFileGeneration(1, 1)]
    );
    assert_eq!(translator.counters().range_frames_rejected, 1);
}

/// Конец сеанса запечатывает файл: полученное доигрывается.
#[test]
fn the_end_of_the_session_seals_the_file() {
    let (mut translator, _log, script) = file_translator();

    run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            range_bytes(1, 1, 0, b"abc"),
            SessionInput::Ended(session(1)),
        ],
    );

    assert_eq!(
        script.calls(),
        vec![
            FileCallRecord::Bytes(1, 0, b"abc".to_vec()),
            FileCallRecord::Seal,
        ]
    );
}

/// `tab_gone` только отмечается; запечатывает файл уже `Ended`, который сервер
/// кладёт в очередь следом.
#[test]
fn tab_gone_is_noted_and_the_following_end_seals_the_stream() {
    let (mut translator, log, script) = file_translator();

    run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            message(
                1,
                PageSessionMessage::TabGone {
                    reason: TabGoneReason::Closed,
                },
            ),
        ],
    );
    assert_eq!(translator.counters().tab_gone_messages, 1);
    assert!(
        !script.calls().contains(&FileCallRecord::Seal),
        "tab_gone alone does not seal: the server's Ended does"
    );

    run(&mut translator, [SessionInput::Ended(session(1))]);

    assert_eq!(script.calls(), vec![FileCallRecord::Seal]);
    assert!(log.calls().is_empty());
}

/// MSE-сообщение в поколении-файле (сторож сервера такого не пускает) — нарушение.
#[test]
fn media_source_input_in_a_file_generation_is_a_protocol_violation() {
    let (mut translator, log, _script) = file_translator();

    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            added(1, 1, 0, "video/mp4"),
        ],
    );

    assert_eq!(
        seen(&notices).last(),
        Some(&Seen::Close(1, SessionCloseReason::ProtocolViolation))
    );
    assert!(log.calls().is_empty());
}

/// `file_source_added` без начатого поколения — нарушение порядка; для прошлого
/// поколения — устаревшее.
#[test]
fn file_source_added_out_of_order_is_a_violation_or_stale() {
    let (mut translator, _log, script) = file_translator();
    let notices = run(&mut translator, [file_source_added(1, 1)]);
    // Экземпляра с поколением ещё нет: сообщение без `generation_started`.
    assert_eq!(
        seen(&notices),
        vec![Seen::Close(1, SessionCloseReason::ProtocolViolation)]
    );
    assert_eq!(script.started(), 0);

    let (mut translator, _log, script) = file_translator();
    let notices = run(&mut translator, [started(1, 2), file_source_added(1, 1)]);
    assert_eq!(seen(&notices), vec![Seen::NewGeneration(1, 2)]);
    assert_eq!(translator.counters().stale_inputs, 1);
    assert_eq!(script.started(), 0);
}
