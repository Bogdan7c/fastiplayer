//! Кадр программного декодера → плоскости видеослоя, без копии.
//!
//! Дескриптор кадра FFmpeg (`HostPlanarFrameDescriptor`) уже отдаёт каждую плоскость одним
//! непрерывным срезом (`visible_plane_block`): от начала первой строки до конца видимой части
//! последней. Этот срез и уходит в `glTexSubImage2D` — единственная копия пути.
//!
//! Перед переводом кадр сверяется с контрактом через `video-core` (число и порядок плоскостей,
//! размеры, stride против видимой строки, границы backing-а) — та же проверка, что у wgpu.

use gles2_video::{
    ChromaSubsampling, HostRows, PlanarFrameUpload, PlanarLayout, PlaneExtent, PlaneSource,
    PlaneSourceError, SampleDepth, VideoFrameKey,
};
use thiserror::Error;
use video_core::{
    DecodedFrame, FrameResourceDescriptor, HostPlanarFrameDescriptor,
    validate_resource_descriptor_against_contract,
};
use video_frame_contract::{VideoFramePixelLayout, VideoFrameTransferPath};

/// Кадр нельзя перевести в плоскости видеослоя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostPlanarFrameError {
    /// Дескриптор DMA-BUF (аппаратный кадр): его импортирует сессия 07, не этот путь.
    #[error("DMA-BUF frames are imported by the hardware path, not uploaded from host rows")]
    DmaBufDescriptor,
    /// Контракт кадра — не программная загрузка из памяти процесса.
    #[error("frame contract transfer path {0:?} is not a software host upload")]
    NotSoftwareHostUpload(VideoFrameTransferPath),
    /// Раскладка вне программного пути GLES2 (NV12/P010 — аппаратные, сессия 07; RGBA).
    #[error("pixel layout {0:?} is not supported by the GLES2 software path")]
    UnsupportedLayout(VideoFramePixelLayout),
    /// Дескриптор противоречит контракту кадра (проверка `video-core`).
    #[error("host-planar descriptor does not match the frame contract: {message}")]
    ContractRejected {
        /// Причина от `video-core` со всей цепочкой.
        message: String,
    },
    /// Плоскость не отдала свой срез.
    #[error("host-planar plane {plane_index} is not readable: {message}")]
    PlaneUnavailable {
        /// Номер плоскости (0 — Y, 1 — U, 2 — V).
        plane_index: usize,
        /// Причина от `video-core`.
        message: String,
    },
    /// Строки плоскости противоречат сами себе (не должно случаться после проверки контракта).
    #[error("host-planar plane {plane_index} rows: {source}")]
    PlaneRows {
        /// Номер плоскости.
        plane_index: usize,
        /// Причина.
        #[source]
        source: PlaneSourceError,
    },
}

/// Плоскости кадра `frame` для загрузки в видеослой.
///
/// Байты не копируются: результат заимствует срезы дескриптора и живёт не дольше него. Ключ
/// кадра — `FrameResourceHandle` декодера (тот же ключ повтора, что у wgpu).
///
/// # Errors
/// [`HostPlanarFrameError`]: DMA-BUF вместо памяти процесса, контракт не программной загрузки,
/// раскладка вне матрицы v1 (YUV 4:2:0/4:2:2 8/10/12 бит, 4:4:4 8/10 бит), дескриптор не совпал
/// с контрактом.
pub fn host_planar_frame_upload<'descriptor>(
    frame: &DecodedFrame,
    descriptor: &'descriptor FrameResourceDescriptor,
) -> Result<PlanarFrameUpload<'descriptor>, HostPlanarFrameError> {
    let host_planar = match descriptor {
        FrameResourceDescriptor::HostPlanar(host_planar) => host_planar,
        FrameResourceDescriptor::DmaBuf(_) => return Err(HostPlanarFrameError::DmaBufDescriptor),
    };
    let contract = frame.frame_contract;
    if contract.transfer_path != VideoFrameTransferPath::SoftwareHostUpload {
        return Err(HostPlanarFrameError::NotSoftwareHostUpload(
            contract.transfer_path,
        ));
    }
    let layout = planar_layout(contract.pixel_layout)?;
    validate_resource_descriptor_against_contract(contract, frame.width, frame.height, descriptor)
        .map_err(|error| HostPlanarFrameError::ContractRejected {
            message: format!("{error:#}"),
        })?;
    Ok(PlanarFrameUpload {
        key: VideoFrameKey(frame.resource_handle.0),
        layout,
        luma_extent: PlaneExtent {
            width: frame.width,
            height: frame.height,
        },
        luma: plane_source(host_planar, 0)?,
        chroma_u: plane_source(host_planar, 1)?,
        chroma_v: plane_source(host_planar, 2)?,
    })
}

/// Раскладка видеослоя для программных раскладок плеера (матрица v1 wgpu). 10/12 бит у FFmpeg —
/// 16-битные слова LE (`*Le`), ровно то, что видеослой грузит парами байтов.
fn planar_layout(layout: VideoFramePixelLayout) -> Result<PlanarLayout, HostPlanarFrameError> {
    let (chroma, depth) = match layout {
        VideoFramePixelLayout::Yuv420Planar8 => (ChromaSubsampling::Yuv420, SampleDepth::Bits8),
        VideoFramePixelLayout::Yuv422Planar8 => (ChromaSubsampling::Yuv422, SampleDepth::Bits8),
        VideoFramePixelLayout::Yuv444Planar8 => (ChromaSubsampling::Yuv444, SampleDepth::Bits8),
        VideoFramePixelLayout::Yuv420Planar10Le => (ChromaSubsampling::Yuv420, SampleDepth::Bits10),
        VideoFramePixelLayout::Yuv420Planar12Le => (ChromaSubsampling::Yuv420, SampleDepth::Bits12),
        VideoFramePixelLayout::Yuv422Planar10Le => (ChromaSubsampling::Yuv422, SampleDepth::Bits10),
        VideoFramePixelLayout::Yuv422Planar12Le => (ChromaSubsampling::Yuv422, SampleDepth::Bits12),
        VideoFramePixelLayout::Yuv444Planar10Le => (ChromaSubsampling::Yuv444, SampleDepth::Bits10),
        VideoFramePixelLayout::Nv12
        | VideoFramePixelLayout::P010
        | VideoFramePixelLayout::Rgba8 => {
            return Err(HostPlanarFrameError::UnsupportedLayout(layout));
        }
    };
    Ok(PlanarLayout { chroma, depth })
}

/// Срез одной плоскости дескриптора как строки видеослоя.
fn plane_source(
    descriptor: &HostPlanarFrameDescriptor,
    plane_index: usize,
) -> Result<PlaneSource<'_>, HostPlanarFrameError> {
    let block = descriptor
        .visible_plane_block(plane_index)
        .map_err(|error| HostPlanarFrameError::PlaneUnavailable {
            plane_index,
            message: format!("{error:#}"),
        })?;
    let rows = HostRows::new(
        block.bytes,
        block.stride,
        block.visible_row_bytes,
        block.visible_height,
    )
    .map_err(|source| HostPlanarFrameError::PlaneRows {
        plane_index,
        source,
    })?;
    Ok(PlaneSource::HostRows(rows))
}

#[cfg(test)]
mod tests;
