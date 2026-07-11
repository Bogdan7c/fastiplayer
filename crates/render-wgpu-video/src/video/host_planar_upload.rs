use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use video_backend_api::{PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneRole, validate_resource_descriptor_against_contract,
};
use video_frame_contract::{
    FrameChromaSubsampling, VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath,
};

use super::{
    WgpuFrameMaterializationUnsupportedReason, WgpuFrameTextureViewLookup,
    WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
};

/// Renderer-side materializer, который загружает HostPlanar descriptors в WGPU textures.
pub struct HostPlanarWgpuFrameMaterializer {
    inner: HostPlanarFrameMaterializerCore<WgpuHostPlanarUploadBackend>,
}

impl HostPlanarWgpuFrameMaterializer {
    /// Создаёт materializer из WGPU handles renderer layer-а и neutral provider-а.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_provider: PresentFrameResourceProviderHandle,
    ) -> Self {
        Self {
            inner: HostPlanarFrameMaterializerCore::new(
                resource_provider,
                HostPlanarUploadTexturePool::new(WgpuHostPlanarUploadBackend::new(
                    device.clone(),
                    queue.clone(),
                )),
            ),
        }
    }

    /// Пытается получить renderer-owned HostPlanar texture views для decoded frame-а.
    ///
    /// Decoder/provider продолжает владеть host frame backing-ом до обычного
    /// release lease; pool ниже владеет только WGPU upload textures.
    pub fn try_host_planar_texture_view_lookup(
        &self,
        frame: &DecodedFrame,
    ) -> HostPlanarWgpuTextureViewLookup {
        HostPlanarWgpuTextureViewLookup::from_core_lookup(self.inner.try_upload_lookup(frame))
    }
}

impl WgpuFrameTextureViewMaterializer for HostPlanarWgpuFrameMaterializer {
    /// Подключает HostPlanar upload к общему app/render materializer trait-у.
    fn try_texture_view_lookup(&self, frame: &DecodedFrame) -> WgpuFrameTextureViewLookup {
        common_lookup_from_host_planar_lookup(self.try_host_planar_texture_view_lookup(frame))
    }

    fn recreate_for_renderer(
        &self,
        _instance: &wgpu::Instance,
        _adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Arc<dyn WgpuFrameTextureViewMaterializer> {
        Arc::new(Self::new(
            device,
            queue,
            self.inner.resource_provider.clone(),
        ))
    }
}

/// WGPU texture views, загруженные из HostPlanar Y/U/V planes.
#[derive(Clone)]
pub struct HostPlanarWgpuTextureViews {
    /// Texture view с luma/Y plane.
    pub y_view: wgpu::TextureView,

    /// Texture view с chroma U/Cb plane.
    pub u_view: wgpu::TextureView,

    /// Texture view с chroma V/Cr plane.
    pub v_view: wgpu::TextureView,

    /// Guard удерживает renderer-owned upload textures живыми до drop-а views.
    _uploaded_texture_guard: Arc<WgpuHostPlanarUploadedTextures>,
}

impl HostPlanarWgpuTextureViews {
    fn from_uploaded_textures(uploaded_textures: Arc<WgpuHostPlanarUploadedTextures>) -> Self {
        Self {
            y_view: uploaded_textures.y_view.clone(),
            u_view: uploaded_textures.u_view.clone(),
            v_view: uploaded_textures.v_view.clone(),
            _uploaded_texture_guard: uploaded_textures,
        }
    }
}

/// Результат HostPlanar upload materialization без участия playback core.
pub enum HostPlanarWgpuTextureViewLookup {
    /// Renderer получил texture views, которые принадлежат upload pool-у.
    Ready {
        /// Views трёх planar Y/U/V textures.
        views: Box<HostPlanarWgpuTextureViews>,

        /// Сколько render thread ждал lock texture pool-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend resource pool или renderer upload pool занят.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock resource/texture pool-а.
        texture_pool_lock_wait: Duration,
    },

    /// Descriptor существует, но этот materializer не умеет такой resource/layout.
    Unsupported {
        /// Техническая причина отказа materializer-а.
        reason: WgpuFrameMaterializationUnsupportedReason,

        /// Сколько render thread ждал lock resource/texture pool-а.
        texture_pool_lock_wait: Duration,
    },

