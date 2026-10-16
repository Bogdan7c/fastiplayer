//! Общие счётчики потерь трубы: пишет производитель, читает потребитель.

use std::sync::atomic::{AtomicU64, Ordering};

/// Счётчики потерянных сообщений (только растут).
#[derive(Debug, Default)]
pub(super) struct SubtitleFeedStats {
    /// Сколько пакетов выброшено из-за переполнения.
    dropped_packets: AtomicU64,

    /// Сколько сообщений «начало media» (с вложениями) выброшено из-за переполнения.
    dropped_media_starts: AtomicU64,
}

impl SubtitleFeedStats {
    /// Фиксирует потерю пакета и возвращает новое значение счётчика.
    pub(super) fn record_dropped_packet(&self) -> u64 {
        self.dropped_packets
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    /// Фиксирует потерю сообщения «начало media» и возвращает новое значение.
    pub(super) fn record_dropped_media_start(&self) -> u64 {
        self.dropped_media_starts
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    /// Всего потеряно пакетов.
    pub(super) fn dropped_packets(&self) -> u64 {
        self.dropped_packets.load(Ordering::Relaxed)
    }

    /// Всего потеряно сообщений «начало media».
    pub(super) fn dropped_media_starts(&self) -> u64 {
        self.dropped_media_starts.load(Ordering::Relaxed)
    }
}
