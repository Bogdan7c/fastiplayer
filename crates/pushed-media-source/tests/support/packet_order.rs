//! Журнал пакетов, которые плеер прочитал из демуксера источника, — в порядке чтения.
//!
//! Декодер H.264 с пирамидой B-кадров обязан получать кадры в порядке декодирования:
//! опорный P раньше своих B, иначе цепочка опорных кадров рвётся. Программный FFmpeg
//! такую поломку скрывает (подставляет недостающие опорные), аппаратный VA-API —
//! сбрасывается до следующего ключевого кадра. Поэтому тест «до кадра» вдобавок к
//! показанным кадрам сверяет и сам порядок пакетов, который плеер отдал декодеру:
//! это наблюдаемый результат источника, одинаковый для любого декодера.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use media_core::{
    DemuxDataEnd, DemuxDataWait, DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult,
    DemuxSeekability, Demuxer, TrackInfo, TrackKind,
};

/// Один прочитанный плеером пакет видео.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadVideoPacket {
    /// Время декодирования (у пакета без отдельного DTS — время показа).
    pub decode_time: Duration,
    /// Время показа.
    pub presentation_time: Duration,
}

/// Общий журнал: демуксер пишет из потока плеера, тест читает после игры.
#[derive(Debug, Clone, Default)]
pub struct VideoPacketLog {
    /// Пакеты видео в порядке чтения.
    packets: Arc<Mutex<Vec<ReadVideoPacket>>>,
}

impl VideoPacketLog {
    /// Пакеты видео в порядке, в котором их прочитал плеер.
    pub fn packets(&self) -> Vec<ReadVideoPacket> {
        self.packets.lock().expect("packet log lock").clone()
    }

    /// Записывает пакет видео.
    fn note(&self, packet: ReadVideoPacket) {
        self.packets.lock().expect("packet log lock").push(packet);
    }
}

/// Демуксер-обёртка: всё делает внутренний, а прочитанные пакеты видео записывает.
pub struct PacketLoggingDemuxer<Inner> {
    /// Настоящий демуксер источника.
    inner: Inner,
    /// Куда писать прочитанное.
    log: VideoPacketLog,
}

impl<Inner: Demuxer> PacketLoggingDemuxer<Inner> {
    /// Обёртка над `inner`, пишущая в `log`.
    pub fn new(inner: Inner, log: VideoPacketLog) -> Self {
        Self { inner, log }
    }
}

impl<Inner: Demuxer> Demuxer for PacketLoggingDemuxer<Inner> {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    fn data_end(&self) -> DemuxDataEnd {
        self.inner.data_end()
    }

    fn data_wait(&self) -> DemuxDataWait {
        self.inner.data_wait()
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let event = self.inner.next_event()?;
        if let DemuxReadEvent::Packet(packet) = &event
            && packet.kind == TrackKind::Video
        {
            self.log.note(ReadVideoPacket {
                decode_time: packet.dts.unwrap_or(packet.pts),
                presentation_time: packet.pts,
            });
        }
        Ok(event)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek_with_request(request)
    }
}
