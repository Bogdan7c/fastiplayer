use std::time::Duration;

use crate::packet::{Packet, TrackInfo};

/// Trait, абстрагирующий источник media packets.
///
/// Позволяет заменить реализацию (symphonia → matroska → streaming)
/// без изменения consumer code (audio/video pipeline).
pub trait Demuxer {
    /// Информация о всех треках (доступна после open()).
    fn tracks(&self) -> &[TrackInfo];

    /// Длительность контента, если известна из контейнера.
    fn duration(&self) -> Option<Duration>;

    /// Следующий packet — None при EOF.
    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>>;

    /// Seek к позиции (заглушка, полная реализация на Этапе 4).
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<()>;
}
