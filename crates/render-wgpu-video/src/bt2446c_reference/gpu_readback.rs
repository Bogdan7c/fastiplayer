//! GPU-readback 16-битного host-planar пути wgpu против CPU-эталона (gles2-renderer сессия 06).
//!
//! Разведка сессии 06 нашла пробел: шейдер `host_yuv420_16bit_to_sdr.wgsl` проверялся только
//! разбором текста и CPU-зеркалом uniform-ов, но ни разу не рисовал на GPU. Здесь настоящий
//! `HostPlanarWgpuFrameMaterializer` загружает кадр 10/12 бит в `R16Uint`, `WgpuVideoRenderer`
//! рисует его в `Rgba8Unorm`, readback сверяется:
//! - HDR (PQ/HLG, 10 бит) — с [`super::convert_p010_sample`] (те же коды, что у P010);
//! - SDR BT.709 10/12 бит — с формулой нормализации и матрицей в `f64`.
//!
//! Без Vulkan-адаптера тест пропускается с причиной в stderr (как остальные GPU-тесты crate-а).

use std::sync::mpsc;
use std::time::Duration;

use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
    VideoDisplayOrientation,
};
use render_core::{HdrToSdrSettings, RenderViewport};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneDescriptor, HostPlaneRole, VideoFrameDiagnostics,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

use super::{Bt2446CReferenceInput, Bt2446CReferenceSettings, P010CodeValues, convert_p010_sample};
use crate::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, WgpuRenderableFrame,
    WgpuVideoRenderInput, WgpuVideoRenderer,
};

/// Сторона кадра и цели: 4:2:0 без растяжения, каждый пиксель — один отсчёт.
const SIDE: u32 = 8;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Шейдер считает в `f32`, эталон — в `f64`; разница — округление до unorm8.
const TOLERANCE: u8 = 1;
/// Сколько ждать GPU и отображения буфера чтения.
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

fn try_create_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Провайдер одного кадра стенда.
struct SingleFrameProvider(HostPlanarFrameDescriptor);

impl PresentFrameResourceProvider for SingleFrameProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        _handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        PresentFrameResourceDescriptorLookup::Ready {
            descriptor: FrameResourceDescriptor::HostPlanar(self.0.clone()),
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}

/// Однотонный кадр 4:2:0 `SIDE × SIDE`: коды `codes` 16-битными словами LE.
fn solid_frame(
    layout: VideoFramePixelLayout,
    codes: [u16; 3],
    color: VideoColorMetadata,
) -> (DecodedFrame, HostPlanarFrameDescriptor) {
    let mut bytes = Vec::new();
    let mut planes = Vec::new();
    for (role, side, code) in [
        (HostPlaneRole::Luma, SIDE, codes[0]),
        (HostPlaneRole::ChromaU, SIDE / 2, codes[1]),
        (HostPlaneRole::ChromaV, SIDE / 2, codes[2]),
    ] {
        let offset = bytes.len();
        for _ in 0..side * side {
            bytes.extend_from_slice(&code.to_le_bytes());
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride: side as usize * 2,
            visible_width: side,
            visible_height: side,
            bytes_per_sample: 2,
        });
    }
    let frame = DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract {
            pixel_layout: layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        width: SIDE,
        height: SIDE,
        render_width: SIDE,
        render_height: SIDE,
        display_orientation: VideoDisplayOrientation::Identity,
        color,
        resource_handle: FrameResourceHandle(1),
        diagnostics: VideoFrameDiagnostics::default(),
    };
    (
        frame,
        HostPlanarFrameDescriptor::from_owned_buffer(bytes, planes),
    )
}

/// Нарисовать кадр настоящим 16-битным путём и прочитать центральный пиксель.
fn render_center_pixel(
    gpu: &(wgpu::Device, wgpu::Queue),
    frame: &DecodedFrame,
    descriptor: HostPlanarFrameDescriptor,
    hdr_to_sdr: HdrToSdrSettings,
) -> [u8; 4] {
    let (device, queue) = gpu;
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        device,
        queue,
        PresentFrameResourceProviderHandle::new(SingleFrameProvider(descriptor)),
    );
    let HostPlanarWgpuTextureViewLookup::Ready { views, .. } =
        materializer.try_host_planar_texture_view_lookup(frame)
    else {
        panic!("materializer uploads the 16-bit frame");
    };
    let renderable =
        WgpuRenderableFrame::from_decoded_host_planar(frame, views.plane_views()).expect("frame");
    let mut renderer = WgpuVideoRenderer::new(device, TARGET_FORMAT);
    renderer.resize(SIDE, SIDE);
    renderer.set_hdr_to_sdr_settings(hdr_to_sdr);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("16-bit host-planar readback target"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let outcome = renderer
        .render_or_clear(WgpuVideoRenderInput {
            frame: Some(&renderable),
            video_viewport: RenderViewport::full_surface(SIDE, SIDE),
            video_exclusion_rects: &[],
            target: &target,
            encoder: &mut encoder,
            device,
            queue,
        })
        .expect("16-bit frame renders");
    assert!(outcome.frame_drawn, "frame drawn");
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("16-bit host-planar readback"),
        size: u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT * SIDE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                rows_per_image: Some(SIDE),
            },
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
    );
    // Копия загрузки — первой в submit кадра, как у шелла.
    let upload = outcome
        .host_upload
        .map(|commands| commands.into_command_buffer());
    let submission = queue.submit(upload.into_iter().chain([encoder.finish()]));
    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _sent = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_WAIT_TIMEOUT),
        })
        .expect("GPU finishes the frame");
    receiver
        .recv_timeout(GPU_WAIT_TIMEOUT)
        .expect("map callback")
        .expect("readback maps");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let row = (SIDE / 2) as usize * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let offset = row + (SIDE / 2) as usize * 4;
    let mut pixel = [0; 4];
    pixel.copy_from_slice(&mapped[offset..offset + 4]);
    pixel
}

