//! Дескриптор DMA-BUF кадра декодера → кадр NV12 видеослоя.
//!
//! Разбор слоёв экспорта DRM PRIME (`VADRMPRIMESurfaceDescriptor` у VA-API):
//! - **composed** — один слой `NV12` с двумя плоскостями, обе в ОДНОМ объекте ядра;
//! - **separate** — два слоя по одной плоскости: `R8` (яркость) и `GR88` (цветность), объекты
//!   могут быть разными.
//!
//! Что видеослой получит для каждой плоскости: fd объекта, смещение и шаг строк из слоя,
//! модификатор объекта. Всё проверяется ДО драйвера: любой отказ — типизированный
//! [`Nv12DmaBufError`].
//!
//! fd кадра заимствуются у дескриптора ([`DmaBufNv12Frame`] держит `BorrowedFd`): закрывает их
//! тот, кто владеет дескриптором — после импорта (EGL fd не забирает,
//! `EGL_EXT_image_dma_buf_import`, issue 3).

use std::os::fd::AsFd;

use gles2_egui::DrmFourcc;
use gles2_video::{
    DmaBufBufferIdentity, DmaBufNv12Frame, DmaBufPlane, DmaBufPlaneArrangement, PlaneExtent,
    VideoFrameKey,
};
use video_core::{DecodedFrame, DmaBufFrameDescriptor, DmaBufLayerDescriptor};
use video_frame_contract::VideoFramePixelLayout;

use super::error::Nv12DmaBufError;
use super::passport::{FOURCC_NV12, FOURCC_P010, arrangement_for, require_nv12_fourcc};

/// Ключ кадра в видеослое: `FrameResourceHandle` декодера (тот же ключ повтора, что у
/// программного пути и у wgpu).
#[must_use]
pub fn dma_buf_frame_key(frame: &DecodedFrame) -> VideoFrameKey {
    VideoFrameKey(frame.resource_handle.0)
}

/// Контракт кадра должен быть аппаратным NV12; P010 отвергается отдельной причиной.
///
/// # Errors
/// [`Nv12DmaBufError::TenBitHardwareNotDeclared`] для P010, [`Nv12DmaBufError::FrameContractNotNv12`]
/// для любой другой раскладки.
pub fn ensure_nv12_frame_contract(frame: &DecodedFrame) -> Result<(), Nv12DmaBufError> {
    match frame.frame_contract.pixel_layout {
        VideoFramePixelLayout::Nv12 => Ok(()),
        VideoFramePixelLayout::P010 => Err(Nv12DmaBufError::TenBitHardwareNotDeclared {
            fourcc: FOURCC_P010,
        }),
        pixel_layout => Err(Nv12DmaBufError::FrameContractNotNv12 { pixel_layout }),
    }
}

/// Кадр NV12 для импорта в видеослой; заимствует fd у `descriptor`.
///
/// `passport` — паспорт этого же буфера (из идентичности провайдера либо из дескриптора через
/// [`DmaBufFrameDescriptor::identity`]): раскладка, размер и модификатор в нём должны
/// совпасть с дескриптором, иначе кэш видеослоя отдал бы чужую текстуру.
///
/// # Errors
/// [`Nv12DmaBufError`]: не NV12 (P010 — отдельный вариант), раскладка экспорта не совпала с
/// паспортом, неверное число слоёв/плоскостей, формат слоя, индекс объекта вне списка, composed
/// из нескольких объектов, несогласованность кадра с паспортом (`Frame`).
pub fn nv12_dma_buf_frame<'descriptor>(
    frame: &DecodedFrame,
    descriptor: &'descriptor DmaBufFrameDescriptor,
    passport: &'descriptor DmaBufBufferIdentity,
) -> Result<DmaBufNv12Frame<'descriptor>, Nv12DmaBufError> {
    ensure_nv12_frame_contract(frame)?;
    require_nv12_fourcc(descriptor.fourcc)?;
    if arrangement_for(descriptor.export_layout) != passport.format.arrangement {
        return Err(Nv12DmaBufError::ExportLayoutDiffersFromIdentity {
            descriptor: descriptor.export_layout,
        });
    }
    let (luma, chroma) = match passport.format.arrangement {
        DmaBufPlaneArrangement::ComposedSingleObject => composed_planes(descriptor)?,
        DmaBufPlaneArrangement::SeparatePlanes => separate_planes(descriptor)?,
    };
    // Размер яркости — размер дескриптора, цветность видеослой считает как `ceil(Y / 2)`
    // (то же делает импорт wgpu: `width × height` и `div_ceil(2)`).
    let luma_extent = PlaneExtent {
        width: descriptor.width,
        height: descriptor.height,
    };
    Ok(DmaBufNv12Frame::new(
        dma_buf_frame_key(frame),
        passport,
        luma_extent,
        luma,
        chroma,
    )?)
}

