use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::{fmt, io};

use anyhow::{Context, ensure};
/// Opaque handle decoded frame resource-а.
///
/// Handle не раскрывает backend storage: decoder/provider владеет таблицей
/// resources, а playback/render слои используют это значение только как ключ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameResourceHandle(pub u64);

/// Renderer-neutral описание platform resource-а decoded frame-а.
///
/// Контракт намеренно хранит только переносимые для boundary данные. WGPU,
/// Vulkan, VA-API, GBM и cros-codecs типы остаются в concrete crates.
#[derive(Debug)]
pub enum FrameResourceDescriptor {
    /// Linux DMA-BUF/DRM PRIME descriptor для zero-copy materialization.
    DmaBuf(DmaBufFrameDescriptor),

    /// CPU-visible planar frame, готовый для будущего host-upload path-а.
    HostPlanar(HostPlanarFrameDescriptor),
}

impl FrameResourceDescriptor {
    /// Дублирует owned platform handles для renderer-side materializer-а.
    ///
    /// Provider продолжает владеть исходным descriptor-ом и decoder frame-ом;
    /// renderer получает отдельные fd и обязан закрыть их через обычный drop.
    pub fn try_clone_for_lookup(&self) -> io::Result<Self> {
        match self {
            Self::DmaBuf(descriptor) => descriptor.try_clone_for_lookup().map(Self::DmaBuf),
            Self::HostPlanar(descriptor) => Ok(Self::HostPlanar(descriptor.clone_for_lookup())),
        }
    }
}

/// Роль plane внутри owned host-planar frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlaneRole {
    /// Luma/Y plane.
    Luma,

    /// Chroma U/Cb plane.
    ChromaU,

    /// Chroma V/Cr plane.
    ChromaV,
}

/// Описание одной visible plane внутри общего host storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlaneDescriptor {
    /// Смысл plane-а; layout остаётся в `VideoFrameContract`, не в descriptor-е.
    pub role: HostPlaneRole,

    /// Byte offset начала plane-а внутри backing-а, которым владеет `HostPlanarFrameOwner`.
    ///
    /// Для simple owned-buffer owner-а это offset от начала общего byte slice-а.
    /// Provider-owned owner может интерпретировать offset относительно своей
    /// plane storage, не раскрывая raw pointers за пределы provider-а.
    pub offset: usize,

    /// Byte stride между началом соседних строк.
    pub stride: usize,

    /// Видимая ширина plane-а в samples; padding справа сюда не входит.
    pub visible_width: u32,

    /// Видимая высота plane-а в строках.
    pub visible_height: u32,

    /// Размер одного sample в байтах: 1 для 8-bit, 2 для 10/12-bit LE storage word.
    pub bytes_per_sample: usize,
}

/// Владелец CPU-visible backing-а для host-planar frame-а.
///
/// `video-core` знает только этот neutral trait. Concrete provider может держать
/// внутри refcounted decoder frame или другой resource, но наружу отдаёт только
/// безопасный slice видимых bytes конкретной строки plane-а.
pub trait HostPlanarFrameOwner: fmt::Debug + Send + Sync {
    /// Возвращает только видимые bytes строки, без правого padding-а.
    ///
    /// `plane_index` и metadata принадлежат descriptor-у; owner использует их,
    /// чтобы найти строку в своём backing-е и не раскрывать storage details.
    fn visible_row_bytes(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]>;

    /// Возвращает один непрерывный блок plane-а, покрывающий все видимые строки.
    ///
    /// Блок начинается с начала plane-а и имеет длину ровно `block_len`
    /// (`(visible_height - 1) * stride + visible_row_bytes`), то есть включает
    /// inter-row padding между строками, но не правый padding последней строки.
    /// Это позволяет renderer-у залить весь plane одним host->GPU upload-ом со
    /// `bytes_per_row = stride` вместо одного вызова на строку, не делая
    /// промежуточной CPU-копии. Owner не раскрывает raw pointers.
    fn visible_plane_block(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]>;
}

/// Простая owned-buffer реализация owner-а для тестов и будущих CPU-owned paths.
#[derive(Debug, Clone)]
pub struct HostPlanarOwnedBuffer {
    /// Общий immutable byte storage всего host frame-а.
    bytes: Arc<[u8]>,
}

impl HostPlanarOwnedBuffer {
    /// Создаёт owner поверх immutable byte storage без дополнительной копии.
    #[must_use]
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }
}

