//! Внешний маршрут native HLS: честная ошибка ресурса VOD, live через мост и
//! обновление корня live через тот же транспорт.
//!
//! Главный контраст: у обычного плеера (`PublicHttp`) 401/403/404/410 на сегменте VOD
//! переводит демуксер в ожидание восстановления адреса, которое ведёт приложение
//! (это поведение закрепляют прежние тесты восстановления app-egui и unit-тесты
//! `network_route`). У моста ведущего нет, поэтому та же ошибка обязана дойти до
//! плеера ошибкой демуксера за ограниченное время — иначе зритель видит вечный спиннер.

use std::thread;
use std::time::{Duration, Instant};

use fastiplayer_config::AppConfig;
use media_core::{DemuxReadEvent, MediaTime};
use source_core::CancellationToken;
use web_media_adaptive::{
    AdaptiveResourceFetchRequest, AdaptiveResourcePurpose, AdaptiveResourceQueryApplication,
};
use web_media_hls::{
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReason,
    HlsEndpointRefreshRequest,
};
use web_media_transport_api::SourceGeneration;

use super::external_route_test_transport::{
    InMemoryHlsTransport, InMemoryResource, TransportOperation, bridge_address, bridge_source,
    decode_base64, test_parent_identity,
};
use super::external_route_vertical_tests::prepare_through;
use super::live_refresh::NativeHlsEndpointRefreshPort;
use super::{
    ExternalHlsTransport, NativeHlsNetworkRoute, PreparedNativeHlsLifecycle,
    PreparedNativeHlsMedia, VodEndpointRecoveryMode,
};

/// H.264 64×64 + AC-3 стерео в одном MPEG-TS сегменте 0.5 с.
const H264_AC3_STEREO_SEGMENT: &[u8] =
    include_bytes!("../../../../mpeg-ts-demux/tests/fixtures/ac3/h264-ac3-stereo-48k.ts");
/// Два последовательных 1-секундных MPEG-TS сегмента H.264 + AAC (фикстуры N07 app-egui).
const LIVE_SEGMENTS_BASE64: [&str; 2] = [
    include_str!("../../../../app-egui/src/media_open/web/tests/fixtures/ts-0.ts.base64"),
    include_str!("../../../../app-egui/src/media_open/web/tests/fixtures/ts-1.ts.base64"),
];
/// За это время ошибка ресурса обязана дойти до плеера (повторы context-а — доли секунды).
const FAILURE_DEADLINE: Duration = Duration::from_secs(10);
/// Ожидание сдвига live-окна (обновление плейлиста раз в target duration = 1 с).
const LIVE_DEADLINE: Duration = Duration::from_secs(10);
/// Пауза между опросами демуксера, который временно не готов.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// VOD из двух сегментов: первый играет, второй — по заданному адресу.
///
/// Второй сегмент стоит после `EXT-X-DISCONTINUITY`: проба контейнера при открытии
/// читает только первую эпоху, поэтому сбой второго сегмента случается уже во время
/// воспроизведения (как истёкший адрес посреди просмотра), а не при подготовке.
fn two_segment_vod(first_segment: &str, second_segment: &str) -> Vec<u8> {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:0.5,\n{first_segment}\n#EXT-X-DISCONTINUITY\n\
         #EXTINF:0.5,\n{second_segment}\n#EXT-X-ENDLIST\n"
    )
    .into_bytes()
}

/// Чем закончилось чтение демуксера до первого не-пакетного исхода.
enum DemuxOutcome {
    /// Демуксер вернул ошибку (текст цепочки — для сообщения теста).
    Failed(String),
    /// Демуксер честно дочитал поток (здесь это означает потерю ошибки).
    Ended,
    /// Дедлайн истёк, а демуксер всё отвечал «временно недоступно».
    StillUnavailable,
}

/// Читает демуксер, пока не случится ошибка, конец или дедлайн.
fn read_until_failure(prepared: &mut PreparedNativeHlsMedia, deadline: Duration) -> DemuxOutcome {
    let deadline = Instant::now() + deadline;
    while Instant::now() < deadline {
        match prepared.demuxer.next_event() {
            Err(error) => return DemuxOutcome::Failed(format!("{error:#}")),
            Ok(DemuxReadEvent::EndOfStream) => return DemuxOutcome::Ended,
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                thread::sleep(READINESS_POLL_INTERVAL);
            }
            Ok(
                DemuxReadEvent::Packet(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_),
            ) => {}
        }
    }
    DemuxOutcome::StillUnavailable
}

