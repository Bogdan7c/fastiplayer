//! Codec-neutral lifecycle decoded surface для zero-copy playback.
//!
//! Единый порядок владения:
//! 1. decoder получает decoded surface из backend frame pool;
//! 2. VA-API экспортирует surface как DMA-BUF/DRM PRIME descriptor;
//! 3. renderer backend создаёт или переиспользует persistent external import;
//! 4. frame lease публикуется renderer-у через opaque [`FrameResourceHandle`];
//! 5. renderer submits GPU work и затем отпускает lease;
//! 6. renderer queue callback подтверждает GPU completion;
//! 7. decoder guard отпускается, surface возвращается в decoder pool;
//! 8. import slot становится free: backend contract решает reuse или replace.

use std::collections::HashMap;
use std::fmt;
use std::os::fd::AsRawFd;

use cros_codecs::decoder::{DecodedDmaBufExportLayout, DecodedDmaBufImage};
use nix::sys::stat::fstat;
use video_core::FrameResourceHandle;

/// Явный backend contract для persistent zero-copy imports.
///
/// Этот контракт отделяет production policy от env-экспериментов: persistent
/// reuse разрешён только для backend-а, который держит stable surface identity,
/// проверяет backing DMA-BUF object identity и возвращает decoded surface
/// decoder-у строго после renderer GPU completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroCopyImportReuseContract {
    /// Имя backend-а, который обещает stable surface/import identity.
    pub backend_name: &'static str,

    /// Renderer не пишет в imported image, а только семплит её после decoder sync.
    pub renderer_is_sample_only: bool,

    /// Decoder получает surface обратно только после renderer GPU completion callback.
    pub decoder_reuse_waits_for_gpu_completion: bool,

    /// Surface id остаётся primary key внутри decoder output pool-а.
    pub import_identity_is_surface_id: bool,

    /// Exported DMA-BUF object identity проверяется отдельно от transient fd.
    pub dma_buf_object_identity_checked: bool,

    /// Backend явно выполняет/гарантирует ownership/layout/cache sync для reuse.
    pub explicit_external_memory_reuse_sync: bool,
}

impl ZeroCopyImportReuseContract {
    /// Контракт текущего VA-API/Vulkan DMA-BUF path.
    ///
    /// VA surface удерживается decoded handle-ом до release ack, а renderer-side
    /// GPU work подтверждается через renderer queue callback перед возвратом surface
    /// в decoder frame pool. Persistent import reuse пока выключен: текущий
    /// Vulkan import слой не оформляет явные VA writer -> Vulkan sampler transitions.
    pub const fn vaapi_vulkan_dma_buf() -> Self {
        Self {
            backend_name: "VA-API Vulkan DMA-BUF",
            renderer_is_sample_only: true,
            decoder_reuse_waits_for_gpu_completion: true,
            import_identity_is_surface_id: true,
            dma_buf_object_identity_checked: true,
            explicit_external_memory_reuse_sync: false,
        }
    }

    /// Проверяет, что contract действительно разрешает persistent import reuse.
    fn allows_persistent_reuse(self) -> bool {
        self.renderer_is_sample_only
            && self.decoder_reuse_waits_for_gpu_completion
            && self.import_identity_is_surface_id
            && self.dma_buf_object_identity_checked
            && self.explicit_external_memory_reuse_sync
    }
}

/// Fd-independent identity backing DMA-BUF object-а.
///
/// Номер fd не годится для cache key: VA export может вернуть новый fd на тот
/// же kernel object. `fstat` даёт стабильную identity самого object-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ZeroCopyObjectIdentity {
    /// Device, на котором живёт fd.
    device: u64,

    /// Inode backing object-а.
    inode: u64,

    /// Special-device id для fd, если kernel его заполняет.
    special_device: u64,
}

/// Immutable fingerprint exported DMA-BUF object-а без fd number.
///
/// File descriptor number меняется при каждом export/dup, поэтому descriptor
/// сравнивает только свойства backing object-а, важные для уже imported image.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZeroCopyObjectDescriptor {
    /// Размер exported object-а в байтах.
    size: u32,

    /// DRM modifier backing memory layout-а.
    drm_format_modifier: u64,

    /// Kernel identity backing object-а, которому соответствует import.
    identity: ZeroCopyObjectIdentity,
}