    /// Provider или renderer upload path обнаружил fatal/materialization error.
    Error {
        /// Сколько render thread ждал lock resource/texture pool-а.
        texture_pool_lock_wait: Duration,
    },
}

impl HostPlanarWgpuTextureViewLookup {
    /// Возвращает lock wait sample независимо от lookup outcome.
    #[must_use]
    pub const fn texture_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                texture_pool_lock_wait,
                ..
            }
            | Self::Busy {
                texture_pool_lock_wait,
            }
            | Self::Missing {
                texture_pool_lock_wait,
            }
            | Self::Unsupported {
                texture_pool_lock_wait,
                ..
            }
            | Self::Error {
                texture_pool_lock_wait,
            } => *texture_pool_lock_wait,
        }
    }

    /// Возвращает `true`, если lookup не стал ждать занятый resource/texture lock.
    #[must_use]
    pub const fn lookup_was_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }

    fn from_core_lookup(
        lookup: HostPlanarUploadLookup<Arc<WgpuHostPlanarUploadedTextures>>,
    ) -> Self {
        match lookup {
            HostPlanarUploadLookup::Ready {
                uploaded_textures,
                texture_pool_lock_wait,
            } => Self::Ready {
                views: Box::new(HostPlanarWgpuTextureViews::from_uploaded_textures(
                    uploaded_textures,
                )),
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Busy {
                texture_pool_lock_wait,
            } => Self::Busy {
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Missing {
                texture_pool_lock_wait,
            } => Self::Missing {
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Unsupported {
                reason,
                texture_pool_lock_wait,
            } => Self::Unsupported {
                reason,
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Error {
                texture_pool_lock_wait,
            } => Self::Error {
                texture_pool_lock_wait,
            },
        }
    }
}

fn common_lookup_from_host_planar_lookup(
    lookup: HostPlanarWgpuTextureViewLookup,
) -> WgpuFrameTextureViewLookup {
    match lookup {
        HostPlanarWgpuTextureViewLookup::Ready {
            views,
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Ready {
            views: WgpuFrameTextureViews::from_host_planar_texture_views(views),
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Busy {
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Busy {
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Missing {
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Missing {
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Unsupported {
            reason,
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Unsupported {
            reason,
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Error {
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Error {
            texture_pool_lock_wait,
        },
    }
}

struct HostPlanarFrameMaterializerCore<B>
where
    B: HostPlanarUploadBackend,
{
    resource_provider: PresentFrameResourceProviderHandle,
    texture_pool: Mutex<HostPlanarUploadTexturePool<B>>,
}

impl<B> HostPlanarFrameMaterializerCore<B>
where
    B: HostPlanarUploadBackend,
{
    fn new(
        resource_provider: PresentFrameResourceProviderHandle,
        texture_pool: HostPlanarUploadTexturePool<B>,
    ) -> Self {
        Self {
            resource_provider,
            texture_pool: Mutex::new(texture_pool),
        }
    }

    fn try_upload_lookup(
        &self,
        frame: &DecodedFrame,
    ) -> HostPlanarUploadLookup<B::UploadedTextures> {
        let provider_lookup = self
            .resource_provider
            .try_resource_descriptor_lookup(frame.resource_handle);
        let provider_wait = provider_lookup.resource_pool_lock_wait();

        let descriptor = match provider_lookup {
            PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
            PresentFrameResourceDescriptorLookup::Busy { .. } => {
                return HostPlanarUploadLookup::Busy {
                    texture_pool_lock_wait: provider_wait,
                };
            }
            PresentFrameResourceDescriptorLookup::Missing { .. } => {
                return HostPlanarUploadLookup::Missing {
                    texture_pool_lock_wait: provider_wait,
                };
            }
            PresentFrameResourceDescriptorLookup::Fatal { .. } => {
                return HostPlanarUploadLookup::Error {
                    texture_pool_lock_wait: provider_wait,
                };
            }
        };

        let host_descriptor = match descriptor {
            FrameResourceDescriptor::HostPlanar(host_descriptor) => host_descriptor,
            FrameResourceDescriptor::DmaBuf(_) => {
                return HostPlanarUploadLookup::Unsupported {
                    reason:
                        WgpuFrameMaterializationUnsupportedReason::DmaBufRequiresDmaBufMaterializer,
                    texture_pool_lock_wait: provider_wait,
                };
            }
        };

        let pool_lock_started_at = Instant::now();
        let mut texture_pool = match self.texture_pool.try_lock() {
            Ok(texture_pool) => texture_pool,
            Err(TryLockError::WouldBlock) => {
                return HostPlanarUploadLookup::Busy {
                    texture_pool_lock_wait: provider_wait
                        .saturating_add(pool_lock_started_at.elapsed()),
                };
            }
            Err(TryLockError::Poisoned(error)) => {
                tracing::warn!(error = %error, "WGPU HostPlanar upload pool mutex poisoned");
                return HostPlanarUploadLookup::Error {
                    texture_pool_lock_wait: provider_wait
                        .saturating_add(pool_lock_started_at.elapsed()),
                };
            }
        };
        let total_lock_wait = provider_wait.saturating_add(pool_lock_started_at.elapsed());

        match texture_pool.materialize(frame, host_descriptor) {
            Ok(uploaded_textures) => HostPlanarUploadLookup::Ready {
                uploaded_textures,
                texture_pool_lock_wait: total_lock_wait,
            },
            Err(HostPlanarUploadFailure::Busy) => HostPlanarUploadLookup::Busy {
                texture_pool_lock_wait: total_lock_wait,
            },
            Err(HostPlanarUploadFailure::Unsupported(reason)) => {
                HostPlanarUploadLookup::Unsupported {
                    reason,
                    texture_pool_lock_wait: total_lock_wait,
                }
            }
            Err(HostPlanarUploadFailure::Error(error)) => host_planar_lookup_after_upload_failure(
                frame.resource_handle,
                error,
                total_lock_wait,
            ),
        }
    }
}

enum HostPlanarUploadLookup<T> {
    Ready {
        uploaded_textures: T,
        texture_pool_lock_wait: Duration,
    },
    Busy {
        texture_pool_lock_wait: Duration,
    },
    Missing {
        texture_pool_lock_wait: Duration,
    },
    Unsupported {
        reason: WgpuFrameMaterializationUnsupportedReason,
        texture_pool_lock_wait: Duration,
    },
    Error {
        texture_pool_lock_wait: Duration,
    },
}

struct HostPlanarUploadTexturePool<B>
where
    B: HostPlanarUploadBackend,
{
    backend: B,
    slots: VecDeque<HostPlanarUploadTextureSlot<B::UploadedTextures>>,
    capacity: usize,
}

impl<B> HostPlanarUploadTexturePool<B>
where
    B: HostPlanarUploadBackend,
{
    const DEFAULT_CAPACITY: usize = 24;

    /// Минимальное число резидентных slot-ов одного layout, которое pool держит
    /// перед тем, как начать переиспользовать idle slot. Даёт GPU pipeline запас:
    /// без него reuse схлопывается на 1–2 сета, и `write_texture` нового кадра
    /// попадает в текстуру, которую GPU ещё семплит из недавнего submitted draw
    /// (write-after-read hazard сериализует upload и draw, раздувая CPU/GPU).
    const MIN_REUSE_RING_SLOTS: usize = 4;

    fn new(backend: B) -> Self {
        Self {
            backend,
            slots: VecDeque::with_capacity(Self::DEFAULT_CAPACITY),
            capacity: Self::DEFAULT_CAPACITY,
        }
    }

    #[cfg(test)]
    fn with_capacity(backend: B, capacity: usize) -> Self {
        Self {
            backend,
            slots: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn materialize(
        &mut self,
        frame: &DecodedFrame,
        host_descriptor: HostPlanarFrameDescriptor,
    ) -> std::result::Result<B::UploadedTextures, HostPlanarUploadFailure> {
        let layout = host_planar_upload_layout(frame.frame_contract, frame.width, frame.height)
            .map_err(HostPlanarUploadFailure::Unsupported)?;

        let descriptor_for_validation =
            FrameResourceDescriptor::HostPlanar(host_descriptor.clone());
        validate_resource_descriptor_against_contract(
            frame.frame_contract,
            frame.width,
            frame.height,
            &descriptor_for_validation,
        )
        .map_err(HostPlanarUploadFailure::Error)?;

        // Same-handle lookup возвращает уже загруженные textures без повторного upload:
        // FrameResourceHandle здесь только identity decoded resource, а не ключ lifecycle pool-а.
        if let Some(slot) = self
            .slots
            .iter()
            .find(|slot| slot.current_handle == Some(frame.resource_handle))
        {
            if slot.layout != layout {
                return Err(HostPlanarUploadFailure::Error(anyhow!(
                    "host-planar upload pool handle {:?} is associated with a different layout",
                    frame.resource_handle
                )));
            }
            return Ok(slot.uploaded_textures.clone());
        }

        // Пока резидентных slot-ов этого layout меньше reuse-окна, растим кольцо
        // вместо немедленного reuse: иначе pool схлопывается на 1–2 сета и запись
        // нового кадра попадает в только что засабмиченную текстуру.
        if self.slot_count_with_layout(layout) < self.target_reuse_ring_slots()
            && self.slots.len() < self.capacity
        {
            return self.allocate_upload_and_store(frame.resource_handle, &host_descriptor, layout);
        }

        // Новый handle с тем же layout обязан перезалить planes, но может использовать
        // idle slot: старые pixels больше не наблюдаются render/app guard-ами.
        if let Some(slot_index) = self.idle_slot_with_layout(layout) {
            return self.upload_into_existing_slot(
                slot_index,
                frame.resource_handle,
                &host_descriptor,
                layout,
            );
        }

        // Idle slot нужного layout нет, но global capacity ещё свободна — создаём новый.
        if self.slots.len() < self.capacity {
            return self.allocate_upload_and_store(frame.resource_handle, &host_descriptor, layout);
        }

        // При полном pool-е разрешён eviction только idle slot-а другого layout.
        // Занятый slot не трогаем, чтобы Busy fallback не получил перезаписанную texture.
        if let Some(slot_index) = self.idle_slot_with_different_layout(layout) {
            self.slots.remove(slot_index);
            return self.allocate_upload_and_store(frame.resource_handle, &host_descriptor, layout);
        }

        Err(HostPlanarUploadFailure::Busy)
    }

    fn target_reuse_ring_slots(&self) -> usize {
        Self::MIN_REUSE_RING_SLOTS.min(self.capacity)
    }

    fn slot_count_with_layout(&self, layout: HostPlanarUploadLayout) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.layout == layout)
            .count()
    }

    fn idle_slot_with_layout(&self, layout: HostPlanarUploadLayout) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.layout == layout
                && self
                    .backend
                    .uploaded_textures_are_idle(&slot.uploaded_textures)
        })
    }

    fn idle_slot_with_different_layout(&self, layout: HostPlanarUploadLayout) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.layout != layout
                && self
                    .backend
                    .uploaded_textures_are_idle(&slot.uploaded_textures)
        })
    }

    fn upload_into_existing_slot(
        &mut self,
        slot_index: usize,
        handle: FrameResourceHandle,
        host_descriptor: &HostPlanarFrameDescriptor,
        layout: HostPlanarUploadLayout,
    ) -> std::result::Result<B::UploadedTextures, HostPlanarUploadFailure> {
        let uploaded_textures = {
            let slot = self.slots.get_mut(slot_index).ok_or_else(|| {
                HostPlanarUploadFailure::Error(anyhow!(
                    "selected host-planar upload pool slot disappeared"
                ))
            })?;
            if slot.layout != layout {
                return Err(HostPlanarUploadFailure::Error(anyhow!(
                    "selected host-planar upload pool slot layout changed"
                )));
            }

            // Пока идёт upload нового кадра, slot не должен отвечать ни за один handle:
            // при ошибке частично перезаписанные pixels нельзя вернуть как старый кадр.
            slot.current_handle = None;
            slot.uploaded_textures.clone()
        };

        self.upload_descriptor_into_textures(host_descriptor, layout, &uploaded_textures)?;

        // Ротация в конец очереди делает reuse LRU: front всегда самый давно
        // использованный idle slot, поэтому запись расходится по всему кольцу и
        // не возвращается к только что отрисованному сету раньше, чем GPU его дочитал.
        let mut slot = self.slots.remove(slot_index).ok_or_else(|| {
            HostPlanarUploadFailure::Error(anyhow!(
                "reused host-planar upload pool slot disappeared"
            ))
        })?;
        slot.current_handle = Some(handle);
        self.slots.push_back(slot);

        Ok(uploaded_textures)
    }

    fn allocate_upload_and_store(
        &mut self,
        handle: FrameResourceHandle,
        host_descriptor: &HostPlanarFrameDescriptor,
        layout: HostPlanarUploadLayout,
    ) -> std::result::Result<B::UploadedTextures, HostPlanarUploadFailure> {
        if self.capacity == 0 {
            return Err(HostPlanarUploadFailure::Busy);
        }

        let uploaded_textures = self
            .backend
            .allocate_textures(layout)
            .map_err(HostPlanarUploadFailure::Error)?;
        self.upload_descriptor_into_textures(host_descriptor, layout, &uploaded_textures)?;

        self.slots.push_back(HostPlanarUploadTextureSlot {
            layout,
            current_handle: Some(handle),
            uploaded_textures: uploaded_textures.clone(),
        });

        Ok(uploaded_textures)
    }

    fn upload_descriptor_into_textures(
        &mut self,
        host_descriptor: &HostPlanarFrameDescriptor,
        layout: HostPlanarUploadLayout,
        uploaded_textures: &B::UploadedTextures,
    ) -> std::result::Result<(), HostPlanarUploadFailure> {
        upload_host_planar_visible_rows(
            host_descriptor,
            layout,
            uploaded_textures,
            &mut self.backend,
        )
        .map_err(HostPlanarUploadFailure::Error)
    }
}

struct HostPlanarUploadTextureSlot<T> {
    /// Layout определяет, можно ли переиспользовать allocation без пересоздания textures.
    layout: HostPlanarUploadLayout,
    /// Handle текущих pixels; `None` значит slot idle/invalid во время перезаливки.
    current_handle: Option<FrameResourceHandle>,
    /// Renderer-owned upload textures, которые живут независимо от decoder resource.
    uploaded_textures: T,
}

#[derive(Debug)]
enum HostPlanarUploadFailure {
    Busy,
    Unsupported(WgpuFrameMaterializationUnsupportedReason),
    Error(anyhow::Error),
}

trait HostPlanarUploadBackend {
    type UploadedTextures: Clone;

    fn allocate_textures(
        &mut self,
        layout: HostPlanarUploadLayout,
    ) -> Result<Self::UploadedTextures>;

    fn upload_plane_block(
        &mut self,
        uploaded_textures: &Self::UploadedTextures,
        plane_index: usize,
        block_bytes: &[u8],
        stride: usize,
        visible_height: u32,
    ) -> Result<()>;

    /// Завершает накопленные plane uploads одного кадра (batch boundary).
    fn flush_plane_uploads(&mut self) -> Result<()> {
        Ok(())
    }

    /// Возвращает `true`, когда slot удерживает только pool и его textures можно перезаписать.
    fn uploaded_textures_are_idle(&self, uploaded_textures: &Self::UploadedTextures) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostPlanarUploadLayout {
    planes: [HostPlanarUploadPlaneLayout; 3],
}

impl HostPlanarUploadLayout {
    fn plane(self, plane_index: usize) -> Result<HostPlanarUploadPlaneLayout> {
        self.planes.get(plane_index).copied().with_context(|| {
            format!("host-planar upload plane index {plane_index} is out of bounds")
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostPlanarUploadPlaneLayout {
    role: HostPlaneRole,
    width: u32,
    height: u32,
    bytes_per_sample: usize,
    texture_format: HostPlanarUploadTextureFormat,
}

impl HostPlanarUploadPlaneLayout {
    fn visible_row_bytes(self) -> Result<usize> {
        let width = usize::try_from(self.width)
            .context("host-planar upload plane width does not fit usize")?;
        width
            .checked_mul(self.bytes_per_sample)
            .context("host-planar upload visible row byte count overflow")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlanarUploadTextureFormat {
    R8Unorm,
    R16Uint,
}

impl HostPlanarUploadTextureFormat {
    const fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            Self::R8Unorm => wgpu::TextureFormat::R8Unorm,
            Self::R16Uint => wgpu::TextureFormat::R16Uint,
        }
    }
}

struct WgpuHostPlanarUploadBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Копии plane→texture одного кадра батчатся в один encoder/submit.
    pending_upload_encoder: Option<wgpu::CommandEncoder>,
    /// Переиспользуемые mapped staging chunks: без per-frame allocate+zero-init.
    staging_belt: Option<wgpu::util::StagingBelt>,
}

impl WgpuHostPlanarUploadBackend {
    fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device,
            queue,
            pending_upload_encoder: None,
            staging_belt: None,
        }
    }
}

impl HostPlanarUploadBackend for WgpuHostPlanarUploadBackend {
    type UploadedTextures = Arc<WgpuHostPlanarUploadedTextures>;

    fn allocate_textures(
        &mut self,
        layout: HostPlanarUploadLayout,
    ) -> Result<Self::UploadedTextures> {
        Ok(Arc::new(WgpuHostPlanarUploadedTextures::new(
            &self.device,
            layout,
        )))
    }

    fn upload_plane_block(
        &mut self,
        uploaded_textures: &Self::UploadedTextures,
        plane_index: usize,
        block_bytes: &[u8],
        stride: usize,
        visible_height: u32,
    ) -> Result<()> {
        let plane_layout = uploaded_textures.layout.plane(plane_index)?;
        let texture = uploaded_textures.plane_texture(plane_index)?;
        let visible_row_bytes = plane_layout.visible_row_bytes()?;

        ensure!(
            visible_height == plane_layout.height,
            "host-planar {:?} upload block height {} does not match texture height {}",
            plane_layout.role,
            visible_height,
            plane_layout.height
        );
        ensure!(
            stride >= visible_row_bytes,
            "host-planar {:?} upload stride {} is smaller than visible row bytes {}",
            plane_layout.role,
            stride,
            visible_row_bytes
        );

        let expected_block_bytes = stride
            .checked_mul(plane_layout.height.saturating_sub(1) as usize)
            .and_then(|rows| rows.checked_add(visible_row_bytes))
            .context("host-planar upload block length overflow")?;
        ensure!(
            block_bytes.len() == expected_block_bytes,
            "host-planar {:?} upload block has {} bytes, expected {}",
            plane_layout.role,
            block_bytes.len(),
            expected_block_bytes
        );

        // Staging belt + copy_buffer_to_texture вместо Queue::write_texture: memcpy 4K
        // plane (8-12МБ) на memory-bandwidth-bound CPU под нагрузкой декодера стоил до
        // 15-30мс одним потоком внутри write_texture. Полосная копия в переиспользуемый
        // mapped chunk срезает хвост p99, а GPU-копии батчатся в один submit на кадр.
        // Belt (а не create_buffer(mapped_at_creation) на кадр) — иначе wgpu каждый раз
        // zero-инициализирует буфер, что дороже самой копии.
        let staging_stride = u32::try_from(visible_row_bytes)
            .ok()
            .map(|row_bytes| row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
            .context("host-planar staging row bytes do not fit u32")?;
        let staging_len = (staging_stride as u64)
            .checked_mul(u64::from(plane_layout.height.saturating_sub(1)))
            .and_then(|rows| rows.checked_add(visible_row_bytes as u64))
            .context("host-planar staging length overflow")?
            .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);

        let device = self.device.clone();
        let staging_belt = self.staging_belt.get_or_insert_with(|| {
            wgpu::util::StagingBelt::new(device, HOST_PLANAR_STAGING_BELT_CHUNK_BYTES)
        });
        let staging_size =
            wgpu::BufferSize::new(staging_len).context("host-planar staging length is zero")?;
        let staging_alignment =
            wgpu::BufferSize::new(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
                .expect("copy alignment is non-zero");
        let staging_slice = staging_belt.allocate(staging_size, staging_alignment);
        {
            let mut mapped = staging_slice.get_mapped_range_mut();
            copy_plane_block_into_staging(
                block_bytes,
                mapped.slice(..),
                stride,
                staging_stride as usize,
                visible_row_bytes,
                plane_layout.height as usize,
            );
        }

        if self.pending_upload_encoder.is_none() {
            self.pending_upload_encoder = Some(self.device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("host-planar-frame-upload"),
                },
            ));
        }
        let encoder = self
            .pending_upload_encoder
            .as_mut()
            .expect("pending upload encoder installed above");
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: staging_slice.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: staging_slice.offset(),
                    bytes_per_row: Some(staging_stride),
                    rows_per_image: None,
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: plane_layout.width,
                height: plane_layout.height,
                depth_or_array_layers: 1,
            },
        );

        Ok(())
    }

    fn uploaded_textures_are_idle(&self, uploaded_textures: &Self::UploadedTextures) -> bool {
        Arc::strong_count(uploaded_textures) == 1
    }

    fn flush_plane_uploads(&mut self) -> Result<()> {
        if let Some(encoder) = self.pending_upload_encoder.take() {
            if let Some(staging_belt) = self.staging_belt.as_mut() {
                staging_belt.finish();
            }
            self.queue.submit(std::iter::once(encoder.finish()));
            if let Some(staging_belt) = self.staging_belt.as_mut() {
                // Возврат chunk-ов в belt: map_async завершится на обычных device polls
                // render-цикла, к следующему кадру chunk снова доступен без zero-init.
                staging_belt.recall();
            }
        }
        Ok(())
    }
}

/// Размер chunk-а staging belt: вмещает все планы 4K-кадра одним chunk-ом.
const HOST_PLANAR_STAGING_BELT_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// Полоса меньше этого объёма копируется без отдельного потока.
const HOST_PLANAR_STAGING_COPY_BAND_MIN_BYTES: usize = 1024 * 1024;

/// Верхний предел потоков полосной staging-копии одного plane.
const HOST_PLANAR_STAGING_COPY_MAX_THREADS: usize = 4;

/// Дизъюнктная полоса mapped staging, передаваемая scoped copy-потоку.
///
/// # Safety-обоснование
/// `WriteOnly<'_, [u8]>` указывает в host-mapped память staging buffer-а; полосы
/// получены через `split_at` и не пересекаются, а запись mapped-байтов в wgpu не
/// привязана к конкретному потоку (mapping/unmap остаются на вызывающем потоке).
/// `Send` у `WriteOnly<[u8]>` отсутствует только из-за `Sized`-bound generic-а.
struct StagingCopyBand<'a>(wgpu::WriteOnly<'a, [u8]>);

// SAFETY: полосы дизъюнктны и указывают в host-mapped память; см. док выше.
unsafe impl Send for StagingCopyBand<'_> {}

/// Копирует plane block в mapped staging полосами в несколько потоков.
///
/// Одиночный memcpy 4K-плоскости упирается в memory bandwidth и под нагрузкой
/// software-декодера растягивается до десятков миллисекунд; полосы делят копию
/// между ядрами и держат стадию upload в бюджете кадра.
fn copy_plane_block_into_staging(
    block_bytes: &[u8],
    staging: wgpu::WriteOnly<'_, [u8]>,
    src_stride: usize,
    dst_stride: usize,
    visible_row_bytes: usize,
    visible_height: usize,
) {
    if visible_height == 0 || visible_row_bytes == 0 {
        return;
    }

    let total_bytes = visible_row_bytes.saturating_mul(visible_height);
    let band_count = (total_bytes / HOST_PLANAR_STAGING_COPY_BAND_MIN_BYTES)
        .clamp(1, HOST_PLANAR_STAGING_COPY_MAX_THREADS)
        .min(visible_height);
    let rows_per_band = visible_height.div_ceil(band_count);

    std::thread::scope(|scope| {
        let mut staging_rest = staging;
        let mut row_start = 0usize;
        while row_start < visible_height {
            let band_rows = rows_per_band.min(visible_height - row_start);
            let is_last_band = row_start + band_rows == visible_height;
            let dst_band;
            if is_last_band {
                dst_band = staging_rest;
                staging_rest = wgpu::WriteOnly::from_mut(&mut []);
            } else {
                let (band, rest) = staging_rest.split_at(band_rows * dst_stride);
                dst_band = band;
                staging_rest = rest;
            }

            let base_row = row_start;
            let dst_band = StagingCopyBand(dst_band);
            let copy_band = move || {
                // Перенос всей обёртки одним place-выражением: precise capture поля .0
                // обошёл бы Send impl обёртки (деструктуризация в pattern не спасает).
                let band = dst_band;
                let StagingCopyBand(mut dst_band) = band;
                for row in 0..band_rows {
                    let src_offset = (base_row + row) * src_stride;
                    let dst_offset = row * dst_stride;
                    dst_band
                        .slice(dst_offset..dst_offset + visible_row_bytes)
                        .copy_from_slice(&block_bytes[src_offset..src_offset + visible_row_bytes]);
                }
            };
            if is_last_band {
                // Последняя полоса на текущем потоке: scope join не ждёт лишний spawn.
                copy_band();
            } else {
                scope.spawn(copy_band);
            }

            row_start += band_rows;
        }
    });
}

struct WgpuHostPlanarUploadedTextures {
    layout: HostPlanarUploadLayout,
    y_texture: wgpu::Texture,
    u_texture: wgpu::Texture,
    v_texture: wgpu::Texture,
    y_view: wgpu::TextureView,
    u_view: wgpu::TextureView,
    v_view: wgpu::TextureView,
}

impl WgpuHostPlanarUploadedTextures {
    fn new(device: &wgpu::Device, layout: HostPlanarUploadLayout) -> Self {
        let y_texture =
            create_upload_plane_texture(device, "host planar Y upload texture", layout.planes[0]);
        let u_texture =
            create_upload_plane_texture(device, "host planar U upload texture", layout.planes[1]);
        let v_texture =
            create_upload_plane_texture(device, "host planar V upload texture", layout.planes[2]);
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar Y upload texture view"),
            ..Default::default()
        });
        let u_view = u_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar U upload texture view"),
            ..Default::default()
        });
        let v_view = v_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar V upload texture view"),
            ..Default::default()
        });

        Self {
            layout,
            y_texture,
            u_texture,
            v_texture,
            y_view,
            u_view,
            v_view,
        }
    }

    fn plane_texture(&self, plane_index: usize) -> Result<&wgpu::Texture> {
        match plane_index {
            0 => Ok(&self.y_texture),
            1 => Ok(&self.u_texture),
            2 => Ok(&self.v_texture),
            _ => Err(anyhow!(
                "host-planar upload plane index {plane_index} is out of bounds"
            )),
        }
    }
}

