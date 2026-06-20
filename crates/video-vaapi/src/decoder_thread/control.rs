use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use super::{DecodeThreadError, DecoderThreadState};

/// Результат, которым decoder thread подтверждает завершение flush.
pub(super) type FlushAck = std::result::Result<(), String>;

/// Результат, которым decoder thread подтверждает замену codec adapter-а.
pub(super) type ConfigureStreamAck = video_core::VideoStreamConfigResult;

/// Результат, которым decoder thread подтверждает запуск EOF/DPB drain.
pub(super) type EndOfStreamDrainAck = video_core::VideoDecoderEndOfStreamDrainResult;

/// Результат, которым decoder thread подтверждает set/clear preroll output floor.
pub(super) type PrerollOutputFloorAck = video_core::VideoPrerollOutputFloorResult;

/// Snapshot давления на bounded decoder control channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDecoderControlChannelPressureStats {
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

impl From<VideoDecoderControlChannelPressureStats>
    for video_core::VideoDecoderControlChannelPressureSnapshot
{
    /// Копирует VA-API control-channel counters в neutral diagnostics snapshot.
    fn from(stats: VideoDecoderControlChannelPressureStats) -> Self {
        Self {
            control_channel_len: stats.control_channel_len,
            control_channel_capacity: stats.control_channel_capacity,
            control_channel_full_count: stats.control_channel_full_count,
            release_control_send_fail_count: stats.release_control_send_fail_count,
            flush_control_send_fail_count: stats.flush_control_send_fail_count,
        }
    }
}

impl From<video_core::VideoDecoderControlChannelPressureSnapshot>
    for VideoDecoderControlChannelPressureStats
{
    /// Адаптирует neutral control-channel snapshot обратно к VA-API stats type.
    fn from(stats: video_core::VideoDecoderControlChannelPressureSnapshot) -> Self {
        Self {
            control_channel_len: stats.control_channel_len,
            control_channel_capacity: stats.control_channel_capacity,
            control_channel_full_count: stats.control_channel_full_count,
            release_control_send_fail_count: stats.release_control_send_fail_count,
            flush_control_send_fail_count: stats.flush_control_send_fail_count,
        }
    }
}

/// Control-команда для decoder thread.
pub(super) enum ThreadControlMsg {
    /// Настроить concrete codec adapter под выбранный stream.
    ConfigureStream(
        video_core::VideoStreamDecodeConfig,
        Sender<ConfigureStreamAck>,
    ),

    /// Освободить decoded handle, удерживаемый zero-copy кадром.
    ReleaseZeroCopy(video_core::FrameResourceHandle),

    /// Сбросить decoder state и подтвердить завершение операции.
    Flush(Sender<FlushAck>),

    /// Установить decoder-side preroll output floor для accurate seek.
    SetPrerollOutputFloor(
        video_core::VideoPrerollOutputFloor,
        Sender<PrerollOutputFloorAck>,
    ),

    /// Очистить decoder-side preroll output floor без изменения generation.
    ClearPrerollOutputFloor(
        video_core::VideoPrerollOutputFloorClear,
        Sender<PrerollOutputFloorAck>,
    ),

    /// Дожать codec tail/DPB без seek flush и generation reset.
    BeginEndOfStreamDrain(u64, Sender<EndOfStreamDrainAck>),
}

/// Sender-side control operation для logs и раздельных counters.
#[derive(Debug, Clone, Copy)]
pub(super) enum DecoderControlOperation {
    /// Возврат zero-copy surface после renderer/GPU ownership.
    Release,

    /// Синхронный flush decoder thread-а.
    Flush,

    /// Установка accurate-seek preroll output floor.
    SetPrerollOutputFloor,

    /// Очистка accurate-seek preroll output floor.
    ClearPrerollOutputFloor,

    /// Explicit EOF/DPB drain без seek reset semantics.
    EofDrain,
}

impl DecoderControlOperation {
    /// Возвращает стабильное имя операции для structured logs.
    const fn metric_name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Flush => "flush",
            Self::SetPrerollOutputFloor => "set_preroll_output_floor",
            Self::ClearPrerollOutputFloor => "clear_preroll_output_floor",
            Self::EofDrain => "eof_drain",
        }
    }

    /// Возвращает прежний текстовый контекст fatal error-а.
    const fn fatal_context(self) -> &'static str {
        match self {
            Self::Release => "zero-copy release",
            Self::Flush => "decoder flush",
            Self::SetPrerollOutputFloor => "preroll output-floor set",
            Self::ClearPrerollOutputFloor => "preroll output-floor clear",
            Self::EofDrain => "decoder EOF drain",
        }
    }
}

/// Shared sender-side counters decoder control channel-а.
#[derive(Debug, Default)]
pub(super) struct DecoderControlChannelPressureCounters {
    /// Накопительное число Full отказов независимо от операции.
    full_count: AtomicU64,

    /// Накопительное число release send failures.
    release_send_fail_count: AtomicU64,

    /// Накопительное число flush send failures.
    flush_send_fail_count: AtomicU64,
}

