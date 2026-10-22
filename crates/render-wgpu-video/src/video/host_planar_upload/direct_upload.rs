//! План GPU-копии кадра прямо из памяти декодера, без CPU-копии в staging (cpu-parity 08b).
//!
//! Обычная загрузка host-planar кадра копирует строки каждой plane-ы на CPU в staging
//! (`wgpu_backend::copy_plane_block_into_staging`), и только потом GPU переносит их в
//! текстуру слота. Если декодер разложил кадр в импортируемый блок
//! (`HostPlanarFrameDescriptor::importable_memory`) со stride, кратным 256, GPU может
//! копировать в текстуры прямо из этого блока.
//!
//! Модуль не знает ни Vulkan, ни wgpu: он только проверяет, что каждая plane дескриптора
//! совпадает с раскладкой слота и что её stride/offset пригодны для копии буфер → текстура,
//! и собирает план. Импорт блока и запись копий делает backend
//! (`HostPlanarUploadBackend::record_direct_frame_upload`).

use anyhow::{Context, Result, ensure};
use video_core::{
    HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT, HostPlanarFrameDescriptor, HostPlanarImportableMemory,
};

use super::{HostPlanarUploadBackend, HostPlanarUploadLayout, HostPlanarUploadPlaneLayout};

/// Проверенный план копии всех plane-ов кадра из импортируемого блока.
#[derive(Debug, Clone)]
pub(super) struct DirectHostPlanarUpload {
    /// Блок памяти кадра (выделенная память) и offset-ы plane-ов.
    memory: HostPlanarImportableMemory,
    /// Дескриптор кадра: держит содержимое блока, пока GPU-копия не выполнится.
    frame: HostPlanarFrameDescriptor,
    /// Копия каждой plane-ы в порядке раскладки слота.
    planes: [DirectPlaneCopy; 3],
}

/// Где в блоке лежит одна plane и с каким шагом строк.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DirectPlaneCopy {
    /// Offset начала plane-ы от начала блока.
    pub(super) offset: u64,
    /// Шаг строк (`bytes_per_row` копии), кратен 256.
    pub(super) bytes_per_row: u32,
}

impl DirectHostPlanarUpload {
    /// Блок памяти кадра для импорта.
    pub(super) fn memory(&self) -> &HostPlanarImportableMemory {
        &self.memory
    }

    /// Дескриптор кадра, который GPU-копия обязана держать до своего завершения.
    pub(super) fn frame(&self) -> &HostPlanarFrameDescriptor {
        &self.frame
    }

    /// Копия plane-ы `plane_index` (0 — luma, 1 — U, 2 — V).
    pub(super) fn plane(&self, plane_index: usize) -> Option<DirectPlaneCopy> {
        self.planes.get(plane_index).copied()
    }
}

/// Итог планирования: можно ли загрузить кадр без CPU-копии.
#[derive(Debug)]
pub(super) enum DirectUploadPlanning {
    /// Кадр пригоден: план проверен.
    Planned(DirectHostPlanarUpload),
    /// Provider не дал импортируемый блок (обычный кадр) — штатный путь через staging.
    NotImportable,
    /// Блок есть, но не годится для прямой копии (stride не кратен 256, противоречивый
    /// блок). Не ошибка кадра: он пойдёт через staging, причина — для диагностики.
    Unsuitable(anyhow::Error),
}

/// Итог попытки записать копию кадра без CPU-копии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectFrameUploadRecording {
    /// Копии всех plane-ов записаны прямо из памяти декодера.
    Recorded,
    /// Не записано: кадр нужно загрузить через staging.
    NotRecorded,
}

