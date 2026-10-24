//! Цветовой путь кадра 10/12 бит: те же правила, что у 16-битного пути wgpu
//! (`select_host_yuv420_color_path` и `validate_hdr_to_sdr_settings_for_host_yuv420` в
//! `render-wgpu-video/src/video/host_yuv420_renderer.rs`).
//!
//! - SDR: только BT.709 (основные цвета, матрица и передаточная функция) с явным диапазоном;
//!   цветокоррекции нет — wgpu её на этом пути не применяет, паритет важнее возможности
//!   видеослоя.
//! - HDR (передаточная функция PQ/HLG у кадра или в его HDR-метаданных): BT.2020 основные цвета
//!   и матрица, явный диапазон, метаданные мастеринга согласованы с кадром, включён HDR → SDR
//!   BT.2446-C с проверенными уровнями.
//! - Всё остальное — типизированный отказ, а не тихая подмена: у wgpu это тоже ошибка рендера
//!   (в отличие от 8-битного пути, где неизвестный вход рисуется как SDR BT.709 limited).

use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
};
use gles2_video::{
    ColorAdjustment, HdrReferenceLevels, HdrToSdrConversion, HdrTransfer, VideoColorConversion,
    YuvColorEncoding, YuvMatrix, YuvRange,
};
use render_core::HdrToSdrSettings;
use thiserror::Error;

/// Кадр 10/12 бит нельзя нарисовать с такими метаданными или настройками.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HighBitColorError {
    /// Не SDR BT.709 и не HDR: широкий охват SDR, BT.601, неизвестные метаданные.
    #[error(
        "10/12-bit frame color is neither SDR BT.709 nor HDR: primaries {primaries:?}, \
         matrix {matrix:?}, transfer {transfer:?}"
    )]
    UnsupportedColor {
        /// Основные цвета.
        primaries: ColorPrimaries,
        /// Матрица.
        matrix: MatrixCoefficients,
        /// Передаточная функция.
        transfer: TransferFunction,
    },
    /// Диапазон кодов не указан (limited или full обязателен, как у wgpu).
    #[error("10/12-bit frame needs an explicit limited/full color range")]
    RangeUnknown,
    /// HDR-кадр с метаданными, которые путь BT.2446-C не принимает.
    #[error("HDR frame metadata is not supported: {0}")]
    UnsupportedHdr(HdrColorRejection),
    /// HDR-кадр, а HDR → SDR выключен или его настройки вне контракта BT.2446-C SDR BT.709.
    #[error("HDR frame needs enabled BT.2446-C SDR BT.709 HDR-to-SDR settings")]
    HdrToSdrUnavailable,
}

/// Почему HDR-метаданные не подходят.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HdrColorRejection {
    /// Передаточная функция кадра не PQ и не HLG (HDR только в метаданных мастеринга).
    #[error("transfer {0:?} is neither PQ nor HLG")]
    Transfer(TransferFunction),
    /// Основные цвета не BT.2020.
    #[error("primaries {0:?} are not BT.2020")]
    Primaries(ColorPrimaries),
    /// Матрица не BT.2020.
    #[error("matrix {0:?} is not BT.2020")]
    Matrix(MatrixCoefficients),
    /// Метаданные мастеринга называют другие основные цвета или передаточную функцию.
    #[error("mastering metadata disagrees with the frame color")]
    MasteringMismatch,
}

/// Перевод цвета кадра 10/12 бит для видеослоя.
///
/// # Errors
/// [`HighBitColorError`]: метаданные вне пути wgpu или HDR без включённого HDR → SDR.
pub fn high_bit_color_conversion(
    color: &VideoColorMetadata,
    hdr_to_sdr: &HdrToSdrSettings,
) -> Result<VideoColorConversion, HighBitColorError> {
    if !color.requires_hdr_processing() {
        if color.primaries == ColorPrimaries::Bt709
            && color.matrix == MatrixCoefficients::Bt709
            && color.transfer == TransferFunction::Bt709
        {
            return Ok(VideoColorConversion::Sdr {
                encoding: YuvColorEncoding {
                    matrix: YuvMatrix::Bt709,
                    range: explicit_range(color.range)?,
                },
                adjustment: ColorAdjustment::IDENTITY,
            });
        }
        return Err(HighBitColorError::UnsupportedColor {
            primaries: color.primaries,
            matrix: color.matrix,
            transfer: color.transfer,
        });
    }
    let transfer = hdr_transfer(color)?;
    let range = explicit_range(color.range)?;
    if !hdr_to_sdr.is_phase10_bt2446_c_sdr_bt709() {
        return Err(HighBitColorError::HdrToSdrUnavailable);
    }
    // Те же условия, что `is_phase10_bt2446_c_sdr_bt709` (конечные, положительные, пик выше
    // белого), поэтому отказ здесь — та же недоступность настроек.
    let levels = HdrReferenceLevels::new(
        hdr_to_sdr.sdr_reference_white_nits,
        hdr_to_sdr.hdr_reference_peak_nits,
    )
    .map_err(|_| HighBitColorError::HdrToSdrUnavailable)?;
    Ok(VideoColorConversion::HdrToSdr(HdrToSdrConversion {
        range,
        transfer,
        levels,
    }))
}

/// Проверки HDR-метаданных (`validate_host_yuv420_hdr_core_metadata`) и передаточная функция.
fn hdr_transfer(color: &VideoColorMetadata) -> Result<HdrTransfer, HighBitColorError> {
    let reject = |reason| Err(HighBitColorError::UnsupportedHdr(reason));
    let transfer = match color.transfer {
        TransferFunction::Pq => HdrTransfer::Pq,
        TransferFunction::Hlg => HdrTransfer::Hlg,
        other => return reject(HdrColorRejection::Transfer(other)),
    };
    if color.primaries != ColorPrimaries::Bt2020 {
        return reject(HdrColorRejection::Primaries(color.primaries));
    }
    if color.matrix != MatrixCoefficients::Bt2020 {
        return reject(HdrColorRejection::Matrix(color.matrix));
    }
    if let Some(mastering) = &color.hdr_metadata
        && (mastering.color_primaries != color.primaries
            || mastering.transfer_function != color.transfer)
    {
        return reject(HdrColorRejection::MasteringMismatch);
    }
    Ok(transfer)
}

fn explicit_range(range: ColorRange) -> Result<YuvRange, HighBitColorError> {
    match range {
        ColorRange::Limited => Ok(YuvRange::Limited),
        ColorRange::Full => Ok(YuvRange::Full),
        ColorRange::Unknown => Err(HighBitColorError::RangeUnknown),
    }
}

#[cfg(test)]
mod tests;