impl HostPlanarFrameOwner for HostPlanarOwnedBuffer {
    fn visible_row_bytes(
        &self,
        _plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        let row_index =
            usize::try_from(row_index).context("host-planar row index does not fit usize")?;
        let row_delta = plane
            .stride
            .checked_mul(row_index)
            .context("host-planar row stride overflow")?;
        let row_start = plane
            .offset
            .checked_add(row_delta)
            .context("host-planar row offset overflow")?;
        let row_end = row_start
            .checked_add(visible_row_bytes)
            .context("host-planar visible row end overflow")?;

        ensure!(
            row_end <= self.bytes.len(),
            "host-planar {:?} visible area exceeds storage: end {}, storage {}",
            plane.role,
            row_end,
            self.bytes.len()
        );

        Ok(&self.bytes[row_start..row_end])
    }

    fn visible_plane_block(
        &self,
        _plane_index: usize,
        plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        let block_end = plane
            .offset
            .checked_add(block_len)
            .context("host-planar plane block end overflow")?;

        ensure!(
            block_end <= self.bytes.len(),
            "host-planar {:?} plane block exceeds storage: end {}, storage {}",
            plane.role,
            block_end,
            self.bytes.len()
        );

        Ok(&self.bytes[plane.offset..block_end])
    }
}

/// Непрерывный видимый plane плюс stride для одного host->GPU upload-а.
#[derive(Debug)]
pub struct HostPlanarVisiblePlaneBlock<'a> {
    /// Срез backing-а owner-а, покрывающий все видимые строки plane-а.
    pub bytes: &'a [u8],

    /// Byte stride между началом соседних строк (`bytes_per_row` для upload-а).
    pub stride: usize,

    /// Видимые bytes одной строки без правого padding-а.
    pub visible_row_bytes: usize,

    /// Количество видимых строк plane-а.
    pub visible_height: u32,
}

/// Вычисляет число видимых bytes в строке plane-а с проверкой переполнения.
pub(super) fn host_plane_visible_row_bytes(plane: &HostPlaneDescriptor) -> anyhow::Result<usize> {
    ensure!(
        plane.visible_width > 0 && plane.visible_height > 0,
        "host-planar {:?} plane has invalid visible size {}x{}",
        plane.role,
        plane.visible_width,
        plane.visible_height
    );

    let visible_width = usize::try_from(plane.visible_width)
        .context("host-planar visible width does not fit usize")?;
    visible_width
        .checked_mul(plane.bytes_per_sample)
        .context("host-planar visible row byte count overflow")
}

/// Owned host-planar decoded frame без raw pointers и layout второго порядка.
#[derive(Debug, Clone)]
pub struct HostPlanarFrameDescriptor {
    /// Owner удерживает backing живым, пока существует descriptor или его clone.
    pub(super) owner: Arc<dyn HostPlanarFrameOwner>,

    /// Per-plane metadata; pixel layout и frame dimensions остаются в contract-е.
    pub planes: Vec<HostPlaneDescriptor>,
}

impl HostPlanarFrameDescriptor {
    /// Создаёт descriptor поверх provider-owned backing-а.
    #[must_use]
    pub fn new(owner: Arc<dyn HostPlanarFrameOwner>, planes: Vec<HostPlaneDescriptor>) -> Self {
        Self { owner, planes }
    }

    /// Создаёт descriptor поверх simple owned byte buffer-а.
    #[must_use]
    pub fn from_owned_buffer(
        bytes: impl Into<Arc<[u8]>>,
        planes: Vec<HostPlaneDescriptor>,
    ) -> Self {
        Self::new(Arc::new(HostPlanarOwnedBuffer::new(bytes)), planes)
    }

    /// Возвращает видимые bytes одной строки plane-а без padding-а.
    pub fn visible_plane_row_bytes(
        &self,
        plane_index: usize,
        row_index: u32,
    ) -> anyhow::Result<&[u8]> {
        let plane = self
            .planes
            .get(plane_index)
            .with_context(|| format!("host-planar plane index {plane_index} is out of bounds"))?;
        ensure!(
            row_index < plane.visible_height,
            "host-planar {:?} row {} is outside visible height {}",
            plane.role,
            row_index,
            plane.visible_height
        );

        let visible_row_bytes = host_plane_visible_row_bytes(plane)?;
        ensure!(
            plane.stride >= visible_row_bytes,
            "host-planar {:?} stride {} is smaller than visible row bytes {}",
            plane.role,
            plane.stride,
            visible_row_bytes
        );

        let row_bytes =
            self.owner
                .visible_row_bytes(plane_index, plane, row_index, visible_row_bytes)?;
        ensure!(
            row_bytes.len() == visible_row_bytes,
            "host-planar {:?} owner returned {} bytes for visible row, expected {}",
            plane.role,
            row_bytes.len(),
            visible_row_bytes
        );

        Ok(row_bytes)
    }

