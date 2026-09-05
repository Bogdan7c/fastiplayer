//! Offscreen consumer соблюдает неблокирующий descriptor contract декодера.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use codec_core::VideoCodec;
use media_core::TrackKind;
use render_wgpu_video::{HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup};
use source_core::CancellationToken;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{DecodedFrame, FrameResourceHandle};

use super::super::{FixtureOriginResponse, RangeFixtureOrigin};
use super::{MUXED_WEBM_BASE64, OffscreenWgpuHarness, decode_first_frame, open_decoder};

/// Общий bounded срок ожидания готовности одного кадра в acceptance harness.
const MATERIALIZATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Ждёт только штатный Busy; missing/unsupported/fatal сохраняют исходную семантику.
///
/// Decoder thread может кратко удерживать pool mutex после публикации кадра.
/// Production materializer намеренно не блокируется: повторная попытка принадлежит
/// consumer-у. В offscreen harness предыдущий GPU submit уже ожидается отдельно;
/// здесь yield даёт владельцу CPU-пула закончить работу без произвольного sleep.
/// Отдельный tests-модуль сохраняет квалифицированную геометрию исходного harness.
pub(super) fn wait_for_host_planar_texture_views(
    materializer: &HostPlanarWgpuFrameMaterializer,
    frame: &DecodedFrame,
) -> HostPlanarWgpuTextureViewLookup {
    let deadline = Instant::now() + MATERIALIZATION_TIMEOUT;
    loop {
        let lookup = materializer.try_host_planar_texture_view_lookup(frame);
        if !matches!(lookup, HostPlanarWgpuTextureViewLookup::Busy { .. }) {
            return lookup;
        }
        assert!(
            Instant::now() < deadline,
            "HostPlanar materialization deadline exceeded"
        );
        thread::yield_now();
    }
}

/// Воспроизводит один nonblocking Busy, затем использует настоящий FFmpeg provider.
struct InitiallyBusyProvider {
    inner: PresentFrameResourceProviderHandle,
    attempts: Arc<AtomicUsize>,
}

impl PresentFrameResourceProvider for InitiallyBusyProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        self.inner.resource_lookup(handle)
    }

    fn resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.inner.resource_descriptor_lookup(handle)
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait: Duration::ZERO,
            };
        }
        self.inner.try_resource_descriptor_lookup(handle)
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.inner.release_frame(handle);
    }
}

#[cfg(feature = "ffmpeg")]
#[test]
fn initial_descriptor_backpressure_still_reaches_draw_readback_and_release() {
    let webm = base64::engine::general_purpose::STANDARD
        .decode(MUXED_WEBM_BASE64)
        .expect("decode muxed VP9 fixture");
    let origin = RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::Ogg(webm));
    let locator = origin.media_url_with_extension("webm");
    let classified = crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("classify direct WebM");
    let config = fastiplayer_config::AppConfig::default();
    let opened = crate::direct_progressive_open::open_direct_media(
        &classified,
        &config.network,
        &config.player.demux,
        CancellationToken::new(),
    )
    .expect("open direct WebM");
    let (mut demuxer, _recovery) = opened.into_runtime_parts();
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .cloned()
        .expect("video track");
    let mut harness = OffscreenWgpuHarness::new();
    let (decoder, provider) = open_decoder(&track, harness.queue(), VideoCodec::Vp9);
    let attempts = Arc::new(AtomicUsize::new(0));
    let provider = PresentFrameResourceProviderHandle::new(InitiallyBusyProvider {
        inner: provider,
        attempts: Arc::clone(&attempts),
    });
    let materializer =
        HostPlanarWgpuFrameMaterializer::new(harness.device(), harness.queue(), provider.clone());
    let frame = decode_first_frame(demuxer.as_mut(), decoder.as_ref());
    assert!(harness.submit_and_release(&materializer, &provider, frame));
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "Busy must precede real materialization"
    );
}
