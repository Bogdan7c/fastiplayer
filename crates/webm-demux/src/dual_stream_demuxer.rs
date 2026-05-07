//! Demuxer для YouTube adaptive streaming.
//!
//! YouTube отдаёт video-only и audio-only WebM отдельно. Этот demuxer объединяет два
//! независимых `SymphoniaDemuxer` в один поток packets, чтобы app layer мог остаться простым.

use std::time::Duration;

use anyhow::Result;
use media_core::{Packet, TrackId, TrackInfo, TrackKind};

use crate::demuxer::Demuxer;
use crate::symphonia_demuxer::SymphoniaDemuxer;

/// Track id для video после remap.
const REMAPPED_VIDEO_TRACK_ID: u32 = 1;

/// Track id для audio после remap.
const REMAPPED_AUDIO_TRACK_ID: u32 = 2;

/// Demuxer, который интерливит packets из отдельных video/audio demuxer-ов по PTS.
pub struct DualStreamDemuxer {
    /// Demuxer video-only WebM.
    video_demuxer: SymphoniaDemuxer,

    /// Demuxer audio-only WebM.
    audio_demuxer: SymphoniaDemuxer,

    /// Информация о remapped tracks.
    tracks: Vec<TrackInfo>,

    /// Общая duration, если удалось извлечь хотя бы из одного stream.
    duration: Option<Duration>,

    /// Следующий video packet, уже прочитанный для сравнения PTS.
    pending_video_packet: Option<Packet>,

    /// Следующий audio packet, уже прочитанный для сравнения PTS.
    pending_audio_packet: Option<Packet>,

    /// Video stream дошёл до EOF.
    video_eof: bool,

    /// Audio stream дошёл до EOF.
    audio_eof: bool,
}

impl DualStreamDemuxer {
    /// Создаёт объединённый demuxer из video-only и audio-only WebM demuxer-ов.
    pub fn new(video_demuxer: SymphoniaDemuxer, audio_demuxer: SymphoniaDemuxer) -> Result<Self> {
        // Выбираем первый VP9 video track.
        let video_track = video_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Video && track.codec_id == "V_VP9")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Streaming video WebM не содержит VP9 video track"))?;

        // Выбираем первый audio track с параметрами для Opus decoder.
        let audio_track = audio_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Streaming audio WebM не содержит audio track"))?;

        // Remap нужен, потому что в двух отдельных WebM оба track id часто равны 1.
        let mut remapped_video_track = video_track;
        remapped_video_track.id = TrackId::new(REMAPPED_VIDEO_TRACK_ID);

        let mut remapped_audio_track = audio_track;
        remapped_audio_track.id = TrackId::new(REMAPPED_AUDIO_TRACK_ID);

        let duration = remapped_video_track
            .duration
            .or(remapped_audio_track.duration)
            .or_else(|| video_demuxer.duration())
            .or_else(|| audio_demuxer.duration());

        Ok(Self {
            video_demuxer,
            audio_demuxer,
            tracks: vec![remapped_video_track, remapped_audio_track],
            duration,
            pending_video_packet: None,
            pending_audio_packet: None,
            video_eof: false,
            audio_eof: false,
        })
    }

    /// Читает следующий video packet, если pending slot пустой.
    fn fill_pending_video_packet(&mut self) -> Result<()> {
        // Уже есть packet для сравнения PTS.
        if self.pending_video_packet.is_some() || self.video_eof {
            return Ok(());
        }

        // Пропускаем не-video packets на всякий случай, хотя stream должен быть video-only.
        while let Some(mut packet) = self.video_demuxer.next_packet()? {
            if packet.kind == TrackKind::Video {
                packet.track_id = TrackId::new(REMAPPED_VIDEO_TRACK_ID);
                self.pending_video_packet = Some(packet);
                return Ok(());
            }
        }

        self.video_eof = true;
        Ok(())
    }

    /// Читает следующий audio packet, если pending slot пустой.
    fn fill_pending_audio_packet(&mut self) -> Result<()> {
        // Уже есть packet для сравнения PTS.
        if self.pending_audio_packet.is_some() || self.audio_eof {
            return Ok(());
        }

        // Пропускаем не-audio packets на всякий случай, хотя stream должен быть audio-only.
        while let Some(mut packet) = self.audio_demuxer.next_packet()? {
            if packet.kind == TrackKind::Audio {
                packet.track_id = TrackId::new(REMAPPED_AUDIO_TRACK_ID);
                self.pending_audio_packet = Some(packet);
                return Ok(());
            }
        }

        self.audio_eof = true;
        Ok(())
    }
}

impl Demuxer for DualStreamDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        // Подготавливаем по одному packet с каждой стороны, чтобы выбрать ранний PTS.
        self.fill_pending_video_packet()?;
        self.fill_pending_audio_packet()?;

        match (&self.pending_video_packet, &self.pending_audio_packet) {
            (Some(video_packet), Some(audio_packet)) => {
                if video_packet.pts <= audio_packet.pts {
                    Ok(self.pending_video_packet.take())
                } else {
                    Ok(self.pending_audio_packet.take())
                }
            }
            (Some(_), None) => Ok(self.pending_video_packet.take()),
            (None, Some(_)) => Ok(self.pending_audio_packet.take()),
            (None, None) => Ok(None),
        }
    }

    fn seek(&mut self, _timestamp: Duration) -> Result<()> {
        anyhow::bail!("Streaming seek не реализован в MVP")
    }
}
