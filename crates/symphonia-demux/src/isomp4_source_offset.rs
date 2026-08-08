//! Concrete ISO-BMFF adapter, который сохраняет точную source-позицию packet-а.
//!
//! Общий Symphonia `Packet` не содержит container byte offset, а `FormatReader` нельзя
//! downcast-ить после probe-а. Поэтому только уже доказанный registry-ем ISO-BMFF input
//! открывается concrete reader-ом и публикует offset через приватный per-read observer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use symphonia::core::errors::Result as SymphoniaResult;
use symphonia::core::formats::{
    Attachment, FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo,
    Track,
};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{ChapterGroup, Metadata};
use symphonia::core::packet::Packet;
use symphonia_format_isomp4::IsoMp4Reader;

use crate::symphonia_api::FormatReaderBox;

/// Shared handle связывает ровно один `next_packet` result с его source offset-ом.
#[derive(Clone, Debug, Default)]
pub(crate) struct PacketSourceOffsetObserver {
    /// Атомарное состояние сохраняет `FormatReader: Send + Sync` без panic-able lock path-а.
    latest_offset: Arc<PacketSourceOffsetState>,
}

/// Offset публикуется перед флагом; consumer сначала атомарно забирает флаг, затем значение.
#[derive(Debug, Default)]
struct PacketSourceOffsetState {
    /// Отличает законный offset `0` от отсутствия provenance.
    is_available: AtomicBool,
    /// Точный offset последнего успешного packet read-а.
    value: AtomicU64,
}

impl PacketSourceOffsetObserver {
    /// Публикует новый offset или очищает старое значение перед read/EOF/error.
    fn publish(&self, source_offset: Option<u64>) {
        if let Some(source_offset) = source_offset {
            self.latest_offset
                .value
                .store(source_offset, Ordering::Relaxed);
            self.latest_offset
                .is_available
                .store(true, Ordering::Release);
        } else {
            self.latest_offset
                .is_available
                .store(false, Ordering::Release);
        }
    }

    /// Забирает offset ровно один раз для соответствующего neutral packet conversion-а.
    pub(crate) fn take(&self) -> Option<u64> {
        self.latest_offset
            .is_available
            .swap(false, Ordering::AcqRel)
            .then(|| self.latest_offset.value.load(Ordering::Acquire))
    }
}

/// Открывает concrete ISO-BMFF reader после уже выполненной signature-проверки registry-я.
pub(crate) fn open_reader_with_source_offsets<'source>(
    media_source_stream: MediaSourceStream<'source>,
) -> SymphoniaResult<(FormatReaderBox<'source>, PacketSourceOffsetObserver)> {
    let reader =
        IsoMp4Reader::try_new_from_stream_start(media_source_stream, FormatOptions::default())?;
    let observer = PacketSourceOffsetObserver::default();
    let adapter = SourcePositionedIsoMp4Reader {
        reader,
        observer: observer.clone(),
    };
    Ok((Box::new(adapter), observer))
}

/// Делегирует весь `FormatReader` contract без изменения track/seek/metadata semantics.
struct SourcePositionedIsoMp4Reader<'source> {
    /// Единственный container parser owner остаётся в локальном Symphonia patch-е.
    reader: IsoMp4Reader<'source>,
    /// Observer переносит только metadata последнего успешного packet read-а.
    observer: PacketSourceOffsetObserver,
}

impl FormatReader for SourcePositionedIsoMp4Reader<'_> {
    fn format_info(&self) -> &FormatInfo {
        self.reader.format_info()
    }

    fn media_info(&self) -> &MediaInfo {
        self.reader.media_info()
    }

    fn attachments(&self) -> &[Attachment] {
        self.reader.attachments()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.reader.chapters()
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.reader.metadata()
    }

    fn seek(&mut self, mode: SeekMode, target: SeekTo) -> SymphoniaResult<SeekedTo> {
        self.observer.publish(None);
        self.reader.seek(mode, target)
    }

    fn tracks(&self) -> &[Track] {
        self.reader.tracks()
    }

    fn next_packet(&mut self) -> SymphoniaResult<Option<Packet>> {
        // Старое значение нельзя оставлять наблюдаемым после EOF или ошибки нового read-а.
        self.observer.publish(None);
        let Some(source_positioned_packet) = self.reader.next_packet_with_source_offset()? else {
            return Ok(None);
        };
        self.observer
            .publish(Some(source_positioned_packet.source_offset()));
        Ok(Some(source_positioned_packet.into_packet()))
    }

    fn into_inner<'source>(self: Box<Self>) -> MediaSourceStream<'source>
    where
        Self: 'source,
    {
        Box::new(self.reader).into_inner()
    }
}