fn hdr_color(transfer: TransferFunction, range: ColorRange) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer,
        ..VideoColorMetadata::sdr_bt709_limited()
    }
}

fn assert_pixel(actual: [u8; 4], expected: [u8; 3], what: &str) {
    for channel in 0..3 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= TOLERANCE,
            "{what}: GPU {actual:?} vs reference {expected:?}"
        );
    }
    assert_eq!(actual[3], 255, "{what}: opaque");
}

fn to_byte(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[test]
fn hdr_10bit_host_planar_frames_match_cpu_reference_on_gpu() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("skip 16-bit host-planar GPU readback: no wgpu adapter");
        return;
    };
    let settings = Bt2446CReferenceSettings::default_reference();
    for (transfer, range) in [
        (TransferFunction::Pq, ColorRange::Limited),
        (TransferFunction::Hlg, ColorRange::Limited),
        (TransferFunction::Pq, ColorRange::Full),
    ] {
        // Нейтраль, тёмная нейтраль, насыщенный красный, смешанный цвет.
        for codes in [
            [575_u16, 512, 512],
            [200, 512, 512],
            [300, 400, 900],
            [700, 300, 600],
        ] {
            let color = hdr_color(transfer, range);
            let reference = convert_p010_sample(&Bt2446CReferenceInput {
                sample: P010CodeValues {
                    y: f64::from(codes[0]),
                    cb: f64::from(codes[1]),
                    cr: f64::from(codes[2]),
                },
                color: color.clone(),
                settings,
            })
            .expect("valid reference input");
            let expected = [
                to_byte(reference.encoded_sdr_rgb.red),
                to_byte(reference.encoded_sdr_rgb.green),
                to_byte(reference.encoded_sdr_rgb.blue),
            ];
            let (frame, descriptor) =
                solid_frame(VideoFramePixelLayout::Yuv420Planar10Le, codes, color);
            let actual = render_center_pixel(&gpu, &frame, descriptor, settings.hdr_to_sdr);
            assert_pixel(
                actual,
                expected,
                &format!("{transfer:?} {range:?} {codes:?}"),
            );
        }
    }
}

#[test]
fn sdr_10_and_12bit_host_planar_frames_match_bt709_formula_on_gpu() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("skip 16-bit host-planar GPU readback: no wgpu adapter");
        return;
    };
    for (layout, bits) in [
        (VideoFramePixelLayout::Yuv420Planar10Le, 10_u32),
        (VideoFramePixelLayout::Yuv420Planar12Le, 12),
    ] {
        let shift = bits - 8;
        let max = f64::from((1_u32 << bits) - 1);
        for range in [ColorRange::Limited, ColorRange::Full] {
            for codes in [
                [16_u16 << shift, 128 << shift, 128 << shift],
                [700 << (shift - 2), 300 << (shift - 2), 900 << (shift - 2)],
            ] {
                let [y, u, v] = codes.map(f64::from);
                let shifted = |code: f64| code * f64::from(1_u32 << shift);
                let (luma, cb, cr) = match range {
                    ColorRange::Full => (
                        y / max,
                        (u - shifted(128.0)) / max,
                        (v - shifted(128.0)) / max,
                    ),
                    _ => (
                        (y.clamp(shifted(16.0), shifted(235.0)) - shifted(16.0)) / shifted(219.0),
                        (u - shifted(128.0)) / shifted(224.0),
                        (v - shifted(128.0)) / shifted(224.0),
                    ),
                };
                let expected = [
                    to_byte(luma + 1.574_8 * cr),
                    to_byte(luma - 0.187_324_27 * cb - 0.468_124_27 * cr),
                    to_byte(luma + 1.855_6 * cb),
                ];
                let color = VideoColorMetadata {
                    range,
                    ..VideoColorMetadata::sdr_bt709_limited()
                };
                let (frame, descriptor) = solid_frame(layout, codes, color);
                let actual =
                    render_center_pixel(&gpu, &frame, descriptor, HdrToSdrSettings::default());
                assert_pixel(actual, expected, &format!("{layout:?} {range:?} {codes:?}"));
            }
        }
    }
}
