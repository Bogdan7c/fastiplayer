//! HLS-поколение в переводчике (10a1): `hls_source_added`, выбор адресата рамок вида 2
//! (`claim_of`: файл и HLS на одном счётчике), итоги, нарушения и печать.
//!
//! Очередь ресурсов — НАСТОЯЩАЯ (`pushed-media-source`): тест сам выдаёт номера её
//! стороной движка, как это делал бы HLS, и читает пришедшие тела.

use std::num::NonZeroU64;

use browser_bridge_protocol::{
    RangeRequestId as WireRangeRequestId, ResourceBodySize, ResourceFetchFailure as WireFailure,
    ResourceFetchOutcome as WireOutcome, ResourceHandle,
};
use pushed_media_source::{
    ExternalResourceHandle, RangeRequestIds, RangeRequestNotifier, ResourceExtent, ResourceFetch,
    ResourceFetchClient, ResourceFetchError, ResourceFetchRequests, ResourceFetchSpec,
    resource_fetch_queue,
};
use source_core::CancellationToken;

use super::file_source::{FakeFileFactory, FileCallRecord, FileScript, file_source_added};
use super::*;
use crate::browser_session::hls_bridge::factory::{HlsFactory, StartedHls, bridge_resource_policy};
use crate::browser_session::hls_bridge::opener::{HlsGenerationOpener, HlsOpenSettings};
use crate::browser_session::hls_bridge::transport::BridgeResourceTransport;
use crate::browser_session::stream_queue::OwnedRangeChunk;

/// Что тест видит в каждом начатом HLS-поколении: номерок корня, сторону движка и
/// очередь отправки (её забирает главный цикл процесса).
#[derive(Clone, Default)]
struct HlsScript {
    started: Arc<Mutex<Vec<(u32, ResourceFetchClient, ResourceFetchRequests)>>>,
}

impl HlsScript {
    /// Сторона движка `index`-го поколения.
    fn client(&self, index: usize) -> ResourceFetchClient {
        self.started.lock().expect("started")[index].1.clone()
    }

    /// Движок `index`-го поколения просит «весь ресурс №`handle`», а главный цикл
    /// забирает просьбу для отправки (только после этого страница может отвечать).
    fn issue_and_send(&self, index: usize, handle: u32) -> ResourceFetch {
        let fetch = issue(&self.client(index), handle);
        let sent = self.started.lock().expect("started")[index]
            .2
            .take_requests();
        assert_eq!(sent.len(), 1, "the request goes out to the page");
        fetch
    }

    fn roots(&self) -> Vec<u32> {
        self.started
            .lock()
            .expect("started")
            .iter()
            .map(|(root, _, _)| *root)
            .collect()
    }
}

/// Фабрика с настоящей очередью; сторона движка достаётся тесту.
struct ScriptedHlsFactory(HlsScript);

impl HlsFactory for ScriptedHlsFactory {
    fn start_hls(&mut self, ids: RangeRequestIds, root: ExternalResourceHandle) -> StartedHls {
        struct Silent;
        impl RangeRequestNotifier for Silent {
            fn range_requested(&self) {}
        }
        let config = fastiplayer_config::AppConfig::default();
        let policy = bridge_resource_policy(&config.network).expect("policy");
        let (writer, client, requests) = resource_fetch_queue(policy, ids, Arc::new(Silent));
        self.0.started.lock().expect("started").push((
            root.get(),
            client.clone(),
            requests.clone(),
        ));
        StartedHls {
            writer: Box::new(writer),
            opener: HlsGenerationOpener::new(
                BridgeResourceTransport::new(client),
                requests.clone(),
                root,
                Arc::new(HlsOpenSettings::from_app_config(&config)),
            ),
            requests,
        }
    }
}

fn hls_translator() -> (ProtocolTranslator<FakeFactory>, HlsScript, FileScript) {
    let hls = HlsScript::default();
    let files = FileScript::default();
    let translator = ProtocolTranslator::new(
        FakeFactory::default(),
        Box::new(FakeFileFactory(files.clone())),
        Box::new(ScriptedHlsFactory(hls.clone())),
    );
    (translator, hls, files)
}

fn hls_source_added(instance: u64, number: u32, root: u32) -> SessionInput {
    message(
        instance,
        PageSessionMessage::HlsSourceAdded {
            generation: generation(number),
            root: ResourceHandle(root),
        },
    )
}

