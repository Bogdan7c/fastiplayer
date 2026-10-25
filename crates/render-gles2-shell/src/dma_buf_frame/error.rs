//! Типизированные отказы разбора аппаратного кадра NV12.
//!
//! Все они возникают ДО обращения к драйверу: кадр или паспорт не годятся для GLES2-импорта
//! по своему содержимому. Каждая причина — отдельный вариант, чтобы приложение могло различить
//! «этот формат у GLES2 не объявлен» и «декодер прислал странный дескриптор».

use gles2_video::DmaBufFrameError;
use thiserror::Error;
use video_core::DmaBufFrameExportLayout;
use video_frame_contract::VideoFramePixelLayout;

/// Кадр или паспорт DMA-BUF нельзя перевести в кадр NV12 видеослоя.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Nv12DmaBufError {
    /// P010 (10 бит) в аппаратном пути GLES2 не объявлен и не реализуется (решение владельца
    /// 33): GPU с 10-битным декодом всегда ведёт wgpu. Это не сбой драйвера, а границы рендера.
    #[error(
        "10-bit hardware frames (P010, fourcc {fourcc:#010x}) are not declared by the GLES2 renderer"
    )]
    TenBitHardwareNotDeclared {
        /// DRM fourcc `P010`.
        fourcc: u32,
    },
    /// Fourcc не NV12 (и не P010): другого аппаратного формата GLES2 не знает.
    #[error("DMA-BUF fourcc {fourcc:#010x} is not NV12")]
    NotNv12 {
        /// DRM fourcc кадра.
        fourcc: u32,
    },
    /// Контракт кадра — не NV12 (например, планарный YUV).
    #[error("frame contract pixel layout {pixel_layout:?} is not hardware NV12")]
    FrameContractNotNv12 {
        /// Раскладка пикселей из контракта кадра.
        pixel_layout: VideoFramePixelLayout,
    },
    /// В паспорте или дескрипторе нет ни одного объекта ядра.
    #[error("DMA-BUF frame has no kernel objects")]
    NoObjects,
    /// У объектов кадра разные DRM-модификаторы: паспорт видеослоя хранит один общий.
    #[error("DMA-BUF objects have different modifiers: {first:#x} and {other:#x}")]
    ObjectModifiersDiffer {
        /// Модификатор первого объекта.
        first: u64,
        /// Отличающийся модификатор другого объекта.
        other: u64,
    },
    /// Раскладка экспорта в дескрипторе не совпала с раскладкой паспорта.
    #[error("descriptor export layout {descriptor:?} differs from the buffer identity arrangement")]
    ExportLayoutDiffersFromIdentity {
        /// Раскладка экспорта в дескрипторе.
        descriptor: DmaBufFrameExportLayout,
    },
    /// Число слоёв не то, что требует раскладка (composed — 1, separate — 2).
    #[error("expected {expected} DRM layers, descriptor has {actual}")]
    LayerCount {
        /// Сколько слоёв требует раскладка.
        expected: usize,
        /// Сколько слоёв в дескрипторе.
        actual: usize,
    },
    /// Формат слоя не тот (composed — `NV12`, separate — `R8` и `GR88`).
    #[error("layer {layer_index} has DRM format {actual:#010x}, expected {expected:#010x}")]
    LayerFormat {
        /// Индекс слоя.
        layer_index: usize,
        /// Нужный fourcc.
        expected: u32,
        /// Fourcc в дескрипторе.
        actual: u32,
    },
    /// Число плоскостей слоя не то (composed — 2, separate — 1).
    #[error("layer {layer_index} has {actual} planes, expected {expected}")]
    LayerPlaneCount {
        /// Индекс слоя.
        layer_index: usize,
        /// Сколько плоскостей требуется.
        expected: u32,
        /// Сколько в дескрипторе.
        actual: u32,
    },
    /// Плоскость ссылается на объект, которого нет в дескрипторе.
    #[error(
        "layer {layer_index} plane {plane_index} references object {object_index}, descriptor has {object_count}"
    )]
    ObjectIndexOutOfRange {
        /// Индекс слоя.
        layer_index: usize,
        /// Индекс плоскости в слое.
        plane_index: usize,
        /// Запрошенный индекс объекта.
        object_index: usize,
        /// Сколько объектов в дескрипторе.
        object_count: usize,
    },
    /// Composed-кадр, плоскости которого лежат в РАЗНЫХ объектах: один образ NV12 так не собрать.
    #[error(
        "composed NV12 spans objects {luma_object_index} and {chroma_object_index}: single-object layout required"
    )]
    ComposedPlanesInDifferentObjects {
        /// Объект яркости.
        luma_object_index: usize,
        /// Объект цветности.
        chroma_object_index: usize,
    },
    /// Кадр противоречит паспорту или формату NV12 (проверка `gles2-video`).
    #[error("NV12 frame is inconsistent: {0}")]
    Frame(#[from] DmaBufFrameError),
}
