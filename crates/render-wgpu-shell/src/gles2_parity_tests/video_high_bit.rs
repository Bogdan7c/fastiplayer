//! Пиксельный паритет видео 10/12 бит и HDR → SDR: wgpu ↔ GLES2 (сессия 06 плана
//! `user/gles2-renderer/`).
//!
//! Тот же стенд, что у 8 бит (`video.rs`): один кадр декодера рисуют настоящий 16-битный путь
//! wgpu (`R16Uint` + `textureLoad`, формула BT.2446-C в шейдере) и GLES2 (пары байтов в
//! `GL_LUMINANCE_ALPHA`, таблица 65³).
//! - SDR 10/12 бит — тот же допуск, что у 8 бит (±2): формулы совпадают.
//! - HDR — документированный допуск таблицы GLES2 (решение владельца сессии 06: таблица только
//!   в GLES2 ради старого железа, wgpu считает формулу): худший случай [`HDR_CHANNEL_TOLERANCE`],
//!   отличие больше 2/255 — не больше [`HDR_MAX_SHARE_OVER_TWO`] пикселей.
//!
//! Размеры кадров с уменьшением подобраны без «ничьих» nearest-выборки: когда центр пикселя
//! цели попадает ровно на границу текселей (`(y + 0,5) × источник / цель` — целое, например 300 в
//! 90 при y = 55), выбор соседнего текселя решает округление float — `floor` в шейдере у wgpu и
//! фиксированная точка текстурного блока у GLES2 — и реализации иногда расходятся на один тексель
//! (замер сессии 06: 4 строки из 30 «ничейных»). Это не отличие формулы и глазом не видно, но
//! пиксельный допуск такую строку не отличит от ошибки, поэтому сцены его не провоцируют.

use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
    VideoDisplayOrientation,
};
use gles2_video::RowLengthPreference;
use render_core::{ColorAdjustment, HdrToSdrSettings, RenderViewport};
use video_frame_contract::VideoFramePixelLayout;

use super::video::{VideoScene, render_gles2, render_wgpu};
use super::{CHANNEL_TOLERANCE, compare_with_tolerance};

/// Худшее отличие HDR-пикселя GLES2 от wgpu, шагов 1/255: 3 — таблица против формулы `f64`
/// (модуль `hdr` gles2-video), +1 — округление `f32` шейдера wgpu.
const HDR_CHANNEL_TOLERANCE: u8 = 4;
/// Доля HDR-пикселей с отличием больше 2/255, не больше.
const HDR_MAX_SHARE_OVER_TWO: f64 = 0.01;

fn base(
    name: &'static str,
    layout: VideoFramePixelLayout,
    color: VideoColorMetadata,
) -> VideoScene {
    VideoScene {
        name,
        layout,
        frame_size: [61, 35],
        row_tail: 0,
        orientation: VideoDisplayOrientation::Identity,
        color,
        adjustment: ColorAdjustment::identity(),
        hdr_to_sdr: HdrToSdrSettings::default(),
        exclusions: Vec::new(),
        gles2_row_length: RowLengthPreference::BestAvailable,
    }
}

fn sdr_709(range: ColorRange) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        ..VideoColorMetadata::sdr_bt709_limited()
    }
}

fn hdr(transfer: TransferFunction, range: ColorRange) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer,
        ..VideoColorMetadata::sdr_bt709_limited()
    }
}

fn sdr_scenes() -> Vec<VideoScene> {
    vec![
        base(
            "420 10-bit bt709 limited upscaled",
            VideoFramePixelLayout::Yuv420Planar10Le,
            sdr_709(ColorRange::Limited),
        ),
        base(
            "422 12-bit bt709 full upscaled",
            VideoFramePixelLayout::Yuv422Planar12Le,
            sdr_709(ColorRange::Full),
        ),
        VideoScene {
            orientation: VideoDisplayOrientation::Rotate90Clockwise,
            exclusions: vec![RenderViewport::new(0, 0, 40, 90)],
            ..base(
                "444 10-bit rotated 90 under a sidebar",
                VideoFramePixelLayout::Yuv444Planar10Le,
                sdr_709(ColorRange::Limited),
            )
        },
        VideoScene {
            orientation: VideoDisplayOrientation::Rotate270Clockwise,
            // 299×121, а не 300×120: без ничьих nearest (см. модуль).
            frame_size: [299, 121],
            ..base(
                "422 10-bit rotated 270 downscaled",
                VideoFramePixelLayout::Yuv422Planar10Le,
                sdr_709(ColorRange::Limited),
            )
        },
        VideoScene {
            row_tail: 7,
            gles2_row_length: RowLengthPreference::StrideWideTextures,
            ..base(
                "420 12-bit padded stride, stride-wide GLES2 textures",
                VideoFramePixelLayout::Yuv420Planar12Le,
                sdr_709(ColorRange::Limited),
            )
        },
        // Ручки цветокоррекции на 16-битном пути wgpu не действуют — и у GLES2 тоже.
        VideoScene {
            adjustment: ColorAdjustment {
                brightness: 0.04,
                contrast: 1.2,
                saturation: 1.35,
                exposure: -0.25,
                rgb_gain: [1.05, 0.95, 1.1],
                rgb_offset: [0.01, -0.02, 0.0],
            },
            ..base(
                "420 10-bit with knobs that 16-bit paths ignore",
                VideoFramePixelLayout::Yuv420Planar10Le,
                sdr_709(ColorRange::Limited),
            )
        },
    ]
}