/// Composed: один слой `NV12` из двух плоскостей, обе в одном объекте.
fn composed_planes(
    descriptor: &DmaBufFrameDescriptor,
) -> Result<(DmaBufPlane<'_>, DmaBufPlane<'_>), Nv12DmaBufError> {
    require_layer_count(descriptor, 1)?;
    require_layer_format(0, &descriptor.layers[0], FOURCC_NV12)?;
    require_plane_count(0, &descriptor.layers[0], 2)?;
    let (luma_object, luma) = plane_of_layer(descriptor, 0, 0, DrmFourcc::R8)?;
    let (chroma_object, chroma) = plane_of_layer(descriptor, 0, 1, DrmFourcc::GR88)?;
    // Один образ NV12 собирается из одного объекта: это и есть проверка «composed».
    if luma_object != chroma_object {
        return Err(Nv12DmaBufError::ComposedPlanesInDifferentObjects {
            luma_object_index: luma_object,
            chroma_object_index: chroma_object,
        });
    }
    Ok((luma, chroma))
}

/// Separate: слой `R8` (яркость) и слой `GR88` (цветность) по одной плоскости.
fn separate_planes(
    descriptor: &DmaBufFrameDescriptor,
) -> Result<(DmaBufPlane<'_>, DmaBufPlane<'_>), Nv12DmaBufError> {
    require_layer_count(descriptor, 2)?;
    for (layer_index, format) in [(0, DrmFourcc::R8), (1, DrmFourcc::GR88)] {
        let layer = &descriptor.layers[layer_index];
        require_layer_format(layer_index, layer, format.0)?;
        require_plane_count(layer_index, layer, 1)?;
    }
    let (_, luma) = plane_of_layer(descriptor, 0, 0, DrmFourcc::R8)?;
    let (_, chroma) = plane_of_layer(descriptor, 1, 0, DrmFourcc::GR88)?;
    Ok((luma, chroma))
}

/// Число слоёв должно быть ровно `expected` (после этой проверки индексы слоёв безопасны).
fn require_layer_count(
    descriptor: &DmaBufFrameDescriptor,
    expected: usize,
) -> Result<(), Nv12DmaBufError> {
    let actual = descriptor.layers.len();
    if actual == expected {
        Ok(())
    } else {
        Err(Nv12DmaBufError::LayerCount { expected, actual })
    }
}

fn require_layer_format(
    layer_index: usize,
    layer: &DmaBufLayerDescriptor,
    expected: u32,
) -> Result<(), Nv12DmaBufError> {
    if layer.drm_format == expected {
        Ok(())
    } else {
        Err(Nv12DmaBufError::LayerFormat {
            layer_index,
            expected,
            actual: layer.drm_format,
        })
    }
}

fn require_plane_count(
    layer_index: usize,
    layer: &DmaBufLayerDescriptor,
    expected: u32,
) -> Result<(), Nv12DmaBufError> {
    if layer.num_planes == expected {
        Ok(())
    } else {
        Err(Nv12DmaBufError::LayerPlaneCount {
            layer_index,
            expected,
            actual: layer.num_planes,
        })
    }
}

/// Плоскость `plane_index` слоя `layer_index` как плоскость видеослоя и индекс её объекта.
///
/// Модификатор плоскости — модификатор её объекта; смещение и шаг — из слоя.
fn plane_of_layer(
    descriptor: &DmaBufFrameDescriptor,
    layer_index: usize,
    plane_index: usize,
    format: DrmFourcc,
) -> Result<(usize, DmaBufPlane<'_>), Nv12DmaBufError> {
    let layer = &descriptor.layers[layer_index];
    let object_index = usize::from(layer.object_index[plane_index]);
    let object =
        descriptor
            .objects
            .get(object_index)
            .ok_or(Nv12DmaBufError::ObjectIndexOutOfRange {
                layer_index,
                plane_index,
                object_index,
                object_count: descriptor.objects.len(),
            })?;
    let plane = DmaBufPlane {
        fd: object.fd.as_fd(),
        offset: u64::from(layer.offset[plane_index]),
        pitch: layer.pitch[plane_index],
        modifier: object.drm_format_modifier,
        drm_format: format.0,
    };
    Ok((object_index, plane))
}
