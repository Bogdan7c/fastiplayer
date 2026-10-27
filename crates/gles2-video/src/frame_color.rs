//! Какой программой и с какими цветовыми uniform-ами рисовать кадр (чистая логика, без GL).
//!
//! Решение зависит от двух вещей: разрядности загруженного кадра (её знает видеослой) и перевода
//! цвета из запроса рисования (его выбирает вызывающий). Несовместимая пара — типизированный
//! отказ, а не тихая подмена: HDR → SDR у 8-битного кадра нет.

use crate::color::ColorTransform;
use crate::color::high_bit::CodeNormalization;
use crate::gl_calls::PlaneTextureFormat;
use crate::hdr::lut::{self, HdrLutKey};
use crate::hdr::{HdrReferenceLevels, HdrTransfer};
use crate::layer_api::{VideoColorConversion, VideoDrawError};
use crate::planes::PlanarLayout;
use crate::program::{BytePairChannels, ProgramVariant};

/// Цветовые uniform-ы варианта программы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ColorUniformValues {
    /// 8 бит: вся цепочка одной матрицей.
    EightBit(ColorTransform),
    /// 10/12 бит: нормализация кодов, матрица и (у HDR) таблица.
    HighBit {
        normalization: CodeNormalization,
        matrix: ColorTransform,
        hdr_table: Option<HdrTableUniformValues>,
    },
}

/// Uniform-ы выборки таблицы HDR → SDR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HdrTableUniformValues {
    /// Узлов на ось − 1, срезов в ряду, узлов на ось.
    pub(crate) layout: [f32; 3],
    /// `1 / размер атласа`.
    pub(crate) texel_size: [f32; 2],
    /// Значение узла = байт / 255 × `[0]` + `[1]`.
    pub(crate) decode: [f32; 2],
}

impl HdrTableUniformValues {
    fn for_atlas() -> Self {
        // Размеры атласа — сотни текселей: в f32 точны.
        let nodes = lut::NODES_PER_AXIS as f32;
        Self {
            layout: [nodes - 1.0, lut::ATLAS_COLUMNS as f32, nodes],
            texel_size: [
                1.0 / lut::atlas_width() as f32,
                1.0 / lut::atlas_height() as f32,
            ],
            decode: lut::decode_scale_offset(),
        }
    }
}

/// Какая таблица HDR нужна кадру.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RequiredHdrTable {
    pub(crate) key: HdrLutKey,
    pub(crate) transfer: HdrTransfer,
    pub(crate) levels: HdrReferenceLevels,
}

/// Итог выбора: программа, её цветовые uniform-ы и таблица, которая должна быть загружена.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameColorPlan {
    pub(crate) variant: ProgramVariant,
    pub(crate) uniforms: ColorUniformValues,
    pub(crate) hdr_table: Option<RequiredHdrTable>,
}

/// Выбрать программу и цвет для кадра раскладки `layout`, чьи плоскости лежат в текстурах
/// формата `texture_format` (у 10/12 бит формат решает, в каких каналах пара байтов).
///
/// # Errors
/// [`VideoDrawError::HdrToSdrNeedsHighBitFrame`] — HDR → SDR для 8-битного кадра;
/// [`VideoDrawError::HighBitFrameWithoutBytePairTextures`] — 10/12 бит в 8-битных текстурах
/// (нарушение инварианта загрузки или импорта).
pub(crate) fn plan_frame_color(
    layout: PlanarLayout,
    texture_format: PlaneTextureFormat,
    conversion: VideoColorConversion,
) -> Result<FrameColorPlan, VideoDrawError> {
    let byte_pairs = || {
        BytePairChannels::of(texture_format).ok_or(
            VideoDrawError::HighBitFrameWithoutBytePairTextures {
                depth: layout.depth,
                format: texture_format,
            },
        )
    };
    match (layout.depth.is_high_bit(), conversion) {
        (
            false,
            VideoColorConversion::Sdr {
                encoding,
                adjustment,
            },
        ) => Ok(FrameColorPlan {
            variant: ProgramVariant::EightBit(layout.chroma),
            uniforms: ColorUniformValues::EightBit(ColorTransform::new(encoding, adjustment)),
            hdr_table: None,
        }),
        (false, VideoColorConversion::HdrToSdr(_)) => {
            Err(VideoDrawError::HdrToSdrNeedsHighBitFrame {
                depth: layout.depth,
            })
        }
        (
            true,
            VideoColorConversion::Sdr {
                encoding,
                adjustment,
            },
        ) => Ok(FrameColorPlan {
            variant: ProgramVariant::HighBitSdr(byte_pairs()?),
            uniforms: ColorUniformValues::HighBit {
                normalization: CodeNormalization::new(layout.depth, encoding.range),
                matrix: ColorTransform::for_normalized_sdr(encoding, adjustment),
                hdr_table: None,
            },
            hdr_table: None,
        }),
        (true, VideoColorConversion::HdrToSdr(hdr)) => Ok(FrameColorPlan {
            variant: ProgramVariant::HighBitHdrTable(byte_pairs()?),
            uniforms: ColorUniformValues::HighBit {
                normalization: CodeNormalization::new(layout.depth, hdr.range),
                matrix: ColorTransform::bt2020_nonlinear_rgb(),
                hdr_table: Some(HdrTableUniformValues::for_atlas()),
            },
            hdr_table: Some(RequiredHdrTable {
                key: HdrLutKey::new(hdr.transfer, hdr.levels),
                transfer: hdr.transfer,
                levels: hdr.levels,
            }),
        }),
    }
}

/// Цвет кадра NV12 из DMA-BUF: одна матрица SDR-цепочки, программа одна.
///
/// Формула та же, что у `nv12_to_rgba.wgsl` плеера: нормализация диапазона
/// (`yuv_range`/`chroma_scale` — Y: `(Y − 0,0625) × 1,1643836` у limited; U/V: `− 0,5`, множитель
/// цветности 1), матрица, цветокоррекция, `clamp`. Те же числа считает [`ColorTransform::new`] для
/// планарных 8 бит (их wgpu-шейдер `host_yuv420_8bit_to_rgba.wgsl` читает из тех же uniform-ов
/// `color_pipeline.rs`), поэтому отдельной NV12-формулы нет.
///
/// # Errors
/// [`VideoDrawError::HdrToSdrNeedsHighBitFrame`] — HDR → SDR запрошен: у NV12 8 бит его нет
/// (HW 10 бит в GLES2 не объявляется, решение владельца 33).
pub(crate) fn plan_nv12_color(
    conversion: VideoColorConversion,
) -> Result<ColorTransform, VideoDrawError> {
    match conversion {
        VideoColorConversion::Sdr {
            encoding,
            adjustment,
        } => Ok(ColorTransform::new(encoding, adjustment)),
        VideoColorConversion::HdrToSdr(_) => Err(VideoDrawError::HdrToSdrNeedsHighBitFrame {
            depth: crate::planes::SampleDepth::Bits8,
        }),
    }
}

#[cfg(test)]
mod tests;
