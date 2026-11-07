//! Открыватель HLS-поколения на настоящей очереди ресурсов: без возможностей плеера он
//! ждёт и ничего не просит; отказ корня — честный отказ поколения; уход до результата
//! отменяет открытие (запрос брошен).

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::{Duration, Instant};

use browser_bridge_protocol::limits::MAX_RESOURCE_FETCH_BYTES;
use browser_bridge_protocol::session::player::StreamRejection;
use fastiplayer_config::AppConfig;
use pushed_media_source::{
    ExternalResourceHandle, RangeRequestIds, RangeRequestNotifier, ResourceFetchFailure,
    ResourceFetchOutcome, ResourceFetchPolicy, ResourceFetchRequests, ResourceFetchWriter,
    resource_fetch_queue,
};

use super::super::capabilities::{
    BrowserDecodeCapabilities, DecodeCapabilitiesAnswer, DecodeCapabilitiesSource,
};
use super::super::transport::BridgeResourceTransport;
use super::{HlsGenerationOpener, HlsOpenProgress, HlsOpenSettings};
use crate::browser_session::playback::video_output::software_only_capabilities;

/// Сколько тест ждёт того, что должно случиться «скоро».
const PROMPT: Duration = Duration::from_secs(10);

struct Silent;
impl RangeRequestNotifier for Silent {
    fn range_requested(&self) {}
}

/// Плеер, который знает (или ещё не знает) свои возможности.
struct Player(DecodeCapabilitiesAnswer);

impl DecodeCapabilitiesSource for Player {
    fn decode_capabilities(&self) -> DecodeCapabilitiesAnswer {
        self.0.clone()
    }
}

fn known_player() -> Player {
    Player(DecodeCapabilitiesAnswer::Known(Box::new(
        BrowserDecodeCapabilities {
            system: software_only_capabilities(render_core::RenderCapabilities::wgpu_nv12(None)),
            audio: audio::AudioDecodeCapabilityProvider::audio_decode_capability_snapshot(
                &audio::ProductionAudioDecoderFactory::default(),
            ),
        },
    )))
}

fn opener() -> (
    HlsGenerationOpener,
    ResourceFetchWriter,
    ResourceFetchRequests,
) {
    let policy = ResourceFetchPolicy::new(
        NonZeroUsize::new(8).expect("non-zero"),
        Duration::from_secs(10),
        NonZeroU64::new(MAX_RESOURCE_FETCH_BYTES).expect("non-zero"),
    )
    .expect("policy");
    let (writer, client, requests) =
        resource_fetch_queue(policy, RangeRequestIds::new(), Arc::new(Silent));
    let opener = HlsGenerationOpener::new(
        BridgeResourceTransport::new(client),
        requests.clone(),
        ExternalResourceHandle::new(0),
        Arc::new(HlsOpenSettings::from_app_config(&AppConfig::default())),
    );
    (opener, writer, requests)
}

/// Ждёт первой просьбы открытия (корень).
fn first_request(requests: &ResourceFetchRequests) -> pushed_media_source::ResourceRequest {
    let deadline = Instant::now() + PROMPT;
    loop {
        if let Some(request) = requests.take_requests().first() {
            return *request;
        }
        assert!(
            Instant::now() < deadline,
            "the opener never asked for the root"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Окно ещё не назвало возможности: открытие ждёт, странице ничего не уходит.
#[test]
fn without_player_capabilities_the_opener_waits_and_asks_nothing() {
    let (mut opener, _writer, requests) = opener();
    let player = Player(DecodeCapabilitiesAnswer::NotYetKnown);
    for _ in 0..3 {
        assert!(matches!(
            opener.poll_open(&player),
            HlsOpenProgress::Pending
        ));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(requests.stats().issued, 0);
}

/// Корень — номерок `root`; фон отказал — поколение получает «не загрузилось».
#[test]
fn a_refused_root_ends_the_open_with_file_unavailable() {
    let (mut opener, writer, requests) = opener();
    let player = known_player();
    assert!(matches!(
        opener.poll_open(&player),
        HlsOpenProgress::Pending
    ));
    let root = first_request(&requests);
    assert_eq!(root.handle.get(), 0);
    writer
        .finish(
            root.id,
            ResourceFetchOutcome::Failed(ResourceFetchFailure::Refused),
        )
        .expect("finish");
    let deadline = Instant::now() + PROMPT;
    let rejection = loop {
        match opener.poll_open(&player) {
            HlsOpenProgress::Pending => {}
            HlsOpenProgress::Failed(rejection) => break rejection,
            HlsOpenProgress::Ready(_) => panic!("a refused root cannot open"),
        }
        assert!(Instant::now() < deadline, "the open never ended");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(rejection.reason, StreamRejection::FileUnavailable);
}

/// Поколение сменилось до результата: открыватель уничтожен — открытие отменено, а
/// его запрос брошен (страница молчит, но поток не висит).
#[test]
fn dropping_the_opener_before_its_result_cancels_the_open() {
    let (mut opener, _writer, requests) = opener();
    let player = known_player();
    assert!(matches!(
        opener.poll_open(&player),
        HlsOpenProgress::Pending
    ));
    let _root = first_request(&requests);
    drop(opener);
    let deadline = Instant::now() + PROMPT;
    while requests.stats().abandoned == 0 {
        assert!(
            Instant::now() < deadline,
            "the cancelled open never let go of its request"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
