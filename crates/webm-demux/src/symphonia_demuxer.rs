use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use symphonia::core::codecs::{CODEC_TYPE_OPUS, CODEC_TYPE_VORBIS};
use symphonia::core::formats::{FormatOptions, FormatReader, Packet, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;
use tracing::{info, warn};

use crate::demuxer::Demuxer;
use crate::error::DemuxError;
use crate::packet::{Packet as OurPacket, TimeBase as OurTimeBase, TrackInfo, TrackKind};

/// Demuxer на базе symphonia для WebM/MKV файлов.
pub struct SymphoniaDemuxer {
    format: Box<dyn FormatReader>,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
}

/// Внутренняя структура для хранения данных о треке
#[derive(Clone)]
struct TrackEntry {
    kind: TrackKind,
    codec_id: String,
    time_base: Option<TimeBase>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
}

impl SymphoniaDemuxer {
    pub fn from_file(path: &Path) -> Result<Self, DemuxError> {
        if !path.exists() {
            return Err(DemuxError::FileNotFound(path.to_path_buf()));
        }

        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let fmt_opts = FormatOptions::default();

        let probe_result = symphonia::default::get_probe()
            .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
            .map_err(|e| DemuxError::UnsupportedFormat(format!("{}", e)))?;

        let format = probe_result.format;
        let mut tracks = Vec::new();
        let mut track_map = HashMap::new();

        for track in format.tracks() {
            let entry = build_track_entry(track);
            track_map.insert(track.id, entry.clone());

            let duration =
                entry
                    .time_base
                    .zip(track.codec_params.n_frames)
                    .map(|(tb, n_frames)| {
                        let time = tb.calc_time(n_frames);
                        Duration::from_secs_f64(time.seconds as f64 + time.frac)
                    });

            tracks.push(TrackInfo {
                id: track.id,
                kind: entry.kind,
                codec_id: entry.codec_id.clone(),
                codec_private: track
                    .codec_params
                    .extra_data
                    .as_ref()
                    .map(|data| Bytes::copy_from_slice(data)),
                time_base: entry.time_base.map(|tb| OurTimeBase {
                    numer: tb.numer,
                    denom: tb.denom,
                }),
                duration,
                sample_rate: entry.sample_rate,
                channels: entry.channels,
            });
        }

        let global_duration = tracks.iter().filter_map(|t| t.duration).max();

        info!(
            path = %path.display(),
            tracks = tracks.len(),
            duration = ?global_duration,
            "WebM файл открыт"
        );

        Ok(Self {
            format,
            tracks,
            duration: global_duration,
            track_map,
        })
    }

    fn convert_packet(&self, packet: &Packet) -> Option<OurPacket> {
        let entry = self.track_map.get(&packet.track_id())?;

        let pts = entry
            .time_base
            .map(|tb| {
                let time = tb.calc_time(packet.ts());
                Duration::from_secs_f64(time.seconds as f64 + time.frac)
            })
            .unwrap_or_default();

        let keyframe = if entry.kind == TrackKind::Video && entry.codec_id == "V_VP9" {
            match vp9_parser::parse_uncompressed_header(packet.buf()) {
                Ok(info) => info.keyframe,
                Err(e) => {
                    tracing::warn!(error = %e, "VP9 packet header parse failed, skipping packet");
                    return None;
                }
            }
        } else {
            false
        };

        Some(OurPacket {
            track_id: packet.track_id(),
            kind: entry.kind,
            pts,
            dts: None,
            keyframe,
            data: Bytes::copy_from_slice(packet.buf()),
        })
    }
}

impl Demuxer for SymphoniaDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn next_packet(&mut self) -> Result<Option<OurPacket>> {
        loop {
            match self.format.next_packet() {
                Ok(packet) => {
                    if let Some(our_packet) = self.convert_packet(&packet) {
                        return Ok(Some(our_packet));
                    }
                    continue;
                }
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(symphonia::core::errors::Error::DecodeError(_))
                | Err(symphonia::core::errors::Error::IoError(_)) => {
                    warn!("Corrupted packet, skipping");
                    continue;
                }
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    return Err(anyhow::anyhow!(
                        "Demux reset required: dynamic track changes are not supported yet"
                    ));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Demux error: {}", e));
                }
            }
        }
    }

    fn seek(&mut self, _timestamp: Duration) -> Result<()> {
        anyhow::bail!("Seek not implemented yet")
    }
}

/// Определяет тип трека и codec_id из CodecParameters.
fn build_track_entry(track: &Track) -> TrackEntry {
    let params = &track.codec_params;

    // Пытаемся извлечь sample_rate/channels из codec params
    let mut sample_rate = params.sample_rate;
    let mut channels = params.channels.map(|c| c.count() as u32);

    // Для Opus в WebM symphonia 0.5 не заполняет params — парсим OpusHead вручную
    if sample_rate.is_none() || channels.is_none() {
        if let Some(ref codec_private) = params.extra_data {
            if let Some((sr, ch)) = parse_opus_head(codec_private) {
                if sample_rate.is_none() {
                    sample_rate = Some(sr);
                }
                if channels.is_none() {
                    channels = Some(ch);
                }
            }
        }
    }

    // Определяем kind по наличию audio params или codec_id
    let kind = if sample_rate.is_some() || channels.is_some() {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };

    // Определяем codec_id
    let codec_id = match params.codec {
        CODEC_TYPE_OPUS => "A_OPUS".to_string(),
        CODEC_TYPE_VORBIS => "A_VORBIS".to_string(),
        c if c == symphonia::core::codecs::CODEC_TYPE_NULL => {
            // Для video треков codec может быть NULL в symphonia
            // Определяем по наличию video-specific полей
            if kind == TrackKind::Video {
                "V_VP9".to_string() // Предполагаем VP9 для WebM
            } else {
                "unknown".to_string()
            }
        }
        c => format!("codec_{:?}", c),
    };

    TrackEntry {
        kind,
        codec_id,
        time_base: params.time_base,
        sample_rate,
        channels,
    }
}

/// Парсит OpusHead из codec private data.
///
/// OpusHead структура (RFC 7845):
/// 0-7:  "OpusHead" magic
/// 8:    version
/// 9:    channel count
/// 10-11: pre-skip
/// 12-15: input sample rate (LE u32)
/// 16-17: output gain
/// 18:   channel mapping family
///
/// Возвращает (sample_rate, channels) если OpusHead валиден.
fn parse_opus_head(data: &[u8]) -> Option<(u32, u32)> {
    // Минимальный размер OpusHead = 19 bytes
    if data.len() < 19 {
        return None;
    }

    // Проверяем magic "OpusHead"
    if &data[0..8] != b"OpusHead" {
        return None;
    }

    let channel_count = data[9] as u32;
    if channel_count == 0 || channel_count > 255 {
        return None;
    }

    // Sample rate — u32 little-endian по смещению 12
    let sample_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    if sample_rate == 0 {
        return None;
    }

    tracing::debug!(
        sample_rate,
        channels = channel_count,
        "OpusHead распарсен из codec private data"
    );

    Some((sample_rate, channel_count))
}
