/// Backend-neutral snapshot decoder/render ресурсов для diagnostics и backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderResourceSnapshot {
    /// Максимальное число persistent texture/import slots.
    pub capacity: usize,

    /// Сколько persistent texture/import slots сейчас создано.
    pub slots: usize,

    /// Сколько surfaces сейчас нельзя переиспользовать decoder-у.
    pub in_use: usize,

    /// Сколько imported surfaces свободно для reuse.
    pub free_surfaces: usize,

    /// Сколько releases ждёт GPU completion callback.
    pub waiting_gpu_completion: usize,

    /// Сколько releases ждёт возврата decoded handle в decoder pool.
    pub waiting_decoder_reuse: usize,

    /// Сколько external imports завершилось ошибкой.
    pub import_failures: u64,

    /// Сколько external imports реально создано.
    pub imports_created: u64,

    /// Сколько кадров переиспользовало existing import.
    pub imports_reused: u64,

    /// Сколько free imports было заменено из-за смены backing object/layout.
    pub imports_replaced: u64,
}

impl DecoderResourceSnapshot {
    /// Возвращает число slots, которые ещё можно занять или переиспользовать.
    #[must_use]
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Software host-upload resource counters без смешивания с VA-API surface/import accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostUploadResourceSnapshot {
    /// Сколько decoded host frames уже готово к upload/presentation path-у.
    pub host_frames_ready: usize,

    /// Сколько host frames уже заняло upload slot и ждёт release.
    pub host_frames_in_flight: usize,

    /// Сколько upload slots может одновременно держать software path.
    pub upload_slots_capacity: usize,

    /// Сколько upload slots сейчас свободно для новых host frames.
    pub upload_slots_free: usize,

    /// Сколько upload попыток завершилось ошибкой.
    pub upload_failures: u64,
}

impl HostUploadResourceSnapshot {
    /// Возвращает typed причину software backpressure без обращения к VA-API counters.
    #[must_use]
    pub const fn backpressure_reason(
        self,
        ready_queue_capacity: usize,
    ) -> Option<HostUploadBackpressureReason> {
        if ready_queue_capacity > 0 && self.host_frames_ready >= ready_queue_capacity {
            return Some(HostUploadBackpressureReason::ReadyQueueFull {
                host_frames_ready: self.host_frames_ready,
                capacity: ready_queue_capacity,
            });
        }

        if self.upload_slots_free == 0 {
            return Some(HostUploadBackpressureReason::UploadSlotsExhausted {
                host_frames_in_flight: self.host_frames_in_flight,
                upload_slots_capacity: self.upload_slots_capacity,
            });
        }

        None
    }
}

/// Typed причина software host-upload backpressure для будущей scheduler integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostUploadBackpressureReason {
    /// Очередь decoded host frames заполнена, decoder не должен публиковать ещё один frame.
    ReadyQueueFull {
        /// Сколько host frames уже ждёт upload/presentation path-а.
        host_frames_ready: usize,

        /// Bounded capacity очереди ready host frames.
        capacity: usize,
    },

    /// Upload slots закончились, поэтому renderer/provider ещё не освободил ресурсы.
    UploadSlotsExhausted {
        /// Сколько host frames уже находится в upload/release lifecycle.
        host_frames_in_flight: usize,

        /// Общая capacity upload slots.
        upload_slots_capacity: usize,
    },
}

/// Typed результат чтения software host-upload snapshot-а через decoder boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostUploadResourceSnapshotStatus {
    /// В `player-core` ещё не установлен video decoder.
    AbsentDecoder,

    /// Decoder установлен, но software host-upload resource boundary ещё отсутствует.
    AbsentResource,

    /// Backend не является software host-upload provider-ом.
    UnsupportedBackend,

    /// Software host-upload resource counters доступны.
    Available(HostUploadResourceSnapshot),
}

/// Snapshot давления на decoder control channel без backend-specific типов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDecoderControlChannelPressureSnapshot {
    /// Текущая глубина control channel на момент чтения snapshot-а.
    pub control_channel_len: usize,

    /// Bounded capacity control channel-а.
    pub control_channel_capacity: usize,

    /// Сколько send failures произошло именно из-за заполненного control channel-а.
    pub control_channel_full_count: u64,

    /// Сколько раз release path не смог отправить control message.
    pub release_control_send_fail_count: u64,

    /// Сколько раз flush path не смог отправить control message.
    pub flush_control_send_fail_count: u64,
}
