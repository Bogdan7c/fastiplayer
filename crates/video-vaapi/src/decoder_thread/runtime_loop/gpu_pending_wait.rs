//! Отдача кадров «в полёте» перед ожиданием потока декодера (сессия 09a).
//!
//! Поток декодера ждёт в двух местах: когда подать GPU нечего (пустой
//! packet-канал) и когда пул выходных surfaces исчерпан. В обоих случаях кадры
//! очереди «в полёте» нужно отдать до ожидания: иначе последний кадр застрял бы
//! до следующего packet-а, а при исчерпанном пуле его surface никогда бы не
//! вернулась.

use crossbeam_channel::Sender;
use video_core::VideoDecoderActivityNotifier;

use super::super::DecodeThreadError;
use super::send_decoder_thread_error;
use crate::decoder::DecoderGpuWaitReason;

/// Отдаёт кадры «в полёте» перед ожиданием; `false` — decoder остановлен фатальной ошибкой.
pub(super) fn complete_gpu_pending_frames_before_waiting(
    decoder: &mut crate::VaapiVideoDecoder,
    wait_reason: DecoderGpuWaitReason,
    error_tx: &Sender<DecodeThreadError>,
    activity_notifier: &VideoDecoderActivityNotifier,
) -> bool {
    match decoder.complete_gpu_pending_frames_before_waiting(wait_reason) {
        Ok(()) => true,
        Err(error) => {
            send_decoder_thread_error(
                error_tx,
                format!("Video decoder stopped while completing in-flight frames: {error:#}"),
                activity_notifier,
            );
            false
        }
    }
}
