//! Стенд 10/12 бит: плоскости из 16-битных слов LE (как `yuv4xxp10le`/`p12le` FFmpeg) и эталоны.
//!
//! Эталон SDR — независимая транскрипция 16-битного шейдера wgpu
//! (`host_yuv420_16bit_to_sdr.wgsl`: нормализация с ограничением кодов, матрица, `clamp`) в
//! `f32`. Эталон HDR — точная CPU-формула crate-а в `f64` (`gles2_video::bt2446c_to_sdr`), её
//! совпадение с эталоном плеера проверяют unit-тесты crate-а; GPU-путь с таблицей сверяется с ней
//! с документированным допуском.

use gles2_video::{
    ChromaSubsampling, ColorAdjustment, HdrReferenceLevels, HdrTransfer, HostRows,
    PlanarFrameUpload, PlanarLayout, PlaneExtent, PlaneSource, SampleDepth, VideoFrameKey,
    YuvColorEncoding, YuvMatrix, YuvRange, bt2446c_to_sdr,
};

use super::apply_sdr_adjustments;

/// Байт мусора в хвосте строки: собранный из него код 0xFFFF уводит цвет далеко.
pub const ROW_TAIL_GARBAGE: u8 = 0xFF;

/// Плоскость 16-битных отсчётов LE в памяти теста; блок кончается на видимой части последней
/// строки (как у FFmpeg), хвосты остальных строк — мусор.
#[derive(Debug, Clone)]
pub struct HighBitPlane {
    pub bytes: Vec<u8>,
    pub stride: usize,
    pub visible_row_bytes: usize,
    pub rows: u32,
}

impl HighBitPlane {
    fn from_fn(
        extent: PlaneExtent,
        row_tail_samples: usize,
        sample: impl Fn(u32, u32) -> u16,
    ) -> Self {
        let visible_row_bytes = extent.width as usize * 2;
        let stride = visible_row_bytes + row_tail_samples * 2;
        let rows = extent.height;
        let length = (rows as usize - 1) * stride + visible_row_bytes;
        let mut bytes = vec![ROW_TAIL_GARBAGE; length];
        for y in 0..rows {
            for x in 0..extent.width {
                let offset = y as usize * stride + x as usize * 2;
                bytes[offset..offset + 2].copy_from_slice(&sample(x, y).to_le_bytes());
            }
        }
        Self {
            bytes,
            stride,
            visible_row_bytes,
            rows,
        }
    }

    pub fn source(&self) -> PlaneSource<'_> {
        PlaneSource::HostRows(
            HostRows::new(&self.bytes, self.stride, self.visible_row_bytes, self.rows)
                .expect("test plane rows are consistent"),
        )
    }
}

/// Коды отсчётов кадра по роли и координатам в своей плоскости.
pub struct HighBitPattern<'a> {
    pub luma: &'a dyn Fn(u32, u32) -> u16,
    pub chroma_u: &'a dyn Fn(u32, u32) -> u16,
    pub chroma_v: &'a dyn Fn(u32, u32) -> u16,
}

/// Кадр 10/12 бит в памяти теста.
#[derive(Debug, Clone)]
pub struct HighBitFrame {
    pub chroma: ChromaSubsampling,
    pub depth: SampleDepth,
    pub luma_extent: PlaneExtent,
    pub luma: HighBitPlane,
    pub chroma_u: HighBitPlane,
    pub chroma_v: HighBitPlane,
}

impl HighBitFrame {
    /// Кадр из функций кодов; `row_tail_samples` — отсчётов мусора после видимой части строки.
    pub fn from_pattern(
        chroma: ChromaSubsampling,
        depth: SampleDepth,
        luma_extent: PlaneExtent,
        row_tail_samples: usize,
        pattern: &HighBitPattern<'_>,
    ) -> Self {
        let chroma_extent = chroma.chroma_extent(luma_extent);
        Self {
            chroma,
            depth,
            luma_extent,
            luma: HighBitPlane::from_fn(luma_extent, row_tail_samples, pattern.luma),
            chroma_u: HighBitPlane::from_fn(chroma_extent, row_tail_samples, pattern.chroma_u),
            chroma_v: HighBitPlane::from_fn(chroma_extent, row_tail_samples, pattern.chroma_v),
        }
    }

