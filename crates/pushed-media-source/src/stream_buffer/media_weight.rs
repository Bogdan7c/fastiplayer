//! «Вес» видео около позиции плеера — битрейт показываемого (решение 277).
//!
//! Правило: от позиции буфера (сообщённая позиция плеера или начало первого
//! поданного) у каждой полосы с видео ([`LaneContent::CarriesVideo`]) считаются
//! байты сегментов и покрытое ими время в окне до позиции
//! (`LaneShelf::weight_near`). Несколько полос с видео (необычно) дают сумму байтов
//! и наибольшее время. Полоса, где видео идёт вместе со звуком, считается целиком —
//! число включает звук. Полоса без меток времени (MPEG-TS) делает ответ неизвестным.

use super::{LaneContent, LaneTimeline, StreamBuffer};
use crate::media_bitrate::{MediaBitrate, VideoWeight};

impl StreamBuffer {
    /// Битрейт видео около позиции; `None` — неизвестно (правило — в описании модуля).
    ///
    /// Ничего не меняет: только читает полки и позицию.
    pub(crate) fn video_bitrate_near_position(&self) -> Option<MediaBitrate> {
        let state = self.lock();
        if state
            .lanes
            .values()
            .any(|lane| lane.timeline == LaneTimeline::Untimed)
        {
            return None;
        }
        let anchor = state.position()?;
        let window_micros = i64::try_from(self.bitrate_window.as_micros()).unwrap_or(i64::MAX);
        state
            .lanes
            .values()
            .filter(|lane| lane.content == LaneContent::CarriesVideo)
            .map(|lane| lane.shelf.weight_near(anchor, window_micros))
            .reduce(VideoWeight::with_lane)?
            .bitrate()
    }
}