/// Immutable fingerprint DRM PRIME layer-а.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZeroCopyLayerDescriptor {
    /// DRM fourcc конкретного layer-а.
    drm_format: u32,

    /// Количество planes внутри layer-а.
    num_planes: u32,

    /// Индексы objects для planes.
    object_index: [u8; 4],

    /// Byte offsets planes внутри objects.
    offset: [u32; 4],

    /// Row pitches planes в байтах.
    pitch: [u32; 4],
}

/// Codec-neutral descriptor decoded surface/import identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZeroCopySurfaceDescriptor {
    /// Backend-native decoded surface id; для VA-API это `VASurfaceID`.
    surface_id: u64,

    /// Top-level DRM fourcc exported surface-а.
    fourcc: u32,

    /// Layout export-а: composed multi-planar или separate layers.
    export_layout: DecodedDmaBufExportLayout,

    /// Coded width imported image-а.
    width: u32,

    /// Coded height imported image-а.
    height: u32,

    /// Stable object descriptors без transient fd number.
    objects: Vec<ZeroCopyObjectDescriptor>,

    /// Stable layer descriptors.
    layers: Vec<ZeroCopyLayerDescriptor>,
}

impl ZeroCopySurfaceDescriptor {
    /// Строит descriptor из exported DMA-BUF image.
    pub fn from_dma_buf_image(
        image: &DecodedDmaBufImage,
    ) -> Result<Self, ZeroCopySurfacePoolError> {
        let objects = image
            .objects
            .iter()
            .enumerate()
            .map(|(object_index, object)| {
                let file_stat = fstat(object.fd.as_raw_fd()).map_err(|error| {
                    ZeroCopySurfacePoolError::ObjectIdentityUnavailable {
                        surface_id: image.surface_id,
                        object_index,
                        reason: error.to_string(),
                    }
                })?;

                Ok(ZeroCopyObjectDescriptor {
                    size: object.size,
                    drm_format_modifier: object.drm_format_modifier,
                    identity: ZeroCopyObjectIdentity {
                        device: file_stat.st_dev,
                        inode: file_stat.st_ino,
                        special_device: file_stat.st_rdev,
                    },
                })
            })
            .collect::<Result<Vec<_>, ZeroCopySurfacePoolError>>()?;

        let layers = image
            .layers
            .iter()
            .map(|layer| ZeroCopyLayerDescriptor {
                drm_format: layer.drm_format,
                num_planes: layer.num_planes,
                object_index: layer.object_index,
                offset: layer.offset,
                pitch: layer.pitch,
            })
            .collect();

        Ok(Self {
            surface_id: image.surface_id,
            fourcc: image.fourcc,
            export_layout: image.export_layout,
            width: image.width,
            height: image.height,
            objects,
            layers,
        })
    }

    /// Возвращает backend-native surface id для logs и cache key.
    pub const fn surface_id(&self) -> u64 {
        self.surface_id
    }
}

/// Состояние одного persistent zero-copy slot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZeroCopySurfaceState {
    /// Slot содержит persistent import и доступен для нового кадра той же surface.
    Free,

    /// Slot отдан renderer-у как frame lease.
    Leased {
        /// Уникальный frame handle текущего lease-а.
        handle: FrameResourceHandle,

        /// Monotonic generation lease-а внутри slot-а.
        generation: u64,
    },

    /// Renderer отпустил lease, но GPU completion callback ещё не пришёл.
    WaitingGpuCompletion {
        /// Уникальный frame handle текущего release-а.
        handle: FrameResourceHandle,

        /// Generation, выданная при lease.
        generation: u64,
    },

    /// GPU completion подтверждён, но decoder ещё держит decoded handle guard.
    WaitingDecoderReuse {
        /// Уникальный frame handle текущего release-а.
        handle: FrameResourceHandle,

        /// Generation, выданная при lease.
        generation: u64,
    },
}

impl ZeroCopySurfaceState {
    /// Возвращает короткое имя состояния для typed lifecycle errors.
    const fn name(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Leased { .. } => "leased",
            Self::WaitingGpuCompletion { .. } => "waiting-gpu-completion",
            Self::WaitingDecoderReuse { .. } => "waiting-decoder-reuse",
        }
    }

    /// Возвращает handle, если состояние привязано к live/releasing lease.
    const fn handle(self) -> Option<FrameResourceHandle> {
        match self {
            Self::Free => None,
            Self::Leased { handle, .. }
            | Self::WaitingGpuCompletion { handle, .. }
            | Self::WaitingDecoderReuse { handle, .. } => Some(handle),
        }
    }

    /// Возвращает generation live/releasing lease-а.
    const fn generation(self) -> Option<u64> {
        match self {
            Self::Free => None,
            Self::Leased { generation, .. }
            | Self::WaitingGpuCompletion { generation, .. }
            | Self::WaitingDecoderReuse { generation, .. } => Some(generation),
        }
    }
}

