//! Test-only сборка доигрывания звука из очереди pending packet-ов.
//!
//! Worker-level тесты не видят `PlaybackPipeline`, поэтому состояние «Playing,
//! демуксер дошёл до конца, в очереди ещё много audio packet-ов» собирается здесь,
//! у владельца session state. Дальше настоящий tick декодирует packet-ы и доливает
//! PCM в output по обычным правилам подпитки (high-water), а worker сам решает, когда
//! просыпаться — именно это проверяют вертикальные тесты пробуждений (cpu-parity 02b).

use std::time::Duration;

use audio_core::{AudioDecoderHandle, PlayerAudioOutput};
use bytes::Bytes;
use media_core::TrackId;

use super::PlayerSession;
use crate::PlayerCommand;
use crate::pipeline::PendingAudioPacket;

/// Track id звуковой дорожки сценария.
const AUDIO_DRAIN_TRACK_ID: TrackId = TrackId::new(2);

/// Демуксер, у которого данные уже кончились: первый же read даёт штатный EOF.
struct EndOfStreamDemuxer;

impl media_core::Demuxer for EndOfStreamDemuxer {
    /// Дорожки описывает сам сценарий через выбор audio track-а.
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    /// Длительность в сценарии не участвует.
    fn duration(&self) -> Option<Duration> {
        None
    }

    /// Всегда штатный конец потока.
    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::finite_packet_read_event(None))
    }

    /// Seek честно возвращает запрошенную точку.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        Ok(media_core::DemuxSeekResult {
            requested_position: media_core::MediaTime::from_duration(timestamp),
            actual_position: media_core::MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

impl PlayerSession {
    /// Запускает воспроизведение звука: `packet_count` packet-ов длиной `packet_duration`
    /// ждут в очереди, демуксер уже на EOF, decoder и output установлены заранее.
    pub(crate) fn install_audio_eof_drain_playback_for_tests(
        &mut self,
        decoder: AudioDecoderHandle,
        output: Box<dyn PlayerAudioOutput>,
        packet_count: u32,
        packet_duration: Duration,
    ) {
        self.pipeline
            .install_opened_media(Box::new(EndOfStreamDemuxer), None, None, Vec::new());
        self.pipeline.select_audio_track(AUDIO_DRAIN_TRACK_ID);
        self.pipeline.install_audio_decoder(decoder);
        self.pipeline.install_audio_output_for_tests(output);
        self.dispatch_command(PlayerCommand::Play)
            .expect("Play is accepted for installed media");

        let generation = self.pipeline.seek_generation();
        for packet_index in 0..packet_count {
            self.pipeline
                .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
                    AUDIO_DRAIN_TRACK_ID,
                    packet_duration * packet_index,
                    None,
                    None,
                    generation,
                    Bytes::from_static(b"audio-drain-packet"),
                ));
        }
    }
}