    /// Возвращает весь видимый plane одним непрерывным блоком плюс stride.
    ///
    /// Renderer использует это, чтобы залить plane одним host->GPU upload-ом
    /// (`bytes_per_row = stride`) вместо вызова на каждую строку. Промежуточной
    /// CPU-копии нет: блок — это срез backing-а owner-а.
    pub fn visible_plane_block(
        &self,
        plane_index: usize,
    ) -> anyhow::Result<HostPlanarVisiblePlaneBlock<'_>> {
        let plane = self
            .planes
            .get(plane_index)
            .with_context(|| format!("host-planar plane index {plane_index} is out of bounds"))?;

        let visible_row_bytes = host_plane_visible_row_bytes(plane)?;
        ensure!(
            plane.stride >= visible_row_bytes,
            "host-planar {:?} stride {} is smaller than visible row bytes {}",
            plane.role,
            plane.stride,
            visible_row_bytes
        );

        let visible_height = usize::try_from(plane.visible_height)
            .context("host-planar visible height does not fit usize")?;
        // block_len = (visible_height - 1) * stride + visible_row_bytes — ровно
        // столько байт требует wgpu::Queue::write_texture для копии height строк
        // со stride; visible_height > 0 гарантирован host_plane_visible_row_bytes.
        let block_len = plane
            .stride
            .checked_mul(visible_height - 1)
            .and_then(|rows| rows.checked_add(visible_row_bytes))
            .context("host-planar plane block length overflow")?;

        let bytes = self
            .owner
            .visible_plane_block(plane_index, plane, block_len)?;
        ensure!(
            bytes.len() == block_len,
            "host-planar {:?} owner returned {} bytes for plane block, expected {}",
            plane.role,
            bytes.len(),
            block_len
        );

        Ok(HostPlanarVisiblePlaneBlock {
            bytes,
            stride: plane.stride,
            visible_row_bytes,
            visible_height: plane.visible_height,
        })
    }

    /// Дешёвый clone для lookup-а: owner/refcount клонируется без копирования bytes.
    #[must_use]
    pub(super) fn clone_for_lookup(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
            planes: self.planes.clone(),
        }
    }
}

/// Layout exported DMA-BUF frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufFrameExportLayout {
    /// Один DRM layer описывает весь multi-planar image.
    ComposedLayers,

    /// Отдельные DRM layers описывают luma/chroma planes.
    SeparateLayers,
}

/// Kernel identity exported DMA-BUF object-а без transient fd number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaBufObjectIdentity {
    /// Device id из `fstat`.
    pub device: u64,

    /// Inode exported object-а из `fstat`.
    pub inode: u64,

    /// Special-device id из `fstat`, если kernel его заполняет.
    pub special_device: u64,
}

/// Один owned DMA-BUF object внутри DRM PRIME descriptor-а.
#[derive(Debug)]
pub struct DmaBufObjectDescriptor {
    /// Owned fd, который живёт вместе с descriptor-ом.
    pub fd: OwnedFd,

    /// Размер object-а в байтах.
    pub size: u32,

    /// DRM format modifier backing memory layout-а.
    pub drm_format_modifier: u64,

    /// Stable identity object-а для cache/reuse проверок.
    pub identity: DmaBufObjectIdentity,
}

impl DmaBufObjectDescriptor {
    /// Дублирует fd, сохраняя immutable metadata.
    fn try_clone_for_lookup(&self) -> io::Result<Self> {
        Ok(Self {
            fd: self.fd.as_fd().try_clone_to_owned()?,
            size: self.size,
            drm_format_modifier: self.drm_format_modifier,
            identity: self.identity,
        })
    }
}