/// Metadata одного persistent surface/import slot-а.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZeroCopySurfaceSlot {
    /// Descriptor, которому соответствует imported GPU resource.
    descriptor: ZeroCopySurfaceDescriptor,

    /// Текущее lifecycle состояние slot-а.
    state: ZeroCopySurfaceState,

    /// Следующая generation, которая будет выдана при lease.
    next_generation: u64,
}

impl ZeroCopySurfaceSlot {
    /// Создаёт slot сразу в состоянии active lease.
    fn leased(descriptor: ZeroCopySurfaceDescriptor, handle: FrameResourceHandle) -> Self {
        Self {
            descriptor,
            state: ZeroCopySurfaceState::Leased {
                handle,
                generation: 0,
            },
            next_generation: 1,
        }
    }

    /// Переводит free slot в новый renderer lease.
    fn lease(&mut self, handle: FrameResourceHandle) {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        self.state = ZeroCopySurfaceState::Leased { handle, generation };
    }
}

/// Результат подготовки imported surface для decoded кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroCopyImportPlan {
    /// Можно переиспользовать уже imported GPU resource.
    Reuse {
        /// Индекс slot-а в storage pool-е владельца.
        slot_index: usize,
    },

    /// Нужно создать новый external import и затем зарегистрировать slot.
    Create,

    /// Нужно заменить existing free import после успешного нового import-а.
    Replace {
        /// Индекс free slot-а, storage которого будет заменён.
        slot_index: usize,
    },
}

/// Token release-а, который должен дожить до GPU completion callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroCopyGpuCompletionLease {
    /// Frame handle, ожидающий GPU completion.
    frame_handle: FrameResourceHandle,
}

impl ZeroCopyGpuCompletionLease {
    /// Возвращает frame handle для callback-а.
    pub const fn frame_handle(self) -> FrameResourceHandle {
        self.frame_handle
    }
}

/// Observable counters zero-copy surface/import pool-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZeroCopySurfacePoolStats {
    /// Максимум persistent import slots.
    pub capacity: usize,

    /// Количество созданных persistent import slots.
    pub slots: usize,

    /// Количество slots, занятых live/releasing кадрами.
    pub active_surfaces: usize,

    /// Количество slots, доступных для reuse.
    pub free_surfaces: usize,

    /// Количество slots, ожидающих GPU completion.
    pub waiting_gpu_completion: usize,

    /// Количество slots, ожидающих decoder reuse ack.
    pub waiting_decoder_reuse: usize,

    /// Количество failed zero-copy imports с момента создания pool-а.
    pub import_failures: u64,

    /// Количество реально созданных external imports.
    pub imports_created: u64,

    /// Количество кадров, которые переиспользовали existing import.
    pub imports_reused: u64,

    /// Количество free imports, заменённых из-за descriptor/object identity change.
    pub imports_replaced: u64,
}

impl ZeroCopySurfacePoolStats {
    /// Возвращает свободный decode headroom с учётом persistent reuse.
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.active_surfaces)
    }
}

/// Typed lifecycle/import ошибки zero-copy surface pool-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZeroCopySurfacePoolError {
    /// Backend contract не разрешает persistent reuse.
    ReuseContractDisabled {
        /// Имя backend-а из contract-а.
        backend_name: &'static str,
    },

    /// Pool достиг bounded capacity.
    PoolExhausted {
        /// Максимальный размер pool-а.
        capacity: usize,

        /// Текущее число созданных slots.
        slots: usize,
    },

    /// Descriptor existing surface-а изменился под тем же surface id.
    DescriptorChanged {
        /// Backend-native surface id.
        surface_id: u64,
    },

    /// Не удалось получить kernel identity exported DMA-BUF object-а.
    ObjectIdentityUnavailable {
        /// Backend-native surface id.
        surface_id: u64,

        /// Индекс object-а внутри DRM PRIME descriptor-а.
        object_index: usize,

        /// Текст syscall ошибки.
        reason: String,
    },

    /// Surface пришла повторно до разрешённого reuse состояния.
    SurfaceBusy {
        /// Backend-native surface id.
        surface_id: u64,

        /// Текущее состояние surface slot-а.
        state: &'static str,
    },

    /// Frame handle не принадлежит текущему pool lifecycle.
    UnknownHandle {
        /// Frame handle id.
        handle_id: u64,

        /// Операция, которая обнаружила stale handle.
        operation: &'static str,
    },

    /// Frame handle находится не в том lifecycle состоянии.
    UnexpectedState {
        /// Frame handle id.
        handle_id: u64,

        /// Ожидаемое состояние.
        expected: &'static str,

        /// Фактическое состояние.
        actual: &'static str,

        /// Операция, которая проверяла состояние.
        operation: &'static str,
    },

    /// Resource storage владельца и lifecycle pool разошлись по slot index.
    MissingStorageSlot {
        /// Slot index, который вернул lifecycle pool.
        slot_index: usize,
    },

    /// Попытка сбросить pool при live/releasing кадрах.
    InvalidateWithActiveSurfaces {
        /// Количество active surfaces.
        active_surfaces: usize,
    },
}