impl DecoderControlChannelPressureCounters {
    /// Учитывает failed send до fail-closed перехода и возвращает актуальный snapshot.
    pub(super) fn record_send_failure(
        &self,
        operation: DecoderControlOperation,
        control_tx: &Sender<ThreadControlMsg>,
        error: &TrySendError<ThreadControlMsg>,
    ) -> VideoDecoderControlChannelPressureStats {
        if matches!(error, TrySendError::Full(_)) {
            self.full_count.fetch_add(1, Ordering::Relaxed);
        }

        match operation {
            DecoderControlOperation::Release => {
                self.release_send_fail_count.fetch_add(1, Ordering::Relaxed);
            }
            DecoderControlOperation::Flush => {
                self.flush_send_fail_count.fetch_add(1, Ordering::Relaxed);
            }
            DecoderControlOperation::SetPrerollOutputFloor
            | DecoderControlOperation::ClearPrerollOutputFloor => {}
            DecoderControlOperation::EofDrain => {}
        }

        self.snapshot(control_tx)
    }

    /// Снимает текущую глубину канала и накопительные counters.
    pub(super) fn snapshot(
        &self,
        control_tx: &Sender<ThreadControlMsg>,
    ) -> VideoDecoderControlChannelPressureStats {
        VideoDecoderControlChannelPressureStats {
            control_channel_len: control_tx.len(),
            control_channel_capacity: control_tx.capacity().unwrap_or(0),
            control_channel_full_count: self.full_count.load(Ordering::Relaxed),
            release_control_send_fail_count: self.release_send_fail_count.load(Ordering::Relaxed),
            flush_control_send_fail_count: self.flush_send_fail_count.load(Ordering::Relaxed),
        }
    }
}

/// Ждёт flush ACK ограниченное время и переводит thread state в fatal при срыве.
pub(super) fn wait_for_flush_ack(
    done_rx: Receiver<FlushAck>,
    timeout: Duration,
    thread_state: &DecoderThreadState,
) -> anyhow::Result<()> {
    match done_rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread flush failed: {message}"
            )));
            Err(anyhow::anyhow!("{}", fatal_error))
        }
        Err(RecvTimeoutError::Timeout) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread did not confirm flush within {} ms",
                timeout.as_millis()
            )));
            Err(anyhow::anyhow!("{}", fatal_error))
        }
        Err(RecvTimeoutError::Disconnected) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(
                "Decoder thread did not confirm flush",
            ));
            Err(anyhow::anyhow!("{}", fatal_error))
        }
    }
}

/// Ждёт stream-config ACK и сохраняет distinct result для caller-а.
pub(super) fn wait_for_configure_stream_ack(
    done_rx: Receiver<ConfigureStreamAck>,
    timeout: Duration,
    thread_state: &DecoderThreadState,
) -> video_core::VideoStreamConfigResult {
    match done_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread did not confirm stream configure within {} ms",
                timeout.as_millis()
            )));
            video_core::VideoStreamConfigResult::Fatal(fatal_error.into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(
                "Decoder thread did not confirm stream configure",
            ));
            video_core::VideoStreamConfigResult::Fatal(fatal_error.into())
        }
    }
}

/// Ждёт ACK запуска EOF drain, сохраняя fatal/backpressure различия boundary.
pub(super) fn wait_for_end_of_stream_drain_ack(
    done_rx: Receiver<EndOfStreamDrainAck>,
    timeout: Duration,
    thread_state: &DecoderThreadState,
) -> video_core::VideoDecoderEndOfStreamDrainResult {
    match done_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread did not confirm EOF drain within {} ms",
                timeout.as_millis()
            )));
            video_core::VideoDecoderEndOfStreamDrainResult::Fatal(fatal_error.into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(
                "Decoder thread did not confirm EOF drain",
            ));
            video_core::VideoDecoderEndOfStreamDrainResult::Fatal(fatal_error.into())
        }
    }
}

/// Ждёт ACK preroll floor control command без потери fatal state.
pub(super) fn wait_for_preroll_output_floor_ack(
    done_rx: Receiver<PrerollOutputFloorAck>,
    timeout: Duration,
    thread_state: &DecoderThreadState,
    operation: &'static str,
) -> video_core::VideoPrerollOutputFloorResult {
    match done_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread did not confirm {operation} within {} ms",
                timeout.as_millis()
            )));
            video_core::VideoPrerollOutputFloorResult::Fatal(fatal_error.into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            let fatal_error = thread_state.mark_fatal(DecodeThreadError::new(format!(
                "Decoder thread did not confirm {operation}"
            )));
            video_core::VideoPrerollOutputFloorResult::Fatal(fatal_error.into())
        }
    }
}

/// Учитывает failed control send и пишет pressure fields перед fail-closed переходом.
pub(super) fn record_decoder_control_send_failure(
    operation: DecoderControlOperation,
    control_tx: &Sender<ThreadControlMsg>,
    pressure_counters: &DecoderControlChannelPressureCounters,
    error: &TrySendError<ThreadControlMsg>,
) -> String {
    let pressure = pressure_counters.record_send_failure(operation, control_tx, error);
    tracing::debug!(
        operation = operation.metric_name(),
        len = pressure.control_channel_len,
        capacity = pressure.control_channel_capacity,
        control_channel_full_count = pressure.control_channel_full_count,
        release_control_send_fail_count = pressure.release_control_send_fail_count,
        flush_control_send_fail_count = pressure.flush_control_send_fail_count,
        error = %error,
        "Decoder control channel send failed before fail-closed transition"
    );
    decoder_control_send_error_message(operation.fatal_context(), error)
}

/// Печатает control-channel failure как fatal lifecycle ошибку.
fn decoder_control_send_error_message<T>(operation: &str, error: &TrySendError<T>) -> String {
    match error {
        TrySendError::Full(_) => format!("Decoder control channel is full before {operation}"),
        TrySendError::Disconnected(_) => {
            format!("Decoder thread disconnected before {operation}")
        }
    }
}
