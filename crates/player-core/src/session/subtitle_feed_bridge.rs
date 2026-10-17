//! Граница session -> труба встроенных субтитров.
//!
//! Session владеет только отправкой; очередь, счётчики и политика переполнения живут
//! в [`crate::subtitle_feed`]. Метод ничего не знает про декодеры и backpressure.

use media_core::Packet;

use super::PlayerSession;
use crate::MediaInstanceId;
use crate::subtitle_feed::{
    SubtitleFeedMediaStart, SubtitleFeedPacket, SubtitleFeedSender, collect_subtitle_track_headers,
};

impl PlayerSession {
    /// Подключает трубу субтитров, созданную worker-ом (по умолчанию она отключена).
    #[must_use]
    pub(crate) fn with_subtitle_feed(mut self, subtitle_feed: SubtitleFeedSender) -> Self {
        self.subtitle_feed = subtitle_feed;
        self
    }

    /// Забирает вложения у demuxer-а нового media и сообщает приложению о его начале.
    ///
    /// Вызывается ровно один раз на установку (commit staged install), до чтения любых
    /// пакетов этого media, чтобы сообщение стояло в очереди раньше пакетов.
    pub(super) fn announce_subtitle_feed_media_start(
        &mut self,
        media_instance_id: MediaInstanceId,
    ) {
        let attachments = self.pipeline.take_media_attachments();
        // Заголовки (ASS) едут один раз здесь, а не в часто клонируемом снимке.
        let track_headers = collect_subtitle_track_headers(self.pipeline.tracks());
        // Исход отправки уже учтён внутри трубы (счётчик и лог); установке он не мешает.
        let _outcome = self
            .subtitle_feed
            .try_send_media_start(SubtitleFeedMediaStart {
                media_instance_id,
                attachments,
                track_headers,
            });
    }

    /// Пересылает пакет субтитровой дорожки в трубу без блокировки и без влияния на tick.
    ///
    /// `pts` передаётся как есть: это та же временная шкала, что у video `PendingVideoPacket`.
    pub(super) fn forward_subtitle_packet(&mut self, packet: &Packet) {
        let Some(media_instance_id) = self.snapshot.media_instance_id else {
            // Пакеты без установленного media_instance_id невозможны в production
            // (id выдаётся при commit раньше первого чтения demux-а).
            tracing::debug!("Subtitle packet ignored: no installed media instance");
            return;
        };
        // Исход отправки уже учтён внутри трубы; нехватка места не должна влиять на tick.
        let _outcome = self.subtitle_feed.try_send_packet(SubtitleFeedPacket {
            media_instance_id,
            track_id: packet.track_id,
            presentation_time: packet.pts,
            duration: packet.duration,
            payload: packet.data.clone(),
        });
    }
}