impl fmt::Display for ZeroCopySurfacePoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReuseContractDisabled { backend_name } => write!(
                formatter,
                "zero-copy import reuse is disabled by backend contract: {backend_name}"
            ),
            Self::PoolExhausted { capacity, slots } => write!(
                formatter,
                "zero-copy surface pool exhausted: slots={slots}, capacity={capacity}"
            ),
            Self::DescriptorChanged { surface_id } => write!(
                formatter,
                "zero-copy surface descriptor changed for stable surface id {surface_id}"
            ),
            Self::ObjectIdentityUnavailable {
                surface_id,
                object_index,
                reason,
            } => write!(
                formatter,
                "zero-copy surface {surface_id} DMA-BUF object {object_index} identity unavailable: {reason}"
            ),
            Self::SurfaceBusy { surface_id, state } => write!(
                formatter,
                "zero-copy surface {surface_id} cannot be reused while state is {state}"
            ),
            Self::UnknownHandle {
                handle_id,
                operation,
            } => write!(
                formatter,
                "zero-copy lifecycle {operation} saw unknown frame handle {handle_id}"
            ),
            Self::UnexpectedState {
                handle_id,
                expected,
                actual,
                operation,
            } => write!(
                formatter,
                "zero-copy lifecycle {operation} expected {expected} for handle {handle_id}, got {actual}"
            ),
            Self::MissingStorageSlot { slot_index } => {
                write!(
                    formatter,
                    "zero-copy lifecycle references missing storage slot {slot_index}"
                )
            }
            Self::InvalidateWithActiveSurfaces { active_surfaces } => write!(
                formatter,
                "zero-copy surface pool cannot be invalidated with {active_surfaces} active surfaces"
            ),
        }
    }
}

impl std::error::Error for ZeroCopySurfacePoolError {}

/// Управляемый lifecycle pool persistent zero-copy imports.
pub struct ZeroCopySurfacePool {
    /// Bounded capacity persistent resources.
    capacity: usize,

    /// Явный backend reuse/synchronization contract.
    reuse_contract: ZeroCopyImportReuseContract,

    /// Slots indexed identically to owner resource storage.
    slots: Vec<ZeroCopySurfaceSlot>,

    /// Быстрый поиск slot-а по backend-native surface id.
    surface_to_slot: HashMap<u64, usize>,

    /// Быстрый поиск live/releasing slot-а по frame handle.
    handle_to_slot: HashMap<u64, usize>,

    /// Счётчик failed imports для diagnostics.
    import_failures: u64,

    /// Счётчик real external imports для churn diagnostics.
    imports_created: u64,

    /// Счётчик reuse hits для churn diagnostics.
    imports_reused: u64,

    /// Счётчик free slot replacements для churn diagnostics.
    imports_replaced: u64,
}

impl ZeroCopySurfacePool {
    /// Создаёт пустой lifecycle pool.
    pub fn new(capacity: usize, reuse_contract: ZeroCopyImportReuseContract) -> Self {
        Self {
            capacity,
            reuse_contract,
            slots: Vec::with_capacity(capacity),
            surface_to_slot: HashMap::new(),
            handle_to_slot: HashMap::new(),
            import_failures: 0,
            imports_created: 0,
            imports_reused: 0,
            imports_replaced: 0,
        }
    }

    /// Возвращает явный backend reuse contract.
    pub const fn reuse_contract(&self) -> ZeroCopyImportReuseContract {
        self.reuse_contract
    }