/// Пробует записать GPU-копию кадра прямо из памяти декодера.
///
/// `Recorded` — копии записаны, staging не нужен; `NotRecorded` — backend не умеет
/// импорт, кадр без импортируемого блока или блок не подошёл (причина отдана backend-у
/// для диагностики): caller загружает кадр обычным путём через staging.
pub(super) fn try_record_direct_upload<B>(
    descriptor: &HostPlanarFrameDescriptor,
    layout: HostPlanarUploadLayout,
    uploaded_textures: &B::UploadedTextures,
    backend: &mut B,
) -> Result<DirectFrameUploadRecording>
where
    B: HostPlanarUploadBackend,
{
    if !backend.supports_direct_frame_upload() {
        return Ok(DirectFrameUploadRecording::NotRecorded);
    }
    match plan_direct_upload(descriptor, layout) {
        DirectUploadPlanning::Planned(upload) => {
            backend.record_direct_frame_upload(uploaded_textures, &upload)
        }
        DirectUploadPlanning::NotImportable => Ok(DirectFrameUploadRecording::NotRecorded),
        DirectUploadPlanning::Unsuitable(reason) => {
            backend.note_unsuitable_direct_upload(&reason);
            Ok(DirectFrameUploadRecording::NotRecorded)
        }
    }
}

/// Строит план прямой GPU-копии кадра в слот с раскладкой `layout`.
pub(super) fn plan_direct_upload(
    descriptor: &HostPlanarFrameDescriptor,
    layout: HostPlanarUploadLayout,
) -> DirectUploadPlanning {
    let memory = match descriptor.importable_memory() {
        Ok(Some(memory)) => memory,
        Ok(None) => return DirectUploadPlanning::NotImportable,
        Err(error) => return DirectUploadPlanning::Unsuitable(error),
    };
    match plan_plane_copies(descriptor, &memory, layout) {
        Ok(planes) => DirectUploadPlanning::Planned(DirectHostPlanarUpload {
            memory,
            frame: descriptor.clone(),
            planes,
        }),
        Err(error) => DirectUploadPlanning::Unsuitable(error),
    }
}

/// Проверяет каждую plane и переводит её offset/stride в параметры копии.
fn plan_plane_copies(
    descriptor: &HostPlanarFrameDescriptor,
    memory: &HostPlanarImportableMemory,
    layout: HostPlanarUploadLayout,
) -> Result<[DirectPlaneCopy; 3]> {
    let mut planes = [DirectPlaneCopy {
        offset: 0,
        bytes_per_row: 0,
    }; 3];
    for (plane_index, plane_layout) in layout.planes.into_iter().enumerate() {
        planes[plane_index] = plan_plane_copy(descriptor, memory, plane_index, plane_layout)?;
    }
    Ok(planes)
}

fn plan_plane_copy(
    descriptor: &HostPlanarFrameDescriptor,
    memory: &HostPlanarImportableMemory,
    plane_index: usize,
    plane_layout: HostPlanarUploadPlaneLayout,
) -> Result<DirectPlaneCopy> {
    let plane = descriptor
        .planes
        .get(plane_index)
        .with_context(|| format!("host-planar descriptor plane index {plane_index} is missing"))?;
    // Те же проверки, что у копии через staging: plane дескриптора обязана совпасть с
    // текстурой слота, иначе GPU записал бы в текстуру чужой размер.
    ensure!(
        plane.role == plane_layout.role
            && plane.visible_width == plane_layout.width
            && plane.visible_height == plane_layout.height
            && plane.bytes_per_sample == plane_layout.bytes_per_sample,
        "host-planar {:?} descriptor metadata does not match upload layout",
        plane.role
    );
    ensure!(
        plane
            .stride
            .is_multiple_of(HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT),
        "host-planar {:?} stride {} is not a multiple of {HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT}",
        plane.role,
        plane.stride
    );
    let offset = memory.plane_offset(plane_index).with_context(|| {
        format!("host-planar importable block has no offset for plane {plane_index}")
    })?;
    // Копия буфер → текстура требует offset, кратный размеру texel-а (1 или 2 байта).
    ensure!(
        offset.is_multiple_of(plane.bytes_per_sample),
        "host-planar {:?} plane offset {offset} is not aligned to its sample size",
        plane.role
    );
    Ok(DirectPlaneCopy {
        offset: u64::try_from(offset).context("host-planar plane offset does not fit u64")?,
        bytes_per_row: u32::try_from(plane.stride)
            .context("host-planar plane stride does not fit u32")?,
    })
}

#[cfg(test)]
mod tests;
