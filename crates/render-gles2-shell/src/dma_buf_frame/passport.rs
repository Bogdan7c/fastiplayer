//! Нейтральная идентичность DMA-BUF кадра плеера → паспорт буфера видеослоя.
//!
//! Паспорт — ключ кэша импортов `gles2-video`: равные паспорта означают «тот же буфер ядра в
//! той же раскладке». Здесь только перевод слов; сравнивает паспорта кэш видеослоя.

use gles2_egui::DrmFourcc;
use gles2_video::{
    DmaBufBufferIdentity, DmaBufFrameFormat, DmaBufObjectIdentity, DmaBufPlaneArrangement,
    PlaneExtent,
};
use video_core::{DmaBufFrameExportLayout, DmaBufFrameIdentity};

use super::error::Nv12DmaBufError;

/// DRM fourcc `NV12` (`drm_fourcc.h`).
pub(super) const FOURCC_NV12: u32 = DrmFourcc::from_code(*b"NV12").0;
/// DRM fourcc `P010`: распознаётся только чтобы отказать честной причиной.
pub(super) const FOURCC_P010: u32 = DrmFourcc::from_code(*b"P010").0;

/// Раскладка плоскостей паспорта по раскладке экспорта декодера.
pub(super) const fn arrangement_for(layout: DmaBufFrameExportLayout) -> DmaBufPlaneArrangement {
    match layout {
        DmaBufFrameExportLayout::ComposedLayers => DmaBufPlaneArrangement::ComposedSingleObject,
        DmaBufFrameExportLayout::SeparateLayers => DmaBufPlaneArrangement::SeparatePlanes,
    }
}

/// Проверяет, что fourcc — NV12; P010 и всё остальное отвергает отдельными причинами.
pub(super) fn require_nv12_fourcc(fourcc: u32) -> Result<(), Nv12DmaBufError> {
    match fourcc {
        FOURCC_NV12 => Ok(()),
        FOURCC_P010 => Err(Nv12DmaBufError::TenBitHardwareNotDeclared { fourcc }),
        other => Err(Nv12DmaBufError::NotNv12 { fourcc: other }),
    }
}

/// Паспорт буфера видеослоя из идентичности кадра декодера (без fd, без обращения к драйверу).
///
/// Размер яркости — `width × height` идентичности (как у wgpu: размер Y — размер дескриптора,
/// цветность считает видеослой как `ceil(Y / 2)`).
///
/// # Errors
/// [`Nv12DmaBufError`]: fourcc не NV12 (P010 — отдельный вариант), нет объектов, у объектов
/// разные модификаторы.
pub fn dma_buf_buffer_identity(
    identity: &DmaBufFrameIdentity,
) -> Result<DmaBufBufferIdentity, Nv12DmaBufError> {
    require_nv12_fourcc(identity.fourcc)?;
    let first_object = identity.objects.first().ok_or(Nv12DmaBufError::NoObjects)?;
    let modifier = first_object.drm_format_modifier;
    if let Some(different) = identity
        .objects
        .iter()
        .find(|object| object.drm_format_modifier != modifier)
    {
        return Err(Nv12DmaBufError::ObjectModifiersDiffer {
            first: modifier,
            other: different.drm_format_modifier,
        });
    }
    Ok(DmaBufBufferIdentity {
        resource_id: identity.resource_id,
        objects: identity
            .objects
            .iter()
            .map(|object| DmaBufObjectIdentity {
                device: object.identity.device,
                inode: object.identity.inode,
                size: u64::from(object.size),
                modifier: object.drm_format_modifier,
            })
            .collect(),
        format: DmaBufFrameFormat {
            luma_extent: PlaneExtent {
                width: identity.width,
                height: identity.height,
            },
            arrangement: arrangement_for(identity.export_layout),
            modifier,
        },
    })
}