    /// Готовит existing slot reuse/replace или разрешает создать новый import.
    pub fn begin_import_or_reuse(
        &mut self,
        descriptor: ZeroCopySurfaceDescriptor,
        handle: FrameResourceHandle,
    ) -> Result<ZeroCopyImportPlan, ZeroCopySurfacePoolError> {
        if let Some(&slot_index) = self.surface_to_slot.get(&descriptor.surface_id) {
            let slot = self
                .slots
                .get_mut(slot_index)
                .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

            if slot.state != ZeroCopySurfaceState::Free {
                return Err(ZeroCopySurfacePoolError::SurfaceBusy {
                    surface_id: descriptor.surface_id,
                    state: slot.state.name(),
                });
            }

            if slot.descriptor == descriptor && self.reuse_contract.allows_persistent_reuse() {
                slot.lease(handle);
                self.handle_to_slot.insert(handle.0, slot_index);
                self.imports_reused = self.imports_reused.saturating_add(1);
                return Ok(ZeroCopyImportPlan::Reuse { slot_index });
            }

            return Ok(ZeroCopyImportPlan::Replace { slot_index });
        }

        if self.slots.len() >= self.capacity {
            return Err(ZeroCopySurfacePoolError::PoolExhausted {
                capacity: self.capacity,
                slots: self.slots.len(),
            });
        }

        Ok(ZeroCopyImportPlan::Create)
    }

    /// Регистрирует storage slot после успешного external import.
    pub fn register_imported_surface(
        &mut self,
        descriptor: ZeroCopySurfaceDescriptor,
        handle: FrameResourceHandle,
        slot_index: usize,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        if slot_index != self.slots.len() {
            return Err(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index });
        }

        if self.slots.len() >= self.capacity {
            return Err(ZeroCopySurfacePoolError::PoolExhausted {
                capacity: self.capacity,
                slots: self.slots.len(),
            });
        }