#[test]
fn bridge_segment_403_fails_the_demuxer_within_deadline_instead_of_waiting_forever() {
    let transport = InMemoryHlsTransport::with_resources([
        (
            40,
            InMemoryResource::Body(two_segment_vod(&bridge_address(41), &bridge_address(42))),
        ),
        (41, InMemoryResource::Body(H264_AC3_STEREO_SEGMENT.to_vec())),
        (42, InMemoryResource::Status(403)),
    ]);
    let mut prepared = prepare_through(&transport, &bridge_source(40), None);
    assert!(matches!(
        prepared.lifecycle,
        PreparedNativeHlsLifecycle::Vod {
            endpoint_recovery: VodEndpointRecoveryMode::NotAvailable,
            ..
        }
    ));

    let started = Instant::now();
    match read_until_failure(&mut prepared, FAILURE_DEADLINE) {
        DemuxOutcome::Failed(error) => {
            assert!(
                started.elapsed() < FAILURE_DEADLINE,
                "the 403 must surface as a demux error in bounded time: {error}"
            );
        }
        DemuxOutcome::Ended => panic!("a 403 segment must not be swallowed as end of stream"),
        DemuxOutcome::StillUnavailable => {
            panic!("bridge VOD waited for an endpoint recovery nobody drives (eternal spinner)")
        }
    }
    assert!(
        transport.requests_for(42) >= 1,
        "the failing segment was requested through the bridge"
    );
}

/// Номер запроса корня (с единицы), который впервые получает сдвинутое окно.
const SHIFTED_SNAPSHOT_REQUEST: usize = 4;

/// Номерок live-сегмента с данным media sequence.
fn live_segment_handle(sequence: usize) -> u32 {
    51 + u32::try_from(sequence).expect("small media sequence")
}

/// Снимок скользящего live-окна из двух 1-секундных сегментов, начиная с `first_sequence`.
fn sliding_snapshot(first_sequence: usize) -> Vec<u8> {
    let entries = (first_sequence..first_sequence + 2)
        .map(|sequence| {
            format!(
                "#EXTINF:1,\n{}\n",
                bridge_address(live_segment_handle(sequence))
            )
        })
        .collect::<String>();
    format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first_sequence}\n{entries}")
        .into_bytes()
}

/// Начало доступного live-окна по timeline port-у подготовленного live.
fn availability_start(prepared: &PreparedNativeHlsMedia) -> Option<MediaTime> {
    let PreparedNativeHlsLifecycle::Live { timeline_port } = &prepared.lifecycle else {
        panic!("a sliding playlist must open as live");
    };
    timeline_port
        .observe()
        .snapshot
        .state
        .as_live()
        .expect("live view")
        .availability_range()
        .map(|range| range.start)
}

/// Читает live-демуксер, пока начало окна не удовлетворит условию; возвращает это начало.
fn pump_live_until(
    prepared: &mut PreparedNativeHlsMedia,
    deadline: Instant,
    accept: impl Fn(MediaTime) -> bool,
) -> MediaTime {
    loop {
        if let Some(start) = availability_start(prepared).filter(|start| accept(*start)) {
            return start;
        }
        assert!(
            Instant::now() < deadline,
            "live window did not reach the expected state"
        );
        match prepared.demuxer.next_event() {
            Err(error) => panic!("bridge live demux failed: {error:#}"),
            Ok(DemuxReadEvent::EndOfStream) => panic!("live ended before the window moved"),
            Ok(_) => thread::sleep(READINESS_POLL_INTERVAL),
        }
    }
}