fn hdr_scenes() -> Vec<VideoScene> {
    vec![
        base(
            "420 10-bit PQ limited upscaled",
            VideoFramePixelLayout::Yuv420Planar10Le,
            hdr(TransferFunction::Pq, ColorRange::Limited),
        ),
        base(
            "420 10-bit HLG limited upscaled",
            VideoFramePixelLayout::Yuv420Planar10Le,
            hdr(TransferFunction::Hlg, ColorRange::Limited),
        ),
        VideoScene {
            row_tail: 5,
            ..base(
                "422 12-bit PQ full, padded stride",
                VideoFramePixelLayout::Yuv422Planar12Le,
                hdr(TransferFunction::Pq, ColorRange::Full),
            )
        },
        VideoScene {
            frame_size: [297, 119],
            orientation: VideoDisplayOrientation::Rotate180,
            hdr_to_sdr: HdrToSdrSettings {
                sdr_reference_white_nits: 150.0,
                hdr_reference_peak_nits: 2_000.0,
                ..HdrToSdrSettings::default()
            },
            ..base(
                "444 10-bit HLG downscaled, white 150 / peak 2000",
                VideoFramePixelLayout::Yuv444Planar10Le,
                hdr(TransferFunction::Hlg, ColorRange::Limited),
            )
        },
    ]
}

/// Кадр 10/12 бит одинаково выглядит через wgpu и GLES2: SDR — в допуске 8 бит, HDR — в
/// документированном допуске таблицы.
#[test]
fn high_bit_and_hdr_video_frames_match_wgpu() {
    let size = [160, 90];
    let scenes = sdr_scenes()
        .into_iter()
        .map(|scene| (scene, CHANNEL_TOLERANCE))
        .chain(
            hdr_scenes()
                .into_iter()
                .map(|scene| (scene, HDR_CHANNEL_TOLERANCE)),
        );
    for (scene, tolerance) in scenes {
        let Some(wgpu_pixels) = render_wgpu(&scene, size) else {
            eprintln!("skip high-bit video parity: no Vulkan adapter for wgpu");
            return;
        };
        let Some(gles2_pixels) = render_gles2(&scene, size) else {
            return;
        };
        let report = compare_with_tolerance(&wgpu_pixels, &gles2_pixels, size[0], tolerance);
        let over_two = compare_with_tolerance(&wgpu_pixels, &gles2_pixels, size[0], 2);
        let pixel_count = (size[0] * size[1]) as usize;
        eprintln!(
            "high-bit video parity {}: max Δ {}, differing {} / {pixel_count}, over ±2 {}, \
             colors {}",
            scene.name,
            report.max_channel_difference,
            report.pixels_differing,
            over_two.pixels_over_tolerance,
            report.distinct_colors
        );
        assert!(
            report.distinct_colors > 200,
            "{}: scene must exercise many colours, got {}",
            scene.name,
            report.distinct_colors
        );
        assert_eq!(
            report.pixels_over_tolerance, 0,
            "{}: {} pixels over ±{tolerance}, first (x, y, wgpu, gles2) = {:?}",
            scene.name, report.pixels_over_tolerance, report.first_over_tolerance
        );
        let share_over_two = over_two.pixels_over_tolerance as f64 / pixel_count as f64;
        assert!(
            share_over_two <= HDR_MAX_SHARE_OVER_TWO,
            "{}: {:.2} % of pixels differ by more than 2/255",
            scene.name,
            share_over_two * 100.0
        );
    }
}
