//! Детерминированная характеристика раннего снимка на app/render boundary.
//! Публикация управляется временем теста; renderer, materializer и lease настоящие.
//! Это не запуск worker/swapchain и не доказательство причины старого 1397/1800.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use render_wgpu_video::{HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneDescriptor, HostPlaneRole, VideoFrameDiagnostics,
};
use video_frame_contract::VideoFrameContract;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

use super::{
    SharedVideoFrameLeaseRole, SharedVideoFrameMaterializationOutcome,
    SharedVideoFrameMaterializationRequest, materialize_shared_video_frame,
};
use crate::frame_prepare::PreparedVideoFrame;

#[path = "cadence_gpu.rs"]
mod cadence_gpu;
use cadence_gpu::CadenceGpu;

#[cfg(target_os = "linux")]
#[path = "cadence_surface_tests.rs"]
mod cadence_surface_tests;

/// Два независимых ресурса исключают aliasing allocation как объяснение повтора.
struct TwoFrames;

impl PresentFrameResourceProvider for TwoFrames {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        if matches!(handle.0, 1 | 2) {
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::ZERO,
            }
        } else {
            PresentFrameResourceProviderLookup::Missing {
                resource_pool_lock_wait: Duration::ZERO,
            }
        }
    }

    fn resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let luma = match handle.0 {
            1 => 48,
            2 => 208,
            _ => {
                return PresentFrameResourceDescriptorLookup::Missing {
                    resource_pool_lock_wait: Duration::ZERO,
                };
            }
        };
        let plane = |role, offset, stride, visible_width, visible_height| HostPlaneDescriptor {
            role,
            offset,
            stride,
            visible_width,
            visible_height,
            bytes_per_sample: 1,
        };
        PresentFrameResourceDescriptorLookup::Ready {
            descriptor: FrameResourceDescriptor::HostPlanar(
                HostPlanarFrameDescriptor::from_owned_buffer(
                    vec![luma, luma, luma, luma, 128, 128],
                    vec![
                        plane(HostPlaneRole::Luma, 0, 2, 2, 2),
                        plane(HostPlaneRole::ChromaU, 4, 1, 1, 1),
                        plane(HostPlaneRole::ChromaV, 5, 1, 1, 1),
                    ],
                ),
            ),
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn release_frame(&self, _: FrameResourceHandle) {
        panic!("release должен пройти через проверяемый lease sink");
    }
}

#[derive(Default)]
struct ReleaseProbe(Mutex<Vec<VideoFrameRelease>>);

impl VideoFrameReleaseSink for ReleaseProbe {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        self.0.lock().expect("release probe").push(release);
        VideoFrameReleaseOutcome::Accepted
    }
}

fn frame(handle: u64, pts: Duration, releases: &Arc<ReleaseProbe>) -> VideoFrameLease {
    VideoFrameLease::new(VideoFrameLeaseConfig::new(
        7,
        DecodedFrame {
            generation: 3,
            pts,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 2,
            height: 2,
            render_width: 2,
            render_height: 2,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(handle),
            diagnostics: VideoFrameDiagnostics::default(),
        },
        releases.clone(),
    ))
}

fn prepare(
    lease: VideoFrameLease,
    materializer: &HostPlanarWgpuFrameMaterializer,
) -> PreparedVideoFrame {
    // Убираем upload readiness из эксперимента: оба сравнения исследуют только Ready.
    assert!(matches!(
        materializer.try_host_planar_texture_view_lookup(lease.decoded_frame()),
        HostPlanarWgpuTextureViewLookup::Ready { .. }
    ));
    let result = materialize_shared_video_frame(
        SharedVideoFrameMaterializationRequest::new(SharedVideoFrameLeaseRole::Playback, lease),
        materializer,
    );
    match result.outcome {
        SharedVideoFrameMaterializationOutcome::Ready { materialized_frame } => {
            PreparedVideoFrame::ready(
                materialized_frame.into_renderable_present_frame(),
                "cadence_test",
            )
        }
        _ => panic!("прогретый материализатор должен вернуть Ready"),
    }
}

#[test]
fn prepared_snapshot_renders_old_pixels_after_new_frame_publication() {
    let mut gpu = CadenceGpu::new();
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        gpu.device(),
        gpu.queue(),
        PresentFrameResourceProviderHandle::new(TwoFrames),
    );
    let releases = Arc::new(ReleaseProbe::default());
    let first = frame(1, Duration::ZERO, &releases);
    let next = frame(2, Duration::from_nanos(16_666_667), &releases);
    let mut latest = first;
    let before_wait = prepare(latest.clone(), &materializer);
    let previous_pixels = gpu.draw(&before_wait);

    // Управляемая задержка acquisition пересекает deadline публикации второго кадра.
    // Здесь нет sleep или зависимости от скорости машины; это scripted source,
    // а не подмена production clock/scheduler. Он владеет только latest lease.
    let acquisition_return = Duration::from_millis(32);
    assert!(next.decoded_frame().pts <= acquisition_return);
    latest = next;
    assert!(releases.0.lock().expect("release probe").is_empty());
    let repeated_pixels = gpu.draw(&before_wait);
    let after_wait = prepare(latest.clone(), &materializer);
    let fresh_pixels = gpu.draw(&after_wait);

    assert_eq!(
        previous_pixels, repeated_pixels,
        "ранний снимок повторяет изображение"
    );
    assert_ne!(
        repeated_pixels, fresh_pixels,
        "поздний выбор должен изменить rendered pixels"
    );
    assert!(
        fresh_pixels
            .chunks_exact(4)
            .all(|pixel| pixel[0] > repeated_pixels[0] && pixel[3] == 255)
    );
    assert!(releases.0.lock().expect("release probe").is_empty());
    // draw дождался GPU completion; проверяем lifetime именно shared lease,
    // не выдавая fake sink за проверку production decoder release bridge.
    drop(before_wait);
    {
        let released = releases.0.lock().expect("release probe");
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].resource_handle(), FrameResourceHandle(1));
        assert!(released[0].submitted_to_renderer());
    }
    drop(after_wait);
    assert_eq!(releases.0.lock().expect("release probe").len(), 1);
    drop(latest);
    let released = releases.0.lock().expect("release probe");
    assert_eq!(released.len(), 2);
    assert_eq!(released[1].resource_handle(), FrameResourceHandle(2));
    assert!(released[1].submitted_to_renderer());
}