#[test]
fn bridge_sliding_live_window_moves_with_refreshes_through_the_bridge() {
    // Первые три запроса корня видят исходное окно, четвёртый — сдвинутое.
    let first_segment = decode_base64(LIVE_SEGMENTS_BASE64[0]);
    let second_segment = decode_base64(LIVE_SEGMENTS_BASE64[1]);
    let initial = sliding_snapshot(0);
    let transport = InMemoryHlsTransport::with_resources([
        (
            50,
            InMemoryResource::Snapshots(vec![
                initial.clone(),
                initial.clone(),
                initial,
                sliding_snapshot(1),
            ]),
        ),
        (
            live_segment_handle(0),
            InMemoryResource::Body(first_segment),
        ),
        (
            live_segment_handle(1),
            InMemoryResource::Body(second_segment.clone()),
        ),
        // Как в app-вертикали N08: третий сегмент повторяет второй, окну важна шкала плейлиста.
        (
            live_segment_handle(2),
            InMemoryResource::Body(second_segment),
        ),
    ]);
    let mut prepared = prepare_through(&transport, &bridge_source(50), None);
    let deadline = Instant::now() + LIVE_DEADLINE;
    // Окно публикуется, когда потребитель начал читать, — как у плеера.
    let initial_start = pump_live_until(&mut prepared, deadline, |_| true);

    let moved_start = pump_live_until(&mut prepared, deadline, |start| start > initial_start);
    assert!(moved_start > initial_start);
    // Сдвинутое окно есть только в четвёртом снимке транспорта: значит, корень
    // обновлялся (минимум три раза после открытия) именно через мост.
    assert!(
        transport.requests_for(50) >= SHIFTED_SNAPSHOT_REQUEST,
        "playlist refreshes go through the bridge transport"
    );
    // И плейлисты (буферный вход), и сегменты (потоковый вход) шли через одну розетку.
    let operations = transport.operations();
    assert!(operations.contains(&TransportOperation::BufferedBlocking));
    assert!(operations.contains(&TransportOperation::StreamingOpen));
    assert!(transport.every_request_used_bridge_addresses());
}

/// Порт обновления корня live над внешним маршрутом заданного транспорта.
fn bridge_refresh_port(
    transport: &std::sync::Arc<InMemoryHlsTransport>,
    root_handle: u32,
    cancellation: CancellationToken,
) -> NativeHlsEndpointRefreshPort {
    let source = bridge_source(root_handle);
    NativeHlsEndpointRefreshPort::new(
        test_parent_identity(&source),
        source,
        AppConfig::default().network,
        NativeHlsNetworkRoute::External(ExternalHlsTransport::new(
            std::sync::Arc::clone(transport) as _,
        )),
        cancellation,
    )
}

fn expiry_refresh_request() -> HlsEndpointRefreshRequest {
    HlsEndpointRefreshRequest {
        previous_generation: SourceGeneration::new(1),
        reason: HlsEndpointRefreshReason::ResourceExpired,
    }
}

#[test]
fn live_endpoint_refresh_refetches_root_and_keeps_the_bridge_for_later_segments() {
    let transport = InMemoryHlsTransport::with_resources([
        (60, InMemoryResource::Body(sliding_snapshot(0))),
        (61, InMemoryResource::Body(H264_AC3_STEREO_SEGMENT.to_vec())),
    ]);
    let port = bridge_refresh_port(&transport, 60, CancellationToken::new());

    let reply = port
        .refresh(expiry_refresh_request())
        .expect("bridge root refresh");
    assert_eq!(reply.generation, SourceGeneration::new(2));
    assert_eq!(
        transport.requests_for(60),
        1,
        "root refetched by its handle"
    );

    // Новый context, на котором live продолжит качать сегменты, тоже идёт через мост.
    let segment = reply
        .http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            reply.generation,
            source_core::HttpRequestTarget::parse_exact(bridge_address(61)).expect("segment"),
            std::num::NonZeroUsize::new(1024 * 1024).expect("segment bound"),
            AdaptiveResourcePurpose::MediaSegment,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        ))
        .expect("segment through refreshed context");
    assert_eq!(segment.bytes(), H264_AC3_STEREO_SEGMENT);
    assert_eq!(transport.requests_for(61), 1);
    assert!(transport.every_request_used_bridge_addresses());
}

#[test]
fn cancelled_live_endpoint_refresh_never_touches_the_bridge() {
    let transport =
        InMemoryHlsTransport::with_resources([(70, InMemoryResource::Body(sliding_snapshot(0)))]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let port = bridge_refresh_port(&transport, 70, cancellation);

    let error = port
        .refresh(expiry_refresh_request())
        .err()
        .expect("cancelled refresh fails");
    assert_eq!(error, HlsEndpointRefreshError::Cancelled);
    assert_eq!(transport.total_requests(), 0);
}

#[test]
fn failing_bridge_root_refresh_is_bounded_and_reported_as_exhausted() {
    // 503 повторяемо: context делает свои повторы и сдаётся, транспорт сам не повторяет.
    let transport = InMemoryHlsTransport::with_resources([(80, InMemoryResource::Status(503))]);
    let port = bridge_refresh_port(&transport, 80, CancellationToken::new());

    let error = port
        .refresh(expiry_refresh_request())
        .err()
        .expect("unavailable root refresh fails");
    assert_eq!(error, HlsEndpointRefreshError::AttemptsExhausted);
    assert_eq!(
        transport.requests_for(80),
        3,
        "exactly the native HLS retry budget of attempts reaches the bridge"
    );
}