fn create_upload_plane_texture(
    device: &wgpu::Device,
    label: &'static str,
    plane_layout: HostPlanarUploadPlaneLayout,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: plane_layout.width,
            height: plane_layout.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: plane_layout.texture_format.wgpu_format(),
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn upload_host_planar_visible_rows<B>(
    descriptor: &HostPlanarFrameDescriptor,
    layout: HostPlanarUploadLayout,
    uploaded_textures: &B::UploadedTextures,
    backend: &mut B,
) -> Result<()>
where
    B: HostPlanarUploadBackend,
{
    for (plane_index, plane_layout) in layout.planes.into_iter().enumerate() {
        upload_host_planar_plane_visible_rows(
            descriptor,
            plane_index,
            plane_layout,
            uploaded_textures,
            backend,
        )?;
    }

    backend.flush_plane_uploads()
}

fn upload_host_planar_plane_visible_rows<B>(
    descriptor: &HostPlanarFrameDescriptor,
    plane_index: usize,
    plane_layout: HostPlanarUploadPlaneLayout,
    uploaded_textures: &B::UploadedTextures,
    backend: &mut B,
) -> Result<()>
where
    B: HostPlanarUploadBackend,
{
    let descriptor_plane = descriptor
        .planes
        .get(plane_index)
        .with_context(|| format!("host-planar descriptor plane index {plane_index} is missing"))?;

    ensure!(
        descriptor_plane.role == plane_layout.role,
        "host-planar upload plane {plane_index} has role {:?}, expected {:?}",
        descriptor_plane.role,
        plane_layout.role
    );
    ensure!(
        descriptor_plane.visible_width == plane_layout.width
            && descriptor_plane.visible_height == plane_layout.height
            && descriptor_plane.bytes_per_sample == plane_layout.bytes_per_sample,
        "host-planar {:?} descriptor metadata does not match upload layout",
        descriptor_plane.role
    );

    let expected_row_bytes = plane_layout.visible_row_bytes()?;
    let block = descriptor
        .visible_plane_block(plane_index)
        .with_context(|| {
            format!(
                "failed to read visible plane block for host-planar {:?} upload",
                plane_layout.role
            )
        })?;
    ensure!(
        block.visible_row_bytes == expected_row_bytes,
        "host-planar {:?} visible row has {} bytes, expected {}",
        plane_layout.role,
        block.visible_row_bytes,
        expected_row_bytes
    );
    ensure!(
        block.visible_height == plane_layout.height,
        "host-planar {:?} visible height {} does not match upload height {}",
        plane_layout.role,
        block.visible_height,
        plane_layout.height
    );

    backend.upload_plane_block(
        uploaded_textures,
        plane_index,
        block.bytes,
        block.stride,
        block.visible_height,
    )?;

    Ok(())
}

fn host_planar_upload_layout(
    frame_contract: VideoFrameContract,
    coded_width: u32,
    coded_height: u32,
) -> std::result::Result<HostPlanarUploadLayout, WgpuFrameMaterializationUnsupportedReason> {
    if frame_contract.transfer_path != VideoFrameTransferPath::SoftwareHostUpload {
        return Err(
            WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresSoftwareUploadContract,
        );
    }

    let Some(chroma_subsampling) = frame_contract.pixel_layout.chroma() else {
        return Err(
            WgpuFrameMaterializationUnsupportedReason::HostPlanarLayoutNotSupportedByUploadMaterializer,
        );
    };

    match frame_contract.pixel_layout {
        VideoFramePixelLayout::Yuv420Planar8
        | VideoFramePixelLayout::Yuv422Planar8
        | VideoFramePixelLayout::Yuv444Planar8 => Ok(planar_yuv_upload_layout(
            coded_width,
            coded_height,
            chroma_subsampling,
            1,
            HostPlanarUploadTextureFormat::R8Unorm,
        )),
        VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv420Planar12Le
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv422Planar12Le
        | VideoFramePixelLayout::Yuv444Planar10Le => Ok(planar_yuv_upload_layout(
            coded_width,
            coded_height,
            chroma_subsampling,
            2,
            HostPlanarUploadTextureFormat::R16Uint,
        )),
        _ => Err(
            WgpuFrameMaterializationUnsupportedReason::HostPlanarLayoutNotSupportedByUploadMaterializer,
        ),
    }
}

fn planar_yuv_upload_layout(
    coded_width: u32,
    coded_height: u32,
    chroma_subsampling: FrameChromaSubsampling,
    bytes_per_sample: usize,
    texture_format: HostPlanarUploadTextureFormat,
) -> HostPlanarUploadLayout {
    let (chroma_width, chroma_height) =
        chroma_plane_dimensions(coded_width, coded_height, chroma_subsampling);

    HostPlanarUploadLayout {
        planes: [
            HostPlanarUploadPlaneLayout {
                role: HostPlaneRole::Luma,
                width: coded_width,
                height: coded_height,
                bytes_per_sample,
                texture_format,
            },
            HostPlanarUploadPlaneLayout {
                role: HostPlaneRole::ChromaU,
                width: chroma_width,
                height: chroma_height,
                bytes_per_sample,
                texture_format,
            },
            HostPlanarUploadPlaneLayout {
                role: HostPlaneRole::ChromaV,
                width: chroma_width,
                height: chroma_height,
                bytes_per_sample,
                texture_format,
            },
        ],
    }
}

fn chroma_plane_dimensions(
    coded_width: u32,
    coded_height: u32,
    chroma_subsampling: FrameChromaSubsampling,
) -> (u32, u32) {
    // В host-planar descriptor padding не является пикселями, поэтому размеры
    // chroma plane считаются только из видимой coded области.
    match chroma_subsampling {
        FrameChromaSubsampling::Yuv420 => {
            (half_rounded_up(coded_width), half_rounded_up(coded_height))
        }
        FrameChromaSubsampling::Yuv422 => (half_rounded_up(coded_width), coded_height),
        FrameChromaSubsampling::Yuv444 => (coded_width, coded_height),
    }
}

const fn half_rounded_up(value: u32) -> u32 {
    value.div_ceil(2)
}

fn host_planar_lookup_after_upload_failure<T>(
    handle: FrameResourceHandle,
    error: anyhow::Error,
    texture_pool_lock_wait: Duration,
) -> HostPlanarUploadLookup<T> {
    tracing::warn!(
        error = %error,
        handle_id = handle.0,
        "Renderer HostPlanar upload failed; CPU conversion fallback is disabled"
    );
    HostPlanarUploadLookup::Error {
        texture_pool_lock_wait,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use codec_core::VideoColorMetadata;
    use video_backend_api::{PresentFrameResourceProvider, PresentFrameResourceProviderLookup};
    use video_core::{
        DmaBufFrameDescriptor, DmaBufFrameExportLayout, HostPlanarFrameDescriptor,
        HostPlaneDescriptor,
    };
    use video_frame_contract::VideoFrameContract;

    use super::*;

    #[test]
    fn host_planar_materializer_rejects_dma_buf_descriptor() {
        let provider = RecordingDescriptorProvider::new_dma_buf();
        let materializer = HostPlanarFrameMaterializerCore::new(
            provider.handle(),
            HostPlanarUploadTexturePool::new(RecordingUploadBackend::default()),
        );

        let lookup = materializer
            .try_upload_lookup(&decoded_host_planar8_test_frame(FrameResourceHandle(11)));

        assert!(matches!(
            lookup,
            HostPlanarUploadLookup::Unsupported {
                reason: WgpuFrameMaterializationUnsupportedReason::DmaBufRequiresDmaBufMaterializer,
                ..
            }
        ));
        assert!(provider.released_handles().is_empty());
    }

    #[test]
    fn upload_path_copies_visible_bytes_only_with_padded_stride() {
        let descriptor = padded_host_planar_descriptor();
        let mut pool = HostPlanarUploadTexturePool::new(RecordingUploadBackend::default());
        let frame = decoded_host_planar8_test_frame(FrameResourceHandle(21));

        let uploaded_textures = pool
            .materialize(&frame, descriptor)
            .expect("host-planar upload should accept padded visible rows");

        assert_eq!(
            uploaded_textures.rows_for_plane(0),
            vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]
        );
        assert_eq!(uploaded_textures.rows_for_plane(1), vec![vec![9, 10]]);
        assert_eq!(uploaded_textures.rows_for_plane(2), vec![vec![11, 12]]);
    }

    #[test]
    fn same_layout_new_handles_reuse_idle_slot_and_upload_again() {
        let backend = RecordingUploadBackend::default();
        let allocation_count = backend.allocation_counter();
        let upload_call_count = backend.upload_call_counter();
        let mut pool = HostPlanarUploadTexturePool::with_capacity(backend, 1);

        let first_frame = decoded_host_planar8_test_frame(FrameResourceHandle(31));
        let first_upload = pool
            .materialize(&first_frame, padded_host_planar_descriptor())
            .expect("first host-planar upload should allocate one slot");
        drop(first_upload);

        let second_frame = decoded_host_planar8_test_frame(FrameResourceHandle(32));
        let second_upload = pool
            .materialize(&second_frame, padded_host_planar_descriptor())
            .expect("idle same-layout slot should be reused for a new handle");

        assert_eq!(allocation_count.load(Ordering::SeqCst), 1);
        assert_eq!(upload_call_count.load(Ordering::SeqCst), 6);
        assert_eq!(
            second_upload.rows_for_plane(0),
            vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]
        );
    }

    #[test]
    fn same_handle_lookup_returns_pool_slot_without_duplicate_upload() {
        let backend = RecordingUploadBackend::default();
        let allocation_count = backend.allocation_counter();
        let upload_call_count = backend.upload_call_counter();
        let mut pool = HostPlanarUploadTexturePool::with_capacity(backend, 1);

        let frame = decoded_host_planar8_test_frame(FrameResourceHandle(41));
        let first_upload = pool
            .materialize(&frame, padded_host_planar_descriptor())
            .expect("first host-planar upload should allocate one slot");
        let second_upload = pool
            .materialize(&frame, padded_host_planar_descriptor())
            .expect("same handle should return the already-uploaded slot");

        assert!(first_upload.same_slot_as(&second_upload));
        assert_eq!(allocation_count.load(Ordering::SeqCst), 1);
        assert_eq!(upload_call_count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn busy_slot_is_not_reused_for_another_handle() {
        let backend = RecordingUploadBackend::default();
        let allocation_count = backend.allocation_counter();
        let upload_call_count = backend.upload_call_counter();
        let mut pool = HostPlanarUploadTexturePool::with_capacity(backend, 2);

        let first_frame = decoded_host_planar8_test_frame(FrameResourceHandle(51));
        let first_upload = pool
            .materialize(&first_frame, padded_host_planar_descriptor())
            .expect("first host-planar upload should allocate one slot");

        let second_frame = decoded_host_planar8_test_frame(FrameResourceHandle(52));
        let second_upload = pool
            .materialize(&second_frame, padded_host_planar_descriptor())
            .expect("busy first slot should force allocation of another slot");

        assert!(!first_upload.same_slot_as(&second_upload));
        assert_eq!(allocation_count.load(Ordering::SeqCst), 2);
        assert_eq!(upload_call_count.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn capacity_full_with_no_idle_slots_returns_busy_without_provider_release() {
        let provider = RecordingDescriptorProvider::new_host_planar();
        let backend = RecordingUploadBackend::default();
        let materializer = HostPlanarFrameMaterializerCore::new(
            provider.handle(),
            HostPlanarUploadTexturePool::with_capacity(backend, 1),
        );

        let first_frame = decoded_host_planar8_test_frame(FrameResourceHandle(61));
        let first_upload = match materializer.try_upload_lookup(&first_frame) {
            HostPlanarUploadLookup::Ready {
                uploaded_textures, ..
            } => uploaded_textures,
            _ => panic!("first host-planar upload should be ready"),
        };

        let second_frame = decoded_host_planar8_test_frame(FrameResourceHandle(62));
        assert!(matches!(
            materializer.try_upload_lookup(&second_frame),
            HostPlanarUploadLookup::Busy { .. }
        ));
        assert_eq!(
            provider.released_handles(),
            Vec::<FrameResourceHandle>::new()
        );

        drop(first_upload);
    }

    #[test]
    fn upload_pool_evicts_idle_textures_without_bypassing_provider_release() {
        let provider = RecordingDescriptorProvider::new_host_planar();
        let drop_count = Arc::new(AtomicUsize::new(0));
        let backend = RecordingUploadBackend::with_drop_counter(Arc::clone(&drop_count));
        let materializer = HostPlanarFrameMaterializerCore::new(
            provider.handle(),
            HostPlanarUploadTexturePool::with_capacity(backend, 1),
        );

        let first_handle = FrameResourceHandle(71);
        let second_handle = FrameResourceHandle(72);
        let first_frame = decoded_host_planar8_test_frame(first_handle);
        let second_frame = decoded_host_planar444_8_test_frame(second_handle);

        assert!(matches!(
            materializer.try_upload_lookup(&first_frame),
            HostPlanarUploadLookup::Ready { .. }
        ));
        provider.set_descriptor_kind(RecordingDescriptorKind::HostPlanarYuv444);
        assert!(matches!(
            materializer.try_upload_lookup(&second_frame),
            HostPlanarUploadLookup::Ready { .. }
        ));

        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
        assert!(provider.released_handles().is_empty());

        provider.handle().release_frame(first_handle);

        assert_eq!(provider.released_handles(), vec![first_handle]);
    }

    #[test]
    fn yuv420_upload_layout_uses_ceil_chroma_dimensions_for_all_ready_bit_depths() {
        let layout = host_planar_upload_layout(VideoFrameContract::host_yuv420_planar8(), 5, 3)
            .expect("YUV420 planar 8-bit is renderable");

        assert_upload_plane_layout(
            layout,
            [(5, 3), (3, 2), (3, 2)],
            1,
            HostPlanarUploadTextureFormat::R8Unorm,
        );

        for contract in [
            VideoFrameContract::host_yuv420_planar10le(),
            VideoFrameContract::host_yuv420_planar12le(),
        ] {
            let layout =
                host_planar_upload_layout(contract, 5, 3).expect("YUV420 16-bit words renderable");

            assert_upload_plane_layout(
                layout,
                [(5, 3), (3, 2), (3, 2)],
                2,
                HostPlanarUploadTextureFormat::R16Uint,
            );
        }
    }

    #[test]
    fn yuv422_upload_layout_uses_ceil_chroma_width_and_full_height() {
        for (coded_width, expected_chroma_width) in [(4, 2), (5, 3)] {
            let layout = host_planar_upload_layout(
                VideoFrameContract {
                    pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
                coded_width,
                3,
            )
            .expect("YUV422 planar 8-bit is renderable");

            assert_upload_plane_layout(
                layout,
                [
                    (coded_width, 3),
                    (expected_chroma_width, 3),
                    (expected_chroma_width, 3),
                ],
                1,
                HostPlanarUploadTextureFormat::R8Unorm,
            );
        }

        for pixel_layout in [
            VideoFramePixelLayout::Yuv422Planar10Le,
            VideoFramePixelLayout::Yuv422Planar12Le,
        ] {
            let layout = host_planar_upload_layout(
                VideoFrameContract {
                    pixel_layout,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
                5,
                3,
            )
            .expect("YUV422 16-bit words are renderable");

            assert_upload_plane_layout(
                layout,
                [(5, 3), (3, 3), (3, 3)],
                2,
                HostPlanarUploadTextureFormat::R16Uint,
            );
        }
    }

    #[test]
    fn yuv444_upload_layout_keeps_chroma_planes_full_size() {
        for (pixel_layout, bytes_per_sample, texture_format) in [
            (
                VideoFramePixelLayout::Yuv444Planar8,
                1,
                HostPlanarUploadTextureFormat::R8Unorm,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar10Le,
                2,
                HostPlanarUploadTextureFormat::R16Uint,
            ),
        ] {
            let layout = host_planar_upload_layout(
                VideoFrameContract {
                    pixel_layout,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
                5,
                3,
            )
            .expect("YUV444 v1 layout is renderable");

            assert_upload_plane_layout(
                layout,
                [(5, 3), (5, 3), (5, 3)],
                bytes_per_sample,
                texture_format,
            );
        }
    }

    fn assert_upload_plane_layout(
        layout: HostPlanarUploadLayout,
        expected_dimensions: [(u32, u32); 3],
        expected_bytes_per_sample: usize,
        expected_texture_format: HostPlanarUploadTextureFormat,
    ) {
        for (plane_layout, (expected_width, expected_height)) in
            layout.planes.into_iter().zip(expected_dimensions)
        {
            assert_eq!(plane_layout.width, expected_width);
            assert_eq!(plane_layout.height, expected_height);
            assert_eq!(plane_layout.bytes_per_sample, expected_bytes_per_sample);
            assert_eq!(plane_layout.texture_format, expected_texture_format);
        }
    }

    #[derive(Clone)]
    struct RecordingUploadedTextures {
        planes: Arc<Mutex<[Vec<Vec<u8>>; 3]>>,
        _drop_probe: Option<Arc<DropProbe>>,
    }

    impl RecordingUploadedTextures {
        fn rows_for_plane(&self, plane_index: usize) -> Vec<Vec<u8>> {
            self.planes.lock().expect("recorded rows lock poisoned")[plane_index].clone()
        }

        fn same_slot_as(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.planes, &other.planes)
        }
    }

    #[derive(Default)]
    struct RecordingUploadBackend {
        drop_count: Option<Arc<AtomicUsize>>,
        allocation_count: Arc<AtomicUsize>,
        upload_call_count: Arc<AtomicUsize>,
    }

    impl RecordingUploadBackend {
        fn with_drop_counter(drop_count: Arc<AtomicUsize>) -> Self {
            Self {
                drop_count: Some(drop_count),
                ..Self::default()
            }
        }

        fn allocation_counter(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.allocation_count)
        }

        fn upload_call_counter(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.upload_call_count)
        }
    }

    impl HostPlanarUploadBackend for RecordingUploadBackend {
        type UploadedTextures = RecordingUploadedTextures;

        fn allocate_textures(
            &mut self,
            _layout: HostPlanarUploadLayout,
        ) -> Result<Self::UploadedTextures> {
            self.allocation_count.fetch_add(1, Ordering::SeqCst);
            Ok(RecordingUploadedTextures {
                planes: Arc::new(Mutex::new([Vec::new(), Vec::new(), Vec::new()])),
                _drop_probe: self
                    .drop_count
                    .as_ref()
                    .map(|drop_count| Arc::new(DropProbe(Arc::clone(drop_count)))),
            })
        }

        fn upload_plane_block(
            &mut self,
            uploaded_textures: &Self::UploadedTextures,
            plane_index: usize,
            block_bytes: &[u8],
            stride: usize,
            visible_height: u32,
        ) -> Result<()> {
            self.upload_call_count.fetch_add(1, Ordering::SeqCst);
            let visible_height = visible_height as usize;
            // Восстанавливаем видимые строки из блока, чтобы тесты по-прежнему
            // проверяли, что upload адресует только visible bytes (без padding-а).
            let visible_row_bytes = block_bytes
                .len()
                .checked_sub(stride * visible_height.saturating_sub(1))
                .expect("recorded block shorter than stride layout");
            let mut rows = Vec::with_capacity(visible_height);
            for row_index in 0..visible_height {
                let start = row_index * stride;
                rows.push(block_bytes[start..start + visible_row_bytes].to_vec());
            }
            uploaded_textures
                .planes
                .lock()
                .expect("recorded rows lock poisoned")[plane_index] = rows;
            Ok(())
        }

        fn uploaded_textures_are_idle(&self, uploaded_textures: &Self::UploadedTextures) -> bool {
            Arc::strong_count(&uploaded_textures.planes) == 1
        }
    }

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct RecordingDescriptorProvider {
        descriptor_kind: Arc<Mutex<RecordingDescriptorKind>>,
        released_handles: Arc<Mutex<Vec<FrameResourceHandle>>>,
    }

    impl RecordingDescriptorProvider {
        fn new_host_planar() -> Self {
            Self {
                descriptor_kind: Arc::new(Mutex::new(RecordingDescriptorKind::HostPlanarYuv420)),
                released_handles: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn new_dma_buf() -> Self {
            Self {
                descriptor_kind: Arc::new(Mutex::new(RecordingDescriptorKind::DmaBuf)),
                released_handles: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn handle(&self) -> PresentFrameResourceProviderHandle {
            PresentFrameResourceProviderHandle::new(self.clone())
        }

        fn released_handles(&self) -> Vec<FrameResourceHandle> {
            self.released_handles
                .lock()
                .expect("released handles lock poisoned")
                .clone()
        }

        fn set_descriptor_kind(&self, descriptor_kind: RecordingDescriptorKind) {
            *self
                .descriptor_kind
                .lock()
                .expect("descriptor kind lock poisoned") = descriptor_kind;
        }

        fn descriptor(&self) -> FrameResourceDescriptor {
            match *self
                .descriptor_kind
                .lock()
                .expect("descriptor kind lock poisoned")
            {
                RecordingDescriptorKind::HostPlanarYuv420 => {
                    FrameResourceDescriptor::HostPlanar(padded_host_planar_descriptor())
                }
                RecordingDescriptorKind::HostPlanarYuv444 => {
                    FrameResourceDescriptor::HostPlanar(host_planar444_descriptor())
                }
                RecordingDescriptorKind::DmaBuf => {
                    FrameResourceDescriptor::DmaBuf(DmaBufFrameDescriptor {
                        resource_id: 9,
                        fourcc: 0,
                        export_layout: DmaBufFrameExportLayout::SeparateLayers,
                        width: 4,
                        height: 2,
                        objects: Vec::new(),
                        layers: Vec::new(),
                    })
                }
            }
        }
    }

    #[derive(Clone, Copy)]
    enum RecordingDescriptorKind {
        HostPlanarYuv420,
        HostPlanarYuv444,
        DmaBuf,
    }

    impl PresentFrameResourceProvider for RecordingDescriptorProvider {
        fn resource_lookup(
            &self,
            _handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::from_millis(1),
            }
        }

        fn try_resource_descriptor_lookup(
            &self,
            _handle: FrameResourceHandle,
        ) -> PresentFrameResourceDescriptorLookup {
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: self.descriptor(),
                resource_pool_lock_wait: Duration::from_millis(1),
            }
        }

        fn release_frame(&self, handle: FrameResourceHandle) {
            self.released_handles
                .lock()
                .expect("released handles lock poisoned")
                .push(handle);
        }
    }

    fn padded_host_planar_descriptor() -> HostPlanarFrameDescriptor {
        HostPlanarFrameDescriptor::from_owned_buffer(
            vec![
                1, 2, 3, 4, 99, 98, 5, 6, 7, 8, 97, 96, 9, 10, 95, 11, 12, 94,
            ],
            vec![
                HostPlaneDescriptor {
                    role: HostPlaneRole::Luma,
                    offset: 0,
                    stride: 6,
                    visible_width: 4,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaU,
                    offset: 12,
                    stride: 3,
                    visible_width: 2,
                    visible_height: 1,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaV,
                    offset: 15,
                    stride: 3,
                    visible_width: 2,
                    visible_height: 1,
                    bytes_per_sample: 1,
                },
            ],
        )
    }

    fn host_planar444_descriptor() -> HostPlanarFrameDescriptor {
        HostPlanarFrameDescriptor::from_owned_buffer(
            vec![
                1, 2, 3, 4, 5, 6, 7, 8, 21, 22, 23, 24, 25, 26, 27, 28, 41, 42, 43, 44, 45, 46, 47,
                48,
            ],
            vec![
                HostPlaneDescriptor {
                    role: HostPlaneRole::Luma,
                    offset: 0,
                    stride: 4,
                    visible_width: 4,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaU,
                    offset: 8,
                    stride: 4,
                    visible_width: 4,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaV,
                    offset: 16,
                    stride: 4,
                    visible_width: 4,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
            ],
        )
    }

    fn decoded_host_planar8_test_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 4,
            height: 2,
            render_width: 4,
            render_height: 2,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    fn decoded_host_planar444_8_test_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract {
                pixel_layout: VideoFramePixelLayout::Yuv444Planar8,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            },
            width: 4,
            height: 2,
            render_width: 4,
            render_height: 2,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }
}
