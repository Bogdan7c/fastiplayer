use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use tracing::trace;

use super::control::{
    DecoderControlChannelPressureCounters, DecoderControlOperation, ThreadControlMsg,
    record_decoder_control_send_failure,
};
use super::{DecodeThreadError, DecoderThreadState};

/// Диагностика получения VAAPI resource pool lock-а на render hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameResourceLockDiagnostics {
    /// Сколько render thread ждал mutex resource pool-а.
    pub wait: Duration,
}

/// Результат playback-facing resource lookup-а без GPU handles.
pub enum VideoFrameResourceLookup {
    /// Resource pool доступен, handle валиден.
    Ready {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool сейчас занят другим потоком, render hot path не должен ждать.
    Busy {
        /// Timing короткой non-blocking попытки получить `FrameResourcePool`.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool доступен, но handle не указывает на active resource.
    Missing {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool не может безопасно ответить из-за poisoned/fatal состояния.
    Fatal {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },
}

impl VideoFrameResourceLookup {
    /// Возвращает timing mutex boundary независимо от lookup outcome.
    #[must_use]
    pub const fn lock_diagnostics(&self) -> VideoFrameResourceLockDiagnostics {
        match self {
            Self::Ready { lock_diagnostics }
            | Self::Busy { lock_diagnostics }
            | Self::Missing { lock_diagnostics }
            | Self::Fatal { lock_diagnostics } => *lock_diagnostics,
        }
    }
}

/// Результат renderer-facing descriptor lookup-а с duplicated platform handles.
pub enum VideoFrameResourceDescriptorLookup {
    /// Descriptor duplicated успешно; renderer владеет returned fd.
    Ready {
        /// Neutral descriptor без VAAPI/cros/renderer API types.
        descriptor: video_core::FrameResourceDescriptor,

        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool сейчас занят другим потоком, render hot path не должен ждать.
    Busy {
        /// Timing короткой non-blocking попытки получить `FrameResourcePool`.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Resource pool доступен, но handle не указывает на active resource.
    Missing {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },

    /// Descriptor нельзя безопасно дублировать из-за poisoned/fatal состояния.
    Fatal {
        /// Timing ожидания `FrameResourcePool` lock-а.
        lock_diagnostics: VideoFrameResourceLockDiagnostics,
    },
}

impl VideoFrameResourceDescriptorLookup {
    /// Возвращает timing mutex boundary независимо от lookup outcome.
    #[must_use]
    pub const fn lock_diagnostics(&self) -> VideoFrameResourceLockDiagnostics {
        match self {
            Self::Ready {
                lock_diagnostics, ..
            }
            | Self::Busy { lock_diagnostics }
            | Self::Missing { lock_diagnostics }
            | Self::Fatal { lock_diagnostics } => *lock_diagnostics,
        }
    }
}

/// Узкий provider для VAAPI resource status, descriptor duplication и release.
#[derive(Clone)]
pub struct VideoFrameResourceProvider {
    /// Канал decoder thread для release zero-copy VA handles.
    pub(super) control_tx: Sender<ThreadControlMsg>,

    /// Shared counters pressure/failure diagnostics для control channel.
    pub(super) control_pressure: Arc<DecoderControlChannelPressureCounters>,

    /// Shared resource pool, из которого renderer получает duplicated descriptors.
    pub(super) resource_pool: Arc<Mutex<crate::resource_pool::FrameResourcePool>>,

    /// Shared fatal state, чтобы release path мог сообщить о disconnect-е.
    pub(super) thread_state: DecoderThreadState,
}

impl VideoFrameResourceProvider {
    /// Получает status и timing ожидания resource pool mutex-а.
    #[must_use]
    pub fn resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> VideoFrameResourceLookup {
        resource_lookup_from_pool(self.resource_pool.as_ref(), handle)
    }

    /// Пытается получить status без ожидания resource pool mutex-а.
    #[must_use]
    pub fn try_resource_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> VideoFrameResourceLookup {
        try_resource_lookup_from_pool(self.resource_pool.as_ref(), handle)
    }

    /// Пытается получить duplicated descriptor без ожидания resource pool mutex-а.
    #[must_use]
    pub fn try_resource_descriptor_lookup(
        &self,
        handle: video_core::FrameResourceHandle,
    ) -> VideoFrameResourceDescriptorLookup {
        try_resource_descriptor_lookup_from_pool(self.resource_pool.as_ref(), handle)
    }

    /// Освобождает frame после того, как caller уже дождался GPU completion.
    pub fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        let release_stats = match self.resource_pool.lock() {
            Ok(mut resource_pool) => {
                if let Err(error) = resource_pool.release_without_gpu_submission(handle) {
                    let resource_stats = resource_pool.stats();
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("Zero-copy surface release lifecycle violation: {error}"),
                    ));
                    tracing::warn!(
                        error = %error,
                        fatal = %fatal_error,
                        handle_id = handle.0,
                        zero_copy_capacity = resource_stats.capacity,
                        zero_copy_slots = resource_stats.slots,
                        zero_copy_in_use = resource_stats.in_use,
                        zero_copy_free_surfaces = resource_stats.free_surfaces,
                        zero_copy_waiting_gpu_completion =
                            resource_stats.waiting_gpu_completion,
                        zero_copy_waiting_decoder_reuse =
                            resource_stats.waiting_decoder_reuse,
                        "Failed to move zero-copy surface into decoder reuse state"
                    );
                    return;
                }
                let resource_stats = resource_pool.stats();
                trace!(
                    handle_id = handle.0,
                    zero_copy_capacity = resource_stats.capacity,
                    zero_copy_slots = resource_stats.slots,
                    zero_copy_in_use = resource_stats.in_use,
                    zero_copy_free_surfaces = resource_stats.free_surfaces,
                    zero_copy_waiting_gpu_completion = resource_stats.waiting_gpu_completion,
                    zero_copy_waiting_decoder_reuse = resource_stats.waiting_decoder_reuse,
                    "Queued renderer-owned zero-copy frame for decoder reuse"
                );
                Some(resource_stats)
            }
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "Zero-copy resource pool mutex poisoned during release: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = handle.0,
                    "Resource pool mutex poisoned during release"
                );
                return;
            }
        };

        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ReleaseZeroCopy(handle))
        {
            let error_message = record_decoder_control_send_failure(
                DecoderControlOperation::Release,
                &self.control_tx,
                &self.control_pressure,
                &error,
            );
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new(error_message));
            tracing::warn!(
                error = %error,
                fatal = %fatal_error,
                handle_id = handle.0,
                zero_copy_capacity = ?release_stats.map(|stats| stats.capacity),
                zero_copy_slots = ?release_stats.map(|stats| stats.slots),
                zero_copy_in_use = ?release_stats.map(|stats| stats.in_use),
                zero_copy_free_surfaces = ?release_stats.map(|stats| stats.free_surfaces),
                zero_copy_waiting_gpu_completion =
                    ?release_stats.map(|stats| stats.waiting_gpu_completion),
                zero_copy_waiting_decoder_reuse =
                    ?release_stats.map(|stats| stats.waiting_decoder_reuse),
                "Failed to send zero-copy release to decoder thread"
            );
        }
    }
}

