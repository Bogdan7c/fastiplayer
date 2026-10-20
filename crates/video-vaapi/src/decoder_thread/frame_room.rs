//! Сигнал «в канале decoded frames появилось место» и ожидание decoder-а на нём.
//!
//! Decoder thread публикует кадры в bounded frame channel неблокирующим
//! `try_send`. Когда канал полон, кадр остаётся pending, и decoder должен
//! дождаться, пока worker заберёт кадр. Раньше он узнавал об этом только опросом
//! каждые 2 мс (`recv_timeout`), а освобождение слота никак не сигналилось.
//!
//! Теперь handle после каждого успешного `try_recv_frame` кладёт «жетон» в канал
//! ёмкости 1 (`FrameRoomNotifier`), а decoder, упёршийся в полный канал, спит в
//! `wait_for_frame_room_or_control` до жетона или control message — без таймаута.
//! Схема та же, что `release_notify` у FFmpeg-бэкенда.
//!
//! Инварианты:
//! - жетоны сливаются (ёмкость 1), отправка никогда не блокирует worker;
//! - жетон, положенный между неудачным `try_send` decoder-а и входом в ожидание,
//!   не теряется: он лежит в канале, и ожидание вернётся сразу;
//! - устаревший жетон (worker забрал кадр, когда decoder не ждал) даёт не больше
//!   одного лишнего повтора публикации;
//! - control message (flush/seek/reconfigure/release/EOF drain) прерывает ожидание
//!   немедленно; разрыв любого из двух каналов завершает decoder loop, как и
//!   прежний `Disconnected` у frame channel.

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased};

use super::control::ThreadControlMsg;

/// Создаёт пару «уведомитель на стороне handle — ожидающий на decoder thread».
pub(super) fn frame_room_channel() -> (FrameRoomNotifier, FrameRoomWaiter) {
    // Ёмкость 1: decoder-у важен сам факт «место было освобождено», не число раз.
    let (room_tx, room_rx) = bounded::<()>(1);
    (FrameRoomNotifier { room_tx }, FrameRoomWaiter { room_rx })
}

/// Сторона handle-а: сообщает decoder thread-у, что worker забрал кадр.
#[derive(Debug)]
pub(super) struct FrameRoomNotifier {
    room_tx: Sender<()>,
}

impl FrameRoomNotifier {
    /// Сообщает, что из frame channel только что забрали кадр. Никогда не блокирует.
    pub(super) fn notify_frame_taken(&self) {
        match self.room_tx.try_send(()) {
            // `Full` — жетон уже лежит, decoder проснётся от него; сливаем.
            Ok(()) | Err(TrySendError::Full(())) => {}
            // Decoder thread уже завершился: ждать места некому, это не ошибка worker-а.
            Err(TrySendError::Disconnected(())) => {}
        }
    }
}

/// Сторона decoder thread-а: ждёт жетон «место появилось».
#[derive(Debug)]
pub(super) struct FrameRoomWaiter {
    room_rx: Receiver<()>,
}

/// Чем закончилось ожидание decoder-а с заблокированной публикацией кадра.
pub(super) enum PublishBlockedWake {
    /// Пришла control message; decoder обязан обработать её сразу.
    Control(ThreadControlMsg),

    /// Worker забрал кадр: можно повторить публикацию pending frame.
    FrameRoom,

    /// Все control sender-ы закрыты — decoder loop завершается.
    ControlDisconnected,

    /// Handle (а с ним frame receiver) уничтожен — публиковать больше некому.
    FrameConsumerGone,
}

/// Блокирует decoder thread до control message или жетона «место в канале».
///
/// Таймаута нет: оба события приходят извне, а прежний 2-мс опрос просыпался
/// впустую. Control message имеет приоритет, чтобы flush/seek не ждали повтора
/// публикации кадра старого поколения.
pub(super) fn wait_for_frame_room_or_control(
    control_rx: &Receiver<ThreadControlMsg>,
    frame_room: &FrameRoomWaiter,
) -> PublishBlockedWake {
    select_biased! {
        recv(control_rx) -> control_result => match control_result {
            Ok(control_message) => PublishBlockedWake::Control(control_message),
            Err(_) => PublishBlockedWake::ControlDisconnected,
        },
        recv(frame_room.room_rx) -> room_result => match room_result {
            Ok(()) => PublishBlockedWake::FrameRoom,
            Err(_) => PublishBlockedWake::FrameConsumerGone,
        },
    }
}