    /// Кадр одного набора кодов.
    pub fn solid(
        chroma: ChromaSubsampling,
        depth: SampleDepth,
        luma_extent: PlaneExtent,
        row_tail_samples: usize,
        codes: [u16; 3],
    ) -> Self {
        Self::from_pattern(
            chroma,
            depth,
            luma_extent,
            row_tail_samples,
            &HighBitPattern {
                luma: &|_, _| codes[0],
                chroma_u: &|_, _| codes[1],
                chroma_v: &|_, _| codes[2],
            },
        )
    }

    pub fn upload(&self, key: u64) -> PlanarFrameUpload<'_> {
        PlanarFrameUpload {
            key: VideoFrameKey(key),
            layout: PlanarLayout {
                chroma: self.chroma,
                depth: self.depth,
            },
            luma_extent: self.luma_extent,
            luma: self.luma.source(),
            chroma_u: self.chroma_u.source(),
            chroma_v: self.chroma_v.source(),
        }
    }
}

/// Нормализация кодов wgpu (`host_yuv420_range_normalization` + `normalize_host_planar_yuv_sample`).
pub fn normalize_codes(depth: SampleDepth, range: YuvRange, codes: [u16; 3]) -> [f32; 3] {
    let max_code = depth.max_code() as f32;
    let shift = depth.bits() - 8;
    let code = |value: u32| (value << shift) as f32;
    let [y, u, v] = codes.map(f32::from);
    match range {
        YuvRange::Limited => {
            let black = code(16);
            let white = code(235);
            let chroma_scale = 1.0 / (code(240) - code(16));
            let center = code(128);
            [
                (y.clamp(black, white.min(max_code)) - black) / (white - black),
                (u.clamp(0.0, max_code) - center) * chroma_scale,
                (v.clamp(0.0, max_code) - center) * chroma_scale,
            ]
        }
        YuvRange::Full => {
            let center = code(128);
            [
                y.clamp(0.0, max_code) / max_code,
                (u.clamp(0.0, max_code) - center) / max_code,
                (v.clamp(0.0, max_code) - center) / max_code,
            ]
        }
    }
}

/// Эталон SDR 10/12 бит: нормализация → матрица (full-строки) → цветокоррекция → `clamp`.
pub fn reference_high_bit_sdr(
    depth: SampleDepth,
    encoding: YuvColorEncoding,
    adjustment: ColorAdjustment,
    codes: [u16; 3],
) -> [u8; 4] {
    let normalized = normalize_codes(depth, encoding.range, codes);
    let (rows, weights): ([[f32; 3]; 3], [f32; 3]) = match encoding.matrix {
        YuvMatrix::Bt709 => (
            [
                [1.0, 0.0, 1.574_8],
                [1.0, -0.187_324_27, -0.468_124_27],
                [1.0, 1.855_6, 0.0],
            ],
            [0.212_6, 0.715_2, 0.072_2],
        ),
        YuvMatrix::Bt601 => (
            [
                [1.0, 0.0, 1.402],
                [1.0, -0.344_136_3, -0.714_136_3],
                [1.0, 1.772, 0.0],
            ],
            [0.299, 0.587, 0.114],
        ),
    };
    let rgb =
        rows.map(|row| row[0] * normalized[0] + row[1] * normalized[1] + row[2] * normalized[2]);
    let adjusted = apply_sdr_adjustments(rgb, weights, adjustment);
    let to_byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        to_byte(adjusted[0]),
        to_byte(adjusted[1]),
        to_byte(adjusted[2]),
        255,
    ]
}

/// Нелинейный R'G'B' BT.2020 из кодов (как `bt2020_yuv_to_rgb` WGSL), в `f64`.
pub fn bt2020_nonlinear_rgb(depth: SampleDepth, range: YuvRange, codes: [u16; 3]) -> [f64; 3] {
    const KR: f64 = 0.2627;
    const KB: f64 = 0.0593;
    const KG: f64 = 0.6780;
    let [y, cb, cr] = normalize_codes(depth, range, codes).map(f64::from);
    let red = y + 2.0 * (1.0 - KR) * cr;
    let blue = y + 2.0 * (1.0 - KB) * cb;
    let green = (y - KR * red - KB * blue) / KG;
    [red, green, blue]
}

/// Эталон HDR → SDR: точная формула в `f64`, выход RGBA8.
pub fn reference_hdr(
    depth: SampleDepth,
    range: YuvRange,
    transfer: HdrTransfer,
    levels: HdrReferenceLevels,
    codes: [u16; 3],
) -> [u8; 4] {
    let sdr = bt2446c_to_sdr(bt2020_nonlinear_rgb(depth, range, codes), transfer, levels);
    let to_byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [to_byte(sdr[0]), to_byte(sdr[1]), to_byte(sdr[2]), 255]
}