fn frame(instance: u64, request: u32, offset: u64, payload: &[u8]) -> SessionInput {
    SessionInput::RangeBytes(
        session(instance),
        OwnedRangeChunk {
            request_id: WireRangeRequestId(request),
            offset,
            payload: payload.to_vec(),
        },
    )
}

fn resource_finished(instance: u64, request: u32, outcome: WireOutcome) -> SessionInput {
    message(
        instance,
        PageSessionMessage::ResourceFetchFinished {
            request_id: WireRangeRequestId(request),
            outcome,
        },
    )
}

fn completed(size: u64) -> WireOutcome {
    WireOutcome::Completed {
        size_bytes: ResourceBodySize::new(size).expect("size"),
    }
}

/// Сторона движка выдаёт номер просьбе «весь ресурс №`handle`».
fn issue(client: &ResourceFetchClient, handle: u32) -> ResourceFetch {
    client
        .start(
            ResourceFetchSpec {
                handle: ExternalResourceHandle::new(handle),
                extent: ResourceExtent::Whole,
                max_bytes: NonZeroU64::new(1024).expect("non-zero"),
            },
            &CancellationToken::new(),
        )
        .expect("issued")
}

/// Тело запроса целиком (после итога страницы).
fn read_body(fetch: &mut ResourceFetch) -> Result<Vec<u8>, ResourceFetchError> {
    let mut body = Vec::new();
    while let Some(chunk) = fetch.next_chunk_blocking(&CancellationToken::new())? {
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Вид содержимого уведомлений о новых поколениях.
fn new_generation_kinds(notices: &[IntakeNotice]) -> Vec<(u32, &'static str)> {
    notices
        .iter()
        .filter_map(|notice| match notice {
            IntakeNotice::NewGeneration(handoff) => {
                Some((handoff.generation.get(), handoff.source.kind_label()))
            }
            _ => None,
        })
        .collect()
}

fn closes(notices: &[IntakeNotice]) -> Vec<SessionCloseReason> {
    notices
        .iter()
        .filter_map(|notice| match notice {
            IntakeNotice::CloseSession { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect()
}

/// Поколение, начатое как MSE, становится HLS с корнем страницы: главный цикл получает
/// сначала ждущий поток сегментов, потом HLS того же поколения.
#[test]
fn hls_source_added_turns_the_generation_into_hls_with_the_page_root() {
    let (mut translator, hls, _files) = hls_translator();
    let notices = run(&mut translator, [started(1, 1), hls_source_added(1, 1, 42)]);
    assert_eq!(
        new_generation_kinds(&notices),
        vec![(1, "segments"), (1, "hls")]
    );
    assert_eq!(hls.roots(), vec![42]);
    assert!(closes(&notices).is_empty());
}

/// Рамка «своего» номера — очереди HLS (движок получает тело); рамка номера файла
/// прежнего поколения того же сеанса — устаревшая (молча); выдуманный номер — 4003.
/// Номера у файла и у HLS сквозные, адресата выбирает `claim_of`.
#[test]
fn frames_go_to_the_hls_queue_only_for_its_own_numbers() {
    let (mut translator, hls, files) = hls_translator();
    // Поколение 1 — файл: его открытие выдаёт номер 1.
    run(&mut translator, [started(1, 1), file_source_added(1, 1)]);
    // Поколение 2 — HLS: следующий номер сеанса — 2.
    run(&mut translator, [started(1, 2), hls_source_added(1, 2, 0)]);
    let mut fetch = hls.issue_and_send(0, 0);
    assert_eq!(fetch.id().get(), 2, "numbers are shared with the file");

    let notices = run(
        &mut translator,
        [
            frame(1, 2, 0, b"#EXTM3U\n"),
            resource_finished(1, 2, completed(8)),
            // Поздняя рамка файла прежнего поколения — устарела, не нарушение.
            frame(1, 1, 0, b"late"),
        ],
    );
    assert!(closes(&notices).is_empty(), "{notices:?}");
    assert_eq!(read_body(&mut fetch), Ok(b"#EXTM3U\n".to_vec()));
    let counters = translator.counters();
    assert_eq!(counters.resources.frames_accepted, 1);
    assert_eq!(counters.resources.bytes_accepted, 8);
    assert_eq!(counters.range_frames_stale, 1);
    // Писатель файла поздней рамки не видел: файла уже нет.
    assert!(
        !files
            .calls()
            .iter()
            .any(|call| matches!(call, FileCallRecord::Bytes(1, _, _))),
        "{:?}",
        files.calls()
    );

    // Номер, которого сеанс не выдавал, — выдумка страницы.
    let notices = run(&mut translator, [frame(1, 99, 0, b"x")]);
    assert_eq!(
        closes(&notices),
        vec![SessionCloseReason::ProtocolViolation]
    );
}

/// Итог выдуманного номера — 4003; итог своего номера с чужим размером (нарушение
/// договора очереди) — тоже 4003, и движок получает нарушение, а не тело.
#[test]
fn invented_results_and_broken_contracts_close_the_session() {
    let (mut translator, _hls, _files) = hls_translator();
    run(&mut translator, [started(1, 1), hls_source_added(1, 1, 0)]);
    let notices = run(&mut translator, [resource_finished(1, 7, completed(1))]);
    assert_eq!(
        closes(&notices),
        vec![SessionCloseReason::ProtocolViolation]
    );

    let (mut translator, hls, _files) = hls_translator();
    run(&mut translator, [started(1, 1), hls_source_added(1, 1, 0)]);
    let mut fetch = hls.issue_and_send(0, 0);
    let id = fetch.id().get();
    let notices = run(
        &mut translator,
        [
            frame(1, id, 0, b"abc"),
            resource_finished(1, id, completed(5)),
        ],
    );
    assert_eq!(
        closes(&notices),
        vec![SessionCloseReason::ProtocolViolation]
    );
    assert!(matches!(
        read_body(&mut fetch),
        Err(ResourceFetchError::PageViolation(_))
    ));
}

/// Отказ страницы по номерку — итог очереди: движок получает свою ошибку, сеанс жив.
#[test]
fn a_page_failure_reaches_the_engine_and_the_session_lives() {
    let (mut translator, hls, _files) = hls_translator();
    run(&mut translator, [started(1, 1), hls_source_added(1, 1, 0)]);
    let mut fetch = hls.issue_and_send(0, 0);
    let notices = run(
        &mut translator,
        [resource_finished(
            1,
            fetch.id().get(),
            WireOutcome::Failed {
                failure: WireFailure::Refused {},
            },
        )],
    );
    assert!(closes(&notices).is_empty());
    assert_eq!(read_body(&mut fetch), Err(ResourceFetchError::Refused));
}

/// Конец сеанса и новое поколение запечатывают очередь HLS: новых запросов нет.
#[test]
fn the_end_of_the_session_and_a_new_generation_seal_the_hls_queue() {
    let (mut translator, hls, _files) = hls_translator();
    run(&mut translator, [started(1, 1), hls_source_added(1, 1, 0)]);
    run(&mut translator, [started(1, 2)]);
    assert!(matches!(
        hls.client(0).start(
            ResourceFetchSpec {
                handle: ExternalResourceHandle::new(1),
                extent: ResourceExtent::Whole,
                max_bytes: NonZeroU64::new(8).expect("non-zero"),
            },
            &CancellationToken::new()
        ),
        Err(ResourceFetchError::SourceClosed)
    ));

    run(&mut translator, [hls_source_added(1, 2, 5)]);
    let notices = run(&mut translator, [SessionInput::Ended(session(1))]);
    assert!(matches!(
        notices.last(),
        Some(IntakeNotice::InstanceSealed { .. })
    ));
    assert!(matches!(
        hls.client(1).start(
            ResourceFetchSpec {
                handle: ExternalResourceHandle::new(1),
                extent: ResourceExtent::Whole,
                max_bytes: NonZeroU64::new(8).expect("non-zero"),
            },
            &CancellationToken::new()
        ),
        Err(ResourceFetchError::SourceClosed)
    ));
}

/// Порядок поколений: MSE-вход в HLS-поколение и HLS поверх файла — нарушение (4003).
#[test]
fn hls_cannot_mix_with_media_source_or_file_input() {
    let (mut translator, _hls, _files) = hls_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            hls_source_added(1, 1, 0),
            added(1, 1, 0, "video/mp4"),
        ],
    );
    assert_eq!(
        closes(&notices),
        vec![SessionCloseReason::ProtocolViolation]
    );

    let (mut translator, hls, _files) = hls_translator();
    let notices = run(
        &mut translator,
        [
            started(1, 1),
            file_source_added(1, 1),
            hls_source_added(1, 1, 0),
        ],
    );
    assert_eq!(
        closes(&notices),
        vec![SessionCloseReason::ProtocolViolation]
    );
    assert!(hls.roots().is_empty(), "no HLS queue for a file generation");
}
