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
    /// Раскладка, которую GLES2-рендер пока не объявляет (10/12 бит — сессия 06).
    #[error("pixel layout {0:?} is not supported by the GLES2 software path yet")]
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
/// раскладка вне трёх 8-битных, дескриптор не совпал с контрактом.
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
    let chroma = chroma_for_layout(contract.pixel_layout)?;
    validate_resource_descriptor_against_contract(contract, frame.width, frame.height, descriptor)
        .map_err(|error| HostPlanarFrameError::ContractRejected {
            message: format!("{error:#}"),
        })?;
    Ok(PlanarFrameUpload {
        key: VideoFrameKey(frame.resource_handle.0),
        layout: PlanarLayout {
            chroma,
            depth: SampleDepth::Bits8,
        },
        luma_extent: PlaneExtent {
            width: frame.width,
            height: frame.height,
        },
        luma: plane_source(host_planar, 0)?,
        chroma_u: plane_source(host_planar, 1)?,
        chroma_v: plane_source(host_planar, 2)?,
    })
}

/// Прореживание цветности для трёх раскладок, которые объявляет GLES2-рендер.
fn chroma_for_layout(
    layout: VideoFramePixelLayout,
) -> Result<ChromaSubsampling, HostPlanarFrameError> {
    match layout {
        VideoFramePixelLayout::Yuv420Planar8 => Ok(ChromaSubsampling::Yuv420),
        VideoFramePixelLayout::Yuv422Planar8 => Ok(ChromaSubsampling::Yuv422),
        VideoFramePixelLayout::Yuv444Planar8 => Ok(ChromaSubsampling::Yuv444),
        VideoFramePixelLayout::Nv12
        | VideoFramePixelLayout::P010
        | VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv420Planar12Le
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv422Planar12Le
        | VideoFramePixelLayout::Yuv444Planar10Le
        | VideoFramePixelLayout::Rgba8 => Err(HostPlanarFrameError::UnsupportedLayout(layout)),
    }
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