/// Один DRM PRIME layer decoded frame-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufLayerDescriptor {
    /// DRM fourcc этого layer-а.
    pub drm_format: u32,

    /// Количество planes, описанных layer-ом.
    pub num_planes: u32,

    /// Индекс DMA-BUF object-а для каждой plane.
    pub object_index: [u8; 4],

    /// Byte offsets planes внутри objects.
    pub offset: [u32; 4],

    /// Row pitches planes в байтах.
    pub pitch: [u32; 4],
}

/// Нейтральный DMA-BUF/DRM PRIME descriptor decoded frame-а.
#[derive(Debug)]
pub struct DmaBufFrameDescriptor {
    /// Backend-stable decoded resource id; для VA-API это numeric surface id.
    pub resource_id: u64,

    /// Top-level DRM fourcc exported surface-а.
    pub fourcc: u32,

    /// Layout export-а.
    pub export_layout: DmaBufFrameExportLayout,

    /// Coded width imported image-а.
    pub width: u32,

    /// Coded height imported image-а.
    pub height: u32,

    /// Owned DMA-BUF objects.
    pub objects: Vec<DmaBufObjectDescriptor>,

    /// DRM PRIME layers.
    pub layers: Vec<DmaBufLayerDescriptor>,
}

/// Типизированная причина, по которой DMA-BUF descriptor нельзя передать import boundary.
///
/// Ошибка остаётся renderer-neutral: здесь нет Vulkan/WGPU типов, только нарушение
/// нейтрального descriptor contract-а. Это позволяет app-layer применить выбранную
/// `auto`/`hardware` policy до попытки создать Vulkan image/memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmaBufDescriptorRejection {
    /// Provider вернул descriptor без экспортированных DMA-BUF objects.
    AbsentObjects,
    /// Provider вернул descriptor без DRM PRIME layers.
    AbsentLayers,
    /// Layer объявил недопустимое число активных planes.
    InvalidPlaneCount {
        /// Индекс layer в descriptor-е.
        layer_index: usize,
        /// Объявленное число planes.
        plane_count: u32,
    },
    /// Plane ссылается на object, которого нет в descriptor-е.
    ObjectIndexOutOfBounds {
        /// Индекс layer в descriptor-е.
        layer_index: usize,
        /// Индекс plane внутри layer.
        plane_index: usize,
        /// Запрошенный индекс object-а.
        object_index: usize,
        /// Фактическое число objects.
        object_count: usize,
    },
    /// Один composed multi-planar image распределён по нескольким DMA-BUF objects.
    ///
    /// Текущий Vulkan importer поддерживает composed image только в одном object-е;
    /// separate-layer descriptor остаётся отдельным поддерживаемым contract-ом.
    UnsupportedComposedMultiObject {
        /// Первый object, используемый composed layer-ом.
        first_object_index: usize,
        /// Следующий plane ссылается уже на другой object.
        conflicting_object_index: usize,
    },
}

impl fmt::Display for DmaBufDescriptorRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AbsentObjects => formatter.write_str("DMA-BUF descriptor has no objects"),
            Self::AbsentLayers => formatter.write_str("DMA-BUF descriptor has no DRM PRIME layers"),
            Self::InvalidPlaneCount {
                layer_index,
                plane_count,
            } => write!(
                formatter,
                "DMA-BUF layer {layer_index} has invalid plane count {plane_count}; expected 1..=4"
            ),
            Self::ObjectIndexOutOfBounds {
                layer_index,
                plane_index,
                object_index,
                object_count,
            } => write!(
                formatter,
                "DMA-BUF layer {layer_index} plane {plane_index} references object {object_index}, but descriptor contains {object_count} objects"
            ),
            Self::UnsupportedComposedMultiObject {
                first_object_index,
                conflicting_object_index,
            } => write!(
                formatter,
                "composed multi-planar DMA-BUF spans objects {first_object_index} and {conflicting_object_index}; multi-object Vulkan import is unsupported"
            ),
        }
    }
}

impl std::error::Error for DmaBufDescriptorRejection {}

impl DmaBufFrameDescriptor {
    /// Дублирует все object fd для materializer lookup-а.
    pub fn try_clone_for_lookup(&self) -> io::Result<Self> {
        let objects = self
            .objects
            .iter()
            .map(DmaBufObjectDescriptor::try_clone_for_lookup)
            .collect::<io::Result<Vec<_>>>()?;

        Ok(Self {
            resource_id: self.resource_id,
            fourcc: self.fourcc,
            export_layout: self.export_layout,
            width: self.width,
            height: self.height,
            objects,
            layers: self.layers.clone(),
        })
    }
}
