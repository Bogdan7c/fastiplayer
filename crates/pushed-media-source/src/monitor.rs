//! Монитор буфера: позиция плеера на вход, «буферизовано», «полон» и просьбы
//! к странице перемотаться — на выход.
//!
//! Монитор берут у писателя ([`crate::PushedStreamWriter::monitor`]) и отдают
//! потоку приложения: тот сообщает позицию плеера и пересылает странице состояние
//! буфера (`buffer_state{ahead_us, intake}` протокола моста). Монитор дешёво
//! клонируется и работает с любого потока; каждый вызов коротко берёт замок
//! буфера потока и никогда не ждёт байтов.

use std::sync::Arc;
use std::time::Duration;

use crate::buffer_window::BufferStatus;
use crate::page_seek::PageSeekRequest;
use crate::stream_buffer::StreamBuffer;

/// Ручка буфера потока для другого потока (`Send + Sync`, клонируется).
#[derive(Debug, Clone)]
pub struct PushedStreamMonitor {
    /// Буфер потока.
    buffer: Arc<StreamBuffer>,
}

impl PushedStreamMonitor {
    /// Монитор над буфером потока.
    pub(crate) const fn new(buffer: Arc<StreamBuffer>) -> Self {
        Self { buffer }
    }

    /// Позиция плеера (время показа, как у пакетов демуксера).
    ///
    /// От неё считается запас вперёд; сегменты, кончившиеся раньше
    /// «позиция − окно назад» и уже поданные демуксеру, выбрасываются. Пока
    /// позицию не сообщили, позицией считается начало первого поданного сегмента.
    ///
    /// После перемотки позиция, далёкая от цели (дальше
    /// [`crate::PushedSourcePolicy::page_seek_match_tolerance`]), около секунды не
    /// принимается: это позиция старого места, снятая до перемотки. Сообщайте
    /// позицию часто (каждый шаг приложения) — тогда первая же позиция у цели
    /// снимает это недоверие. Заодно здесь продолжается подача, отложенная из-за
    /// предела ждущих эпизодов.
    pub fn note_playback_position(&self, position: Duration) {
        self.buffer.note_playback_position(position);
    }

    /// Забирает просьбу к странице «перемотайся на T» (не более одного раза).
    ///
    /// Появляется, когда плеер перемотался туда, где на полке нет байтов, и
    /// страница сама туда не шла ([`crate::PushedStreamWriter::note_page_seek`]).
    /// Последняя перемотка побеждает: незабранная старая просьба заменяется.
    /// Приложение переводит её в `seek{seek_id, target_us}` протокола моста.
    #[must_use]
    pub fn take_page_seek_request(&self) -> Option<PageSeekRequest> {
        self.buffer.take_page_seek_request()
    }

    /// Запас вперёд и «полон» (правило — в [`BufferStatus::buffered_ahead`] и
    /// [`crate::BufferIntake::Full`]: окно в секундах или доля потолка байтов,
    /// [`crate::PushedSourcePolicy::buffer_full_at_held_bytes`]).
    #[must_use]
    pub fn buffer_status(&self) -> BufferStatus {
        self.buffer.buffer_status()
    }

    /// Сколько байтов целых сегментов держит поток: полки, трубы эпизодов и
    /// init-сегменты, каждая память один раз. Недособранные хвосты сканеров сюда
    /// не входят (их видит [`crate::PushedStreamWriter::held_bytes`]).
    #[must_use]
    pub fn held_segment_bytes(&self) -> usize {
        self.buffer.held_segment_bytes()
    }
}