/// Измеряет ожидание resource pool mutex-а и сохраняет lookup семантику.
pub(super) fn resource_lookup_from_pool(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
) -> VideoFrameResourceLookup {
    resource_lookup_from_pool_started_at(resource_pool, handle, Instant::now())
}

/// Выполняет lookup от уже зафиксированного start time; используется для точного теста timing-а.
pub(super) fn resource_lookup_from_pool_started_at(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
    lock_started_at: Instant,
) -> VideoFrameResourceLookup {
    match resource_pool.lock() {
        Ok(resource_pool) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            resource_lookup_from_locked_pool(&resource_pool, handle, lock_diagnostics)
        }
        Err(error) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            tracing::warn!(error = %error, "Resource pool mutex poisoned during lookup");
            VideoFrameResourceLookup::Fatal { lock_diagnostics }
        }
    }
}

/// Неблокирующе выполняет lookup и отдельно возвращает transient busy state.
pub(super) fn try_resource_lookup_from_pool(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
) -> VideoFrameResourceLookup {
    try_resource_lookup_from_pool_started_at(resource_pool, handle, Instant::now())
}

/// Выполняет non-blocking lookup от уже зафиксированного start time для unit-тестов.
pub(super) fn try_resource_lookup_from_pool_started_at(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
    lock_started_at: Instant,
) -> VideoFrameResourceLookup {
    match resource_pool.try_lock() {
        Ok(resource_pool) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            resource_lookup_from_locked_pool(&resource_pool, handle, lock_diagnostics)
        }
        Err(TryLockError::WouldBlock) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            VideoFrameResourceLookup::Busy { lock_diagnostics }
        }
        Err(TryLockError::Poisoned(error)) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            tracing::warn!(error = %error, "Resource pool mutex poisoned during try_lookup");
            VideoFrameResourceLookup::Fatal { lock_diagnostics }
        }
    }
}

/// Преобразует доступный resource pool в typed lookup result без знания о mutex state.
fn resource_lookup_from_locked_pool(
    resource_pool: &crate::resource_pool::FrameResourcePool,
    handle: video_core::FrameResourceHandle,
    lock_diagnostics: VideoFrameResourceLockDiagnostics,
) -> VideoFrameResourceLookup {
    if resource_pool.is_registered_handle(handle) {
        VideoFrameResourceLookup::Ready { lock_diagnostics }
    } else {
        VideoFrameResourceLookup::Missing { lock_diagnostics }
    }
}

/// Неблокирующе дублирует descriptor и отдельно возвращает transient busy state.
pub(super) fn try_resource_descriptor_lookup_from_pool(
    resource_pool: &Mutex<crate::resource_pool::FrameResourcePool>,
    handle: video_core::FrameResourceHandle,
) -> VideoFrameResourceDescriptorLookup {
    let lock_started_at = Instant::now();
    match resource_pool.try_lock() {
        Ok(resource_pool) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            match resource_pool.duplicate_descriptor(handle) {
                Ok(Some(descriptor)) => VideoFrameResourceDescriptorLookup::Ready {
                    descriptor,
                    lock_diagnostics,
                },
                Ok(None) => VideoFrameResourceDescriptorLookup::Missing { lock_diagnostics },
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        handle_id = handle.0,
                        "Failed to duplicate VAAPI DMA-BUF resource descriptor"
                    );
                    VideoFrameResourceDescriptorLookup::Fatal { lock_diagnostics }
                }
            }
        }
        Err(TryLockError::WouldBlock) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            VideoFrameResourceDescriptorLookup::Busy { lock_diagnostics }
        }
        Err(TryLockError::Poisoned(error)) => {
            let lock_diagnostics = VideoFrameResourceLockDiagnostics {
                wait: lock_started_at.elapsed(),
            };
            tracing::warn!(error = %error, "Resource pool mutex poisoned during descriptor lookup");
            VideoFrameResourceDescriptorLookup::Fatal { lock_diagnostics }
        }
    }
}
