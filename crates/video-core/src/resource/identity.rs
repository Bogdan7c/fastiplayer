//! Стабильная идентичность DMA-BUF кадра без platform fd.
//!
//! Зачем это нужно. `FrameResourceHandle` уникален на каждый декодированный
//! кадр, а GPU-буфер под кадром живёт долго: аппаратный декодер переиспользует
//! одну и ту же выходную поверхность из своего пула на многих кадрах. Рендерер,
//! который импортирует DMA-BUF в Vulkan, хочет сделать этот импорт один раз на
//! поверхность, а не на кадр. Для этого ему нужен ключ, который:
//! - не меняется от кадра к кадру, пока под ним тот же буфер;
//! - меняется, когда декодер пересоздал буфер (смена формата или разрешения),
//!   даже если backend-id поверхности совпал;
//! - не зависит от номера fd (fd каждый раз новый и закрывается).
//!
//! Этот модуль чистый: он только переписывает поля descriptor-а в сравнимую
//! форму и не дублирует fd. Владелец descriptor-а (backend) формирует
//! идентичность, рендерер её только сравнивает.

use std::sync::Arc;

use super::descriptor::{
    DmaBufFrameDescriptor, DmaBufFrameExportLayout, DmaBufLayerDescriptor, DmaBufObjectIdentity,
};

/// Идентичность одного DMA-BUF объекта вместе с его неизменяемой геометрией.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufObjectIdentityDescriptor {
    /// Kernel identity объекта (`st_dev`/`st_ino`/`st_rdev`), не fd.
    pub identity: DmaBufObjectIdentity,

    /// Размер объекта в байтах.
    pub size: u32,

    /// DRM format modifier backing memory layout-а.
    pub drm_format_modifier: u64,
}

/// Полная идентичность DMA-BUF кадра: backend resource id, kernel identity
/// объектов и layout. Равенство двух значений означает «тот же буфер в той же
/// раскладке», поэтому импортированный Vulkan image можно переиспользовать.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmaBufFrameIdentity {
    /// Backend-stable id ресурса; для VA-API это numeric surface id.
    pub resource_id: u64,

    /// Top-level DRM fourcc exported surface-а.
    pub fourcc: u32,

    /// Layout export-а.
    pub export_layout: DmaBufFrameExportLayout,

    /// Coded width image-а.
    pub width: u32,

    /// Coded height image-а.
    pub height: u32,

    /// Объекты в порядке descriptor-а.
    pub objects: Vec<DmaBufObjectIdentityDescriptor>,

    /// DRM PRIME layers в порядке descriptor-а.
    pub layers: Vec<DmaBufLayerDescriptor>,
}

impl DmaBufFrameIdentity {
    /// Сигнатура формата потока: по ней рендерер понимает, что декодер
    /// переключился на другой формат/разрешение, и кэш старого формата надо
    /// освободить целиком до создания новых image.
    #[must_use]
    pub fn format_signature(&self) -> DmaBufFrameFormatSignature {
        DmaBufFrameFormatSignature {
            fourcc: self.fourcc,
            export_layout: self.export_layout,
            width: self.width,
            height: self.height,
            drm_format_modifier: self
                .objects
                .first()
                .map(|object| object.drm_format_modifier),
        }
    }

    /// Возвращает `true`, если оба значения описывают один и тот же backend
    /// resource id. Совпадение id при разной identity означает, что декодер
    /// пересоздал буфер под тем же номером поверхности.
    #[must_use]
    pub fn shares_resource_id(&self, other: &Self) -> bool {
        self.resource_id == other.resource_id
    }
}

/// Та часть идентичности, которая одинакова для всех поверхностей одного
/// декодируемого потока. Используется для групповой инвалидации кэша.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufFrameFormatSignature {
    /// Top-level DRM fourcc.
    pub fourcc: u32,

    /// Layout export-а.
    pub export_layout: DmaBufFrameExportLayout,

    /// Coded width.
    pub width: u32,

    /// Coded height.
    pub height: u32,

    /// Modifier первого объекта; `None`, если объектов нет.
    pub drm_format_modifier: Option<u64>,
}

impl DmaBufFrameDescriptor {
    /// Снимает стабильную идентичность с descriptor-а, не трогая fd.
    #[must_use]
    pub fn identity(&self) -> DmaBufFrameIdentity {
        DmaBufFrameIdentity {
            resource_id: self.resource_id,
            fourcc: self.fourcc,
            export_layout: self.export_layout,
            width: self.width,
            height: self.height,
            objects: self
                .objects
                .iter()
                .map(|object| DmaBufObjectIdentityDescriptor {
                    identity: object.identity,
                    size: object.size,
                    drm_format_modifier: object.drm_format_modifier,
                })
                .collect(),
            layers: self.layers.clone(),
        }
    }
}

/// Renderer-neutral идентичность frame resource-а для дешёвой проверки кэша.
///
/// `Arc` внутри делает клонирование на горячем пути бесплатным: provider хранит
/// одно значение на слот и отдаёт ссылку на него каждый кадр.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FrameResourceIdentity {
    /// Linux DMA-BUF кадр.
    DmaBuf(Arc<DmaBufFrameIdentity>),
}