        let surface_id = descriptor.surface_id;
        self.surface_to_slot.insert(surface_id, slot_index);
        self.handle_to_slot.insert(handle.0, slot_index);
        self.slots
            .push(ZeroCopySurfaceSlot::leased(descriptor, handle));
        self.imports_created = self.imports_created.saturating_add(1);
        Ok(())
    }

    /// Регистрирует replacement storage после успешного external import-а.
    pub fn register_replaced_surface(
        &mut self,
        descriptor: ZeroCopySurfaceDescriptor,
        handle: FrameResourceHandle,
        slot_index: usize,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        if slot.state != ZeroCopySurfaceState::Free {
            return Err(ZeroCopySurfacePoolError::SurfaceBusy {
                surface_id: descriptor.surface_id,
                state: slot.state.name(),
            });
        }

        let previous_surface_id = slot.descriptor.surface_id;
        self.surface_to_slot.remove(&previous_surface_id);
        self.surface_to_slot
            .insert(descriptor.surface_id, slot_index);
        slot.descriptor = descriptor;
        slot.lease(handle);
        self.handle_to_slot.insert(handle.0, slot_index);
        self.imports_created = self.imports_created.saturating_add(1);
        self.imports_replaced = self.imports_replaced.saturating_add(1);
        Ok(())
    }

    /// Возвращает storage slot index для live renderer lease.
    pub fn leased_slot_index(&self, handle: FrameResourceHandle) -> Option<usize> {
        let slot_index = *self.handle_to_slot.get(&handle.0)?;
        let slot = self.slots.get(slot_index)?;
        let is_same_handle = slot.state.handle() == Some(handle);
        let is_leased = matches!(slot.state, ZeroCopySurfaceState::Leased { .. });
        is_same_handle.then_some(()).filter(|_| is_leased)?;
        Some(slot_index)
    }

    /// Переводит renderer-owned frame в ожидание GPU completion.
    pub fn release_after_gpu_submission(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<ZeroCopyGpuCompletionLease, ZeroCopySurfacePoolError> {
        let (slot_index, generation) =
            self.expect_handle_state(handle, "release_after_gpu_submission", "leased")?;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        if !matches!(slot.state, ZeroCopySurfaceState::Leased { .. }) {
            return Err(ZeroCopySurfacePoolError::UnexpectedState {
                handle_id: handle.0,
                expected: "leased",
                actual: slot.state.name(),
                operation: "release_after_gpu_submission",
            });
        }

        slot.state = ZeroCopySurfaceState::WaitingGpuCompletion { handle, generation };
        Ok(ZeroCopyGpuCompletionLease {
            frame_handle: handle,
        })
    }

    /// Релизит frame, который не был отдан renderer GPU work-у.
    pub fn release_without_gpu_submission(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        let (slot_index, generation) =
            self.expect_handle_state(handle, "release_without_gpu_submission", "leased")?;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        if !matches!(slot.state, ZeroCopySurfaceState::Leased { .. }) {
            return Err(ZeroCopySurfacePoolError::UnexpectedState {
                handle_id: handle.0,
                expected: "leased",
                actual: slot.state.name(),
                operation: "release_without_gpu_submission",
            });
        }

        slot.state = ZeroCopySurfaceState::WaitingDecoderReuse { handle, generation };
        Ok(())
    }

    /// Подтверждает completion GPU work-а для renderer-owned frame-а.
    pub fn acknowledge_gpu_completion(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        let (slot_index, generation) = self.expect_handle_state(
            handle,
            "acknowledge_gpu_completion",
            "waiting-gpu-completion",
        )?;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        if !matches!(
            slot.state,
            ZeroCopySurfaceState::WaitingGpuCompletion { .. }
        ) {
            return Err(ZeroCopySurfacePoolError::UnexpectedState {
                handle_id: handle.0,
                expected: "waiting-gpu-completion",
                actual: slot.state.name(),
                operation: "acknowledge_gpu_completion",
            });
        }

        slot.state = ZeroCopySurfaceState::WaitingDecoderReuse { handle, generation };
        Ok(())
    }

    /// Подтверждает, что decoder guard отпущен и surface можно переиспользовать.
    pub fn acknowledge_decoder_reuse(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        let (slot_index, _generation) =
            self.expect_handle_state(handle, "acknowledge_decoder_reuse", "waiting-decoder-reuse")?;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        if !matches!(slot.state, ZeroCopySurfaceState::WaitingDecoderReuse { .. }) {
            return Err(ZeroCopySurfacePoolError::UnexpectedState {
                handle_id: handle.0,
                expected: "waiting-decoder-reuse",
                actual: slot.state.name(),
                operation: "acknowledge_decoder_reuse",
            });
        }

        self.handle_to_slot.remove(&handle.0);
        slot.state = ZeroCopySurfaceState::Free;
        Ok(())
    }

    /// Увеличивает счётчик failed imports для diagnostics.
    pub fn record_import_failure(&mut self) {
        self.import_failures = self.import_failures.saturating_add(1);
    }

    /// Проверяет handle и возвращает slot index + lease generation.
    fn expect_handle_state(
        &self,
        handle: FrameResourceHandle,
        operation: &'static str,
        expected: &'static str,
    ) -> Result<(usize, u64), ZeroCopySurfacePoolError> {
        let Some(&slot_index) = self.handle_to_slot.get(&handle.0) else {
            return Err(ZeroCopySurfacePoolError::UnknownHandle {
                handle_id: handle.0,
                operation,
            });
        };

        let slot = self
            .slots
            .get(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        if slot.state.handle() != Some(handle) {
            return Err(ZeroCopySurfacePoolError::UnknownHandle {
                handle_id: handle.0,
                operation,
            });
        }

        let actual = slot.state.name();
        if actual != expected {
            return Err(ZeroCopySurfacePoolError::UnexpectedState {
                handle_id: handle.0,
                expected,
                actual,
                operation,
            });
        }

        Ok((slot_index, slot.state.generation().unwrap_or(0)))
    }

    /// Сбрасывает pool только если нет live/releasing surfaces.
    pub fn invalidate_all(&mut self) -> Result<(), ZeroCopySurfacePoolError> {
        let active_surfaces = self.stats().active_surfaces;
        if active_surfaces != 0 {
            return Err(ZeroCopySurfacePoolError::InvalidateWithActiveSurfaces { active_surfaces });
        }

        self.slots.clear();
        self.surface_to_slot.clear();
        self.handle_to_slot.clear();
        Ok(())
    }

    /// Собирает observable lifecycle counters.
    pub fn stats(&self) -> ZeroCopySurfacePoolStats {
        let mut stats = ZeroCopySurfacePoolStats {
            capacity: self.capacity,
            slots: self.slots.len(),
            import_failures: self.import_failures,
            imports_created: self.imports_created,
            imports_reused: self.imports_reused,
            imports_replaced: self.imports_replaced,
            ..ZeroCopySurfacePoolStats::default()
        };

        for slot in &self.slots {
            match slot.state {
                ZeroCopySurfaceState::Free => {
                    stats.free_surfaces += 1;
                }
                ZeroCopySurfaceState::Leased { .. } => {
                    stats.active_surfaces += 1;
                }
                ZeroCopySurfaceState::WaitingGpuCompletion { .. } => {
                    stats.active_surfaces += 1;
                    stats.waiting_gpu_completion += 1;
                }
                ZeroCopySurfaceState::WaitingDecoderReuse { .. } => {
                    stats.active_surfaces += 1;
                    stats.waiting_decoder_reuse += 1;
                }
            }
        }

        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Создаёт минимальный descriptor без реального fd/GPU resource.
    fn descriptor(surface_id: u64) -> ZeroCopySurfaceDescriptor {
        ZeroCopySurfaceDescriptor {
            surface_id,
            fourcc: u32::from_le_bytes(*b"NV12"),
            export_layout: DecodedDmaBufExportLayout::ComposedLayers,
            width: 1920,
            height: 1080,
            objects: vec![ZeroCopyObjectDescriptor {
                size: 3_110_400,
                drm_format_modifier: 0,
                identity: ZeroCopyObjectIdentity {
                    device: 1,
                    inode: surface_id,
                    special_device: 0,
                },
            }],
            layers: vec![ZeroCopyLayerDescriptor {
                drm_format: u32::from_le_bytes(*b"NV12"),
                num_planes: 2,
                object_index: [0, 0, 0, 0],
                offset: [0, 2_073_600, 0, 0],
                pitch: [1920, 1920, 0, 0],
            }],
        }
    }

    /// Создаёт contract, где unit test явно моделирует backend-safe reuse.
    fn verified_reuse_contract_for_tests() -> ZeroCopyImportReuseContract {
        ZeroCopyImportReuseContract {
            explicit_external_memory_reuse_sync: true,
            ..ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf()
        }
    }

    /// Создаёт pool с test contract-ом, который разрешает persistent reuse.
    fn pool(capacity: usize) -> ZeroCopySurfacePool {
        ZeroCopySurfacePool::new(capacity, verified_reuse_contract_for_tests())
    }

    /// Создаёт pool с production VAAPI/DMA-BUF contract-ом.
    fn production_pool(capacity: usize) -> ZeroCopySurfacePool {
        ZeroCopySurfacePool::new(
            capacity,
            ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf(),
        )
    }

    #[test]
    fn lease_release_order_requires_gpu_before_decoder_reuse() {
        let mut surface_pool = pool(2);
        let first_handle = FrameResourceHandle(10);
        let first_descriptor = descriptor(7);

        let import_plan = surface_pool
            .begin_import_or_reuse(first_descriptor.clone(), first_handle)
            .unwrap();
        assert_eq!(import_plan, ZeroCopyImportPlan::Create);
        surface_pool
            .register_imported_surface(first_descriptor, first_handle, 0)
            .unwrap();

        surface_pool
            .release_after_gpu_submission(first_handle)
            .unwrap();
        assert!(
            surface_pool
                .acknowledge_decoder_reuse(first_handle)
                .unwrap_err()
                .to_string()
                .contains("expected waiting-decoder-reuse")
        );

        surface_pool
            .acknowledge_gpu_completion(first_handle)
            .unwrap();
        surface_pool
            .acknowledge_decoder_reuse(first_handle)
            .unwrap();

        let stats = surface_pool.stats();
        assert_eq!(stats.active_surfaces, 0);
        assert_eq!(stats.free_surfaces, 1);
    }

    #[test]
    fn released_surface_reuses_import_with_new_generation_and_rejects_stale_handle() {
        let mut surface_pool = pool(2);
        let first_handle = FrameResourceHandle(10);
        let second_handle = FrameResourceHandle(11);
        let first_descriptor = descriptor(7);

        surface_pool
            .begin_import_or_reuse(first_descriptor.clone(), first_handle)
            .unwrap();
        surface_pool
            .register_imported_surface(first_descriptor.clone(), first_handle, 0)
            .unwrap();
        surface_pool
            .release_without_gpu_submission(first_handle)
            .unwrap();
        surface_pool
            .acknowledge_decoder_reuse(first_handle)
            .unwrap();

        let import_plan = surface_pool
            .begin_import_or_reuse(first_descriptor, second_handle)
            .unwrap();

        assert_eq!(import_plan, ZeroCopyImportPlan::Reuse { slot_index: 0 });
        assert_eq!(surface_pool.leased_slot_index(second_handle), Some(0));
        assert!(surface_pool.leased_slot_index(first_handle).is_none());
        assert!(matches!(
            surface_pool
                .release_without_gpu_submission(first_handle)
                .unwrap_err(),
            ZeroCopySurfacePoolError::UnknownHandle { .. }
        ));
        assert_eq!(surface_pool.stats().imports_reused, 1);
    }

    #[test]
    fn free_surface_replaces_import_when_descriptor_changes() {
        let mut surface_pool = pool(2);
        let first_handle = FrameResourceHandle(10);
        let second_handle = FrameResourceHandle(11);
        let first_descriptor = descriptor(7);
        let mut changed_descriptor = descriptor(7);
        changed_descriptor.width = 1280;

        surface_pool
            .begin_import_or_reuse(first_descriptor.clone(), first_handle)
            .unwrap();
        surface_pool
            .register_imported_surface(first_descriptor, first_handle, 0)
            .unwrap();
        surface_pool
            .release_without_gpu_submission(first_handle)
            .unwrap();
        surface_pool
            .acknowledge_decoder_reuse(first_handle)
            .unwrap();

        let import_plan = surface_pool
            .begin_import_or_reuse(changed_descriptor, second_handle)
            .unwrap();

        assert_eq!(import_plan, ZeroCopyImportPlan::Replace { slot_index: 0 });
    }

    #[test]
    fn production_contract_replaces_same_descriptor_until_explicit_sync_exists() {
        let mut surface_pool = production_pool(2);
        let first_handle = FrameResourceHandle(10);
        let second_handle = FrameResourceHandle(11);
        let first_descriptor = descriptor(7);

        surface_pool
            .begin_import_or_reuse(first_descriptor.clone(), first_handle)
            .unwrap();
        surface_pool
            .register_imported_surface(first_descriptor.clone(), first_handle, 0)
            .unwrap();
        surface_pool
            .release_without_gpu_submission(first_handle)
            .unwrap();
        surface_pool
            .acknowledge_decoder_reuse(first_handle)
            .unwrap();

        let import_plan = surface_pool
            .begin_import_or_reuse(first_descriptor, second_handle)
            .unwrap();

        assert_eq!(import_plan, ZeroCopyImportPlan::Replace { slot_index: 0 });
        assert_eq!(surface_pool.stats().imports_reused, 0);
    }

    #[test]
    fn replaced_surface_keeps_generation_safety_and_records_churn() {
        let mut surface_pool = pool(2);
        let first_handle = FrameResourceHandle(10);
        let second_handle = FrameResourceHandle(11);
        let first_descriptor = descriptor(7);
        let mut changed_descriptor = descriptor(7);
        changed_descriptor.objects[0].identity.inode = 99;

        surface_pool
            .begin_import_or_reuse(first_descriptor.clone(), first_handle)
            .unwrap();
        surface_pool
            .register_imported_surface(first_descriptor, first_handle, 0)
            .unwrap();
        surface_pool
            .release_without_gpu_submission(first_handle)
            .unwrap();
        surface_pool
            .acknowledge_decoder_reuse(first_handle)
            .unwrap();

        let import_plan = surface_pool
            .begin_import_or_reuse(changed_descriptor.clone(), second_handle)
            .unwrap();

        assert_eq!(import_plan, ZeroCopyImportPlan::Replace { slot_index: 0 });
        surface_pool
            .register_replaced_surface(changed_descriptor, second_handle, 0)
            .unwrap();

        assert_eq!(surface_pool.leased_slot_index(second_handle), Some(0));
        assert!(surface_pool.leased_slot_index(first_handle).is_none());
        assert!(matches!(
            surface_pool
                .release_without_gpu_submission(first_handle)
                .unwrap_err(),
            ZeroCopySurfacePoolError::UnknownHandle { .. }
        ));
        assert_eq!(surface_pool.stats().imports_created, 2);
        assert_eq!(surface_pool.stats().imports_replaced, 1);
        assert_eq!(surface_pool.stats().imports_reused, 0);
    }

    #[test]
    fn busy_surface_cannot_be_leased_twice() {
        let mut surface_pool = pool(2);
        let first_handle = FrameResourceHandle(10);
        let second_handle = FrameResourceHandle(11);
        let first_descriptor = descriptor(7);

        surface_pool
            .begin_import_or_reuse(first_descriptor.clone(), first_handle)
            .unwrap();
        surface_pool
            .register_imported_surface(first_descriptor.clone(), first_handle, 0)
            .unwrap();

        let error = surface_pool
            .begin_import_or_reuse(first_descriptor, second_handle)
            .unwrap_err();

        assert!(matches!(
            error,
            ZeroCopySurfacePoolError::SurfaceBusy {
                surface_id: 7,
                state: "leased"
            }
        ));
    }
}
