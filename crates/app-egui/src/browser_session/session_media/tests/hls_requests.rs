//! Просьбы ресурсов HLS-поколения странице (10a1): только после `Attached` и только
//! странице 1.1; отказ — закрытие сеанса, а не тихая потеря.
//!
//! Очередь ресурсов — настоящая; открыватель native HLS не запускается (плеер-подделка
//! не знает своих возможностей), поэтому просьбы выдаёт сам тест стороной движка.

use std::num::NonZeroU64;
use std::sync::Arc;

use browser_bridge_protocol::ProtocolVersion;
use pushed_media_source::{
    ExternalResourceHandle, RangeRequestIds, RangeRequestNotifier, ResourceExtent, ResourceFetch,
    ResourceFetchClient, ResourceFetchSpec, ResourceFetchWriter, resource_fetch_queue,
};
use source_core::CancellationToken;

use super::*;
use crate::browser_session::hls_bridge::factory::bridge_resource_policy;
use crate::browser_session::hls_bridge::opener::{HlsGenerationOpener, HlsOpenSettings};
use crate::browser_session::hls_bridge::transport::BridgeResourceTransport;

struct Silent;
impl RangeRequestNotifier for Silent {
    fn range_requested(&self) {}
}

/// HLS-поколение `number`: уведомление главному циклу, сторона движка его очереди и
/// писатель (держит тест: уничтожение писателя — печать очереди).
fn hls_generation(
    session: &SessionRef,
    number: u32,
) -> (IntakeNotice, ResourceFetchClient, ResourceFetchWriter) {
    let config = fastiplayer_config::AppConfig::default();
    let policy = bridge_resource_policy(&config.network).expect("policy");
    let (writer, client, requests) =
        resource_fetch_queue(policy, RangeRequestIds::new(), Arc::new(Silent));
    let opener = HlsGenerationOpener::new(
        BridgeResourceTransport::new(client.clone()),
        requests.clone(),
        ExternalResourceHandle::new(0),
        Arc::new(HlsOpenSettings::from_app_config(&config)),
    );
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: session.clone(),
        generation: generation(number),
        source: GenerationSource::Hls { opener, requests },
    }));
    (notice, client, writer)
}

fn issue(client: &ResourceFetchClient, handle: u32) -> ResourceFetch {
    client
        .start(
            ResourceFetchSpec {
                handle: ExternalResourceHandle::new(handle),
                extent: ResourceExtent::Whole,
                max_bytes: NonZeroU64::new(4096).expect("non-zero"),
            },
            &CancellationToken::new(),
        )
        .expect("issued")
}

/// Номерки ресурсов в `fetch_resource`, ушедших странице.
fn fetched_resources(messages: &[PlayerSessionMessage]) -> Vec<u32> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::FetchResource { resource, .. } => Some(resource.0),
            _ => None,
        })
        .collect()
}

/// До `Attached` просьбы ждут в очереди (страница о них не знает); после `Attached`
/// страницы 1.1 уходят все, в форме протокола.
#[test]
fn resource_requests_wait_for_the_attach_and_then_go_to_a_1_1_page() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    let (notice, client, _writer) = hls_generation(&current, 1);
    harness.media.on_intake(vec![notice], None);
    let _root = issue(&client, 0);
    let _segment = issue(&client, 9);

    harness.media.drive(Instant::now());
    assert!(
        fetched_resources(&harness.page.take_sent()).is_empty(),
        "nothing goes out before the page is attached"
    );

    // Подключение ЧУЖОГО экземпляра версию текущего не задаёт.
    harness
        .media
        .on_session_attached(&session(2), ProtocolVersion::V1_1);
    harness.media.drive(Instant::now());
    assert!(fetched_resources(&harness.page.take_sent()).is_empty());

    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_1);
    harness.media.drive(Instant::now());
    assert_eq!(fetched_resources(&harness.page.take_sent()), vec![0, 9]);
    assert!(harness.closer.closed.borrow().is_empty());
    // Просьба уходит один раз: следующий проход ничего не повторяет.
    harness.media.drive(Instant::now());
    assert!(fetched_resources(&harness.page.take_sent()).is_empty());
}

/// Страховка за сторожем: HLS у страницы 1.0 — ни одной просьбы, сеанс закрыт 4003.
#[test]
fn an_hls_generation_of_a_1_0_page_sends_nothing_and_closes_the_session() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    harness
        .media
        .on_session_attached(&current, ProtocolVersion::V1_0);
    let (notice, client, _writer) = hls_generation(&current, 1);
    harness.media.on_intake(vec![notice], None);
    let _root = issue(&client, 0);

    harness.media.drive(Instant::now());
    harness.media.drive(Instant::now());
    assert!(fetched_resources(&harness.page.take_sent()).is_empty());
    let closed = harness.closer.closed.borrow().clone();
    let violations = closed
        .iter()
        .filter(|(_, reason)| *reason == SessionCloseReason::ProtocolViolation)
        .count();
    let farewells = harness.closer.farewells.borrow().len();
    // 4003 закрывает сеанс с прощанием (решение 132) или без — но ровно один раз.
    assert_eq!(violations + farewells, 1, "{closed:?}");
}
