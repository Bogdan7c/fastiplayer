// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecId;
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64LE,
    CODEC_ID_PCM_S8, CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S16LE, CODEC_ID_PCM_S24BE,
    CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S32BE, CODEC_ID_PCM_S32LE, CODEC_ID_PCM_U8,
    CODEC_ID_PCM_U16BE, CODEC_ID_PCM_U16LE, CODEC_ID_PCM_U24BE, CODEC_ID_PCM_U24LE,
    CODEC_ID_PCM_U32BE, CODEC_ID_PCM_U32LE,
};
use symphonia_core::support_format;

use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_ISOMP4;
use symphonia_core::io::*;
use symphonia_core::meta::well_known::METADATA_ID_ISOMP4;
use symphonia_core::meta::{
    Metadata, MetadataBuilder, MetadataInfo, MetadataLog, PerTrackMetadataBuilder, Tag,
};
use symphonia_core::packet::PacketBuilder;
use symphonia_core::units::Time;

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};
use std::num::NonZero;
use std::sync::Arc;

use crate::atoms::{AtomError, AtomIterator, AtomType, ReadAtom};
use crate::atoms::{FtypAtom, MetaAtom, MoofAtom, MoovAtom, SidxAtom, TrakAtom};
use crate::stream::*;

use log::{debug, info, trace, warn};

const ISOMP4_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_ISOMP4,
    short_name: "isomp4",
    long_name: "ISO Base Media File Format",
};

const ISOMP4_METADATA_INFO: MetadataInfo = MetadataInfo {
    metadata: METADATA_ID_ISOMP4,
    short_name: "isomp4",
    long_name: "ISO Base Media File Format",
};

const RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG: &str =
    "rustiplayer.display_orientation.clockwise_degrees";
const RUSTIPLAYER_H264_PARAMETER_SETS_IN_BAND_TAG: &str =
    "rustiplayer.video.h264.parameter_sets_in_band";
const RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG: &str = "rustiplayer.video.color.full_range";
const RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG: &str =
    "rustiplayer.video.color.matrix_coefficients_h273";
const RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG: &str = "rustiplayer.video.color.primaries_h273";
const RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG: &str =
    "rustiplayer.video.color.transfer_characteristics_h273";
const RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG: &str =
    "rustiplayer.video.hdr.mastering_display.max_luminance_nits";
const RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG: &str =
    "rustiplayer.video.hdr.mastering_display.min_luminance_nits";
const RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG: &str =
    "rustiplayer.video.hdr.max_content_light_level_nits";
const RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG: &str =
    "rustiplayer.video.hdr.max_frame_average_light_level_nits";
const PCM_FRAMES_PER_READER_PACKET: u32 = 1024;

pub struct TrackState {
    /// The track number.
    track_num: usize,
    /// The track ID.
    track_id: u32,
    /// The current segment.
    cur_seg: usize,
    /// The current sample index relative to the track.
    next_sample: u32,
    /// The current sample byte position relative to the start of the track.
    next_sample_pos: u64,
}

impl TrackState {
    pub fn make(track_num: usize, trak: &TrakAtom, timespan: &TimeSpan) -> (Self, Track) {
        let mut track = Track::new(trak.tkhd.id);

        // Create the codec parameters using the sample description atom.
        if let Some(codec_params) = trak.mdia.minf.stbl.stsd.make_codec_params() {
            track.with_codec_params(codec_params);
        }

        // Populate timing information.
        track.with_time_base(TimeBase::from_recip(timespan.timescale));
        if let Some(duration) = timespan.duration {
            track.with_duration(duration);
        }

        // If the track is an audio track, and the timescale is equal to the sample rate, then the
        // number of frames is equal to the duration. This is the case for almost all audio tracks.
        // If not, there is no generic, low overhead, & precise way to determine the number of
        // frames.
        if let Some(CodecParameters::Audio(audio)) = &track.codec_params {
            if let Some(sample_rate) = audio.sample_rate {
                if sample_rate == timespan.timescale.get() {
                    if let Some(duration) = timespan.duration {
                        track.with_num_frames(duration.get());
                    }
                }
            }
        }

        let state = Self {
            track_num,
            track_id: trak.tkhd.id,
            cur_seg: 0,
            next_sample: 0,
            next_sample_pos: 0,
        };

        (state, track)
    }
}

/// Information regarding the next sample.
#[derive(Debug)]
struct NextSampleInfo {
    /// The track number of the next sample.
    track_num: usize,
    /// The track id.
    track_id: u32,
    /// The presentation timestamp of the next sample.
    pts: Timestamp,
    /// The decode timestamp of the next sample.
    dts: Timestamp,
    /// The decode timestamp expressed in seconds.
    time: Time,
    /// The duration of the next sample.
    dur: Duration,
    /// The segment containing the next sample.
    seg_idx: usize,
}

/// Information regarding a sample.
#[derive(Debug)]
struct SampleDataInfo {
    /// The position of the sample in the track.
    pos: u64,
    /// The length of the sample.
    len: u32,
}

/// Диапазон байтов и тайминга для одного packet, который отдаёт MP4 reader.
#[derive(Debug, PartialEq, Eq)]
struct PacketSampleSpan {
    /// Позиция первого sample в media data.
    pos: u64,
    /// Общая длина всех samples в packet.
    len: usize,
    /// Сумма длительностей samples в timebase трека.
    dur: Duration,
    /// Сколько MP4 samples нужно продвинуть в `TrackState`.
    sample_count: u32,
}

/// Packet ISO-BMFF вместе с точной позицией первого sample в исходном byte stream-е.
///
/// Позиция относится к логическому input-у reader-а. Если несколько transport resources
/// последовательно склеены одним `Read`, offset считается от начала этой виртуальной
/// конкатенации, а не от начала отдельного resource-а.
#[derive(Debug)]
pub struct IsoMp4PacketWithSourceOffset {
    /// Обычный Symphonia packet без изменения codec payload или timing semantics.
    packet: Packet,
    /// Позиция первого байта packet sample span-а в логическом input-е.
    source_offset: u64,
}

impl IsoMp4PacketWithSourceOffset {
    /// Возвращает точную позицию первого sample без передачи ownership packet-а.
    pub const fn source_offset(&self) -> u64 {
        self.source_offset
    }

    /// Передаёт обычный Symphonia packet следующему neutral adapter-у.
    pub fn into_packet(self) -> Packet {
        self.packet
    }
}

fn pcm_packet_sample_limit(track: &Track, sample_duration: Duration) -> Option<u32> {
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        return None;
    };

    if !is_packet_coalescing_pcm_codec(params.codec) {
        return None;
    }

    if sample_duration != Duration::from(1_u32) {
        return None;
    }

    // QuickTime LPCM often describes one PCM frame per MP4 sample. Without reader-side
    // chunking this creates 48_000 tiny packets per second and starves playback after seek.
    let requested_sample_limit = params
        .max_frames_per_packet
        .filter(|max_frames_per_packet| *max_frames_per_packet > 1)
        .unwrap_or(u64::from(PCM_FRAMES_PER_READER_PACKET));
    let bounded_sample_limit = requested_sample_limit.min(u64::from(PCM_FRAMES_PER_READER_PACKET));

    Some(u32::try_from(bounded_sample_limit).unwrap_or(PCM_FRAMES_PER_READER_PACKET))
}

fn is_packet_coalescing_pcm_codec(codec: AudioCodecId) -> bool {
    matches!(
        codec,
        CODEC_ID_PCM_S8
            | CODEC_ID_PCM_U8
            | CODEC_ID_PCM_S16BE
            | CODEC_ID_PCM_S16LE
            | CODEC_ID_PCM_U16BE
            | CODEC_ID_PCM_U16LE
            | CODEC_ID_PCM_S24BE
            | CODEC_ID_PCM_S24LE
            | CODEC_ID_PCM_U24BE
            | CODEC_ID_PCM_U24LE
            | CODEC_ID_PCM_S32BE
            | CODEC_ID_PCM_S32LE
            | CODEC_ID_PCM_U32BE
            | CODEC_ID_PCM_U32LE
            | CODEC_ID_PCM_F32BE
            | CODEC_ID_PCM_F32LE
            | CODEC_ID_PCM_F64BE
            | CODEC_ID_PCM_F64LE
    )
}

fn collect_pcm_packet_span(
    seg: &dyn StreamSegment,
    track_num: usize,
    start_sample: u32,
    start_sample_pos: u64,
    max_samples_per_packet: u32,
) -> Result<PacketSampleSpan> {
    let sample_range = seg.track_sample_range(track_num);

    if !sample_range.contains(&start_sample) {
        return decode_error("isomp4: invalid sample index");
    }

    let sample_limit = max_samples_per_packet.min(sample_range.end - start_sample);
    let mut span_pos = start_sample_pos;
    let mut span_len = 0usize;
    let mut span_dur = Duration::ZERO;
    let mut sample_count = 0u32;
    let mut next_sample_pos = start_sample_pos;
    let mut first_base_pos = None;
    let mut expected_pts = None;
    let mut expected_dts = None;

    while sample_count < sample_limit {
        let sample_num = start_sample
            .checked_add(sample_count)
            .ok_or(Error::DecodeError("isomp4: sample index overflow"))?;

        let Some(timing) = seg.sample_timing(track_num, sample_num)? else {
            break;
        };

        // LPCM frame-samples должны быть соседними во времени. Если таблицы показывают разрыв,
        // завершаем текущий packet до разрыва и не меняем семантику следующих samples.
        if sample_count > 0
            && (expected_pts != Some(timing.pts) || expected_dts != Some(timing.dts))
        {
            break;
        }

        let sample_data_desc = seg.sample_data(track_num, sample_num, false)?;

        if let Some(base_pos) = first_base_pos {
            // Не склеиваем через границу chunk: позиционирование в `stsc/stco/stsz` уже меняет
            // базовую позицию, и безопаснее отдать остаток текущего chunk отдельным packet.
            if base_pos != sample_data_desc.base_pos {
                break;
            }
        } else {
            first_base_pos = Some(sample_data_desc.base_pos);
        }

        let sample_pos = if sample_data_desc.base_pos > next_sample_pos {
            sample_data_desc.base_pos
        } else {
            next_sample_pos
        };

        if sample_count == 0 {
            span_pos = sample_pos;
        } else if sample_pos != span_pos + u64::try_from(span_len).unwrap_or(u64::MAX) {
            break;
        }

        let sample_len = usize::try_from(sample_data_desc.size)
            .map_err(|_| Error::DecodeError("isomp4: sample size overflow"))?;

        let Some(next_span_len) = span_len.checked_add(sample_len) else {
            if sample_count == 0 {
                return decode_error("isomp4: packet size overflow");
            }
            break;
        };

        let sample_dur = Duration::from(timing.dur);
        let Some(next_span_dur) = span_dur.checked_add(sample_dur) else {
            if sample_count == 0 {
                return decode_error("isomp4: packet duration overflow");
            }
            break;
        };

        let Some(sample_end_pos) = sample_pos.checked_add(u64::from(sample_data_desc.size)) else {
            if sample_count == 0 {
                return decode_error("isomp4: sample position overflow");
            }
            break;
        };

        let Some(next_expected_pts) = timing.pts.checked_add(u64::from(timing.dur)) else {
            if sample_count == 0 {
                return decode_error("isomp4: sample timestamp overflow");
            }
            break;
        };

        let Some(next_expected_dts) = timing.dts.checked_add(u64::from(timing.dur)) else {
            if sample_count == 0 {
                return decode_error("isomp4: sample timestamp overflow");
            }
            break;
        };

        span_len = next_span_len;
        span_dur = next_span_dur;
        next_sample_pos = sample_end_pos;
        expected_pts = Some(next_expected_pts);
        expected_dts = Some(next_expected_dts);
        sample_count += 1;
    }

    if sample_count == 0 {
        return decode_error("isomp4: missing sample");
    }

    Ok(PacketSampleSpan {
        pos: span_pos,
        len: span_len,
        dur: span_dur,
        sample_count,
    })
}

/// A representation of time, defining a duration relative to a specific frequency
#[derive(Debug)]
pub struct TimeSpan {
    pub timescale: NonZero<u32>,
    pub duration: Option<Duration>,
}

/// Direct `sidx` entry нужен только для пропуска линейного fragmented-MP4 scan-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SidxSeekPoint {
    start_timestamp: u64,
    end_timestamp: u64,
    byte_offset: u64,
    end_byte_offset: u64,
    starts_with_proven_sap: bool,
}

/// Проверенный direct-media index для одного track/reference ID.
#[derive(Debug)]
struct SidxSeekIndex {
    timescale: NonZero<u32>,
    points: Vec<SidxSeekPoint>,
}

impl SidxSeekIndex {
    fn from_atom(sidx: &SidxAtom) -> Option<Self> {
        let mut timestamp = sidx.earliest_pts;
        let mut byte_offset = sidx.first_offset;
        let mut points = Vec::with_capacity(sidx.references.len());

        for reference in &sidx.references {
            if !matches!(
                reference.reference_type,
                crate::atoms::sidx::ReferenceType::Media
            ) || reference.reference_size == 0
                || reference.subsegment_duration == 0
            {
                return None;
            }

            let end_timestamp = timestamp.checked_add(u64::from(reference.subsegment_duration))?;
            let next_byte_offset = byte_offset.checked_add(u64::from(reference.reference_size))?;
            points.push(SidxSeekPoint {
                start_timestamp: timestamp,
                end_timestamp,
                byte_offset,
                end_byte_offset: next_byte_offset,
                starts_with_proven_sap: reference.starts_with_sap
                    && matches!(reference.sap_type, 1 | 2)
                    && reference.sap_delta_time < reference.subsegment_duration,
            });
            timestamp = end_timestamp;
            byte_offset = next_byte_offset;
        }

        (!points.is_empty()).then_some(Self {
            timescale: sidx.timescale,
            points,
        })
    }

    fn append_if_ordered(&mut self, other: Self) {
        let Some(last) = self.points.last() else {
            *self = other;
            return;
        };
        let Some(first_other) = other.points.first() else {
            return;
        };
        if self.timescale == other.timescale
            && first_other.start_timestamp >= last.end_timestamp
            && first_other.byte_offset >= last.end_byte_offset
        {
            self.points.extend(other.points);
        }
    }

    fn fits_source_length(&self, source_length: u64) -> bool {
        self.points
            .last()
            .is_some_and(|point| point.end_byte_offset <= source_length)
    }

    #[cfg(test)]
    fn byte_offset_for_timestamp(&self, timestamp: u64) -> Option<u64> {
        self.seek_point_for_timestamp(timestamp)
            .map(|point| point.byte_offset)
    }

    fn seek_point_for_timestamp(&self, timestamp: u64) -> Option<SidxSeekPoint> {
        let first_point = self.points.first()?;
        if timestamp < first_point.start_timestamp {
            return first_point.starts_with_proven_sap.then_some(*first_point);
        }

        let point_index = self
            .points
            .iter()
            .position(|point| timestamp < point.end_timestamp)
            .or_else(|| {
                self.points
                    .last()
                    .filter(|point| timestamp == point.end_timestamp)
                    .map(|_| self.points.len() - 1)
            })?;

        // Внутри authored subsegment-а и ровно на его границе собственный SAP является точным
        // container anchor; если его нет, откатываемся к ближайшему предыдущему SAP.
        let current_point = self.points[point_index];
        let search_end =
            if timestamp >= current_point.start_timestamp && current_point.starts_with_proven_sap {
                point_index + 1
            } else {
                point_index
            };
        let sap_index = self.points[..search_end]
            .iter()
            .rposition(|point| point.starts_with_proven_sap)
            .or_else(|| (point_index == 0 && first_point.starts_with_proven_sap).then_some(0))?;
        Some(self.points[sap_index])
    }
}

fn record_sidx_seek_index(
    indexes: &mut HashMap<u32, SidxSeekIndex>,
    sidx: &SidxAtom,
    source_length: Option<u64>,
) {
    let Some(seek_index) = SidxSeekIndex::from_atom(sidx) else {
        return;
    };
    if source_length.is_some_and(|length| !seek_index.fits_source_length(length)) {
        return;
    }

    if let Some(existing) = indexes.get_mut(&sidx.reference_id) {
        existing.append_if_ordered(seek_index);
    } else {
        indexes.insert(sidx.reference_id, seek_index);
    }
}

impl Default for TimeSpan {
    fn default() -> Self {
        Self {
            timescale: NonZero::new(1).unwrap(),
            duration: None,
        }
    }
}

impl TimeSpan {
    pub fn new(timescale: NonZero<u32>, duration: Option<Duration>) -> Self {
        TimeSpan {
            timescale,
            duration,
        }
    }
}

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader<'s> {
    iter: AtomIterator<MediaSourceStream<'s>>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    /// Segments of the movie. Sorted in ascending order by sequence number.
    segs: Vec<Box<dyn StreamSegment>>,
    /// State tracker for each track.
    track_states: Vec<TrackState>,
    /// Optional, movie extends atom used for fragmented streams.
    moov: Arc<MoovAtom>,
    /// Direct segment indexes, адресованные referenced track ID.
    sidx_seek_indexes: HashMap<u32, SidxSeekIndex>,
    /// `sidx` SAP разрешает первый sample indexed scan-а; packet verification остаётся отдельной.
    indexed_seek_sap_track_id: Option<u32>,
    /// Физическая длина ограничивает startup и поздние `sidx` offsets одинаково.
    source_length: Option<u64>,
}

impl<'s> IsoMp4Reader<'s> {
    /// Открывает ISO-BMFF reader с начала stream-а без общего Symphonia probe-а.
    ///
    /// Этот opt-in constructor нужен integration-ам, которым требуется concrete
    /// [`Self::next_packet_with_source_offset`] boundary. Caller уже должен доказать ISO-BMFF
    /// signature; обычный registry path продолжает использовать `ProbeableFormat`.
    pub fn try_new_from_stream_start(
        mut mss: MediaSourceStream<'s>,
        opts: FormatOptions,
    ) -> Result<Self> {
        if mss.pos() != 0 {
            return decode_error("isomp4: source-position reader must start at byte zero");
        }

        // `try_new` вызывается probe-ом сразу после четырёхбайтового marker-а и сам возвращается
        // на эти четыре байта назад. Читаем ровно один quad, чтобы сохранить тот же проверенный
        // parser entry point без второго варианта atom parsing-а.
        mss.read_quad_bytes()?;
        Self::try_new(mss, opts)
    }

    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        // To get to beginning of the atom.
        mss.seek_buffered_rel(-4);

        let is_seekable = mss.is_seekable();

        let mut ftyp = None;
        let mut moov = None;

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            info!("stream is seekable with len={len} bytes.");
            Some(len)
        } else {
            None
        };

        let mut metadata = opts.external_data.metadata.unwrap_or_default();

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut it = AtomIterator::new(mss, total_len);
        // Maps each track id to its cumulative duration (TimeSpan) as parsed from the segment
        // index.
        let mut sidx_timespans: HashMap<u32, TimeSpan> = HashMap::new();
        let mut sidx_seek_indexes: HashMap<u32, SidxSeekIndex> = HashMap::new();

        while let Some(header) = it.next_header()? {
            // Top-level atoms.
            match header.atom_type() {
                AtomType::FileType => {
                    ftyp = Some(it.read_atom::<FtypAtom>()?);
                }
                AtomType::Movie => {
                    moov = Some(it.read_atom::<MoovAtom>()?);
                }
                AtomType::SegmentIndex => {
                    let sidx = it.read_atom::<SidxAtom>()?;

                    record_sidx_seek_index(&mut sidx_seek_indexes, &sidx, total_len);

                    // Calculate the total duration, per track, from the segment index atoms.
                    let sidx_timespan = sidx_timespans
                        .entry(sidx.reference_id)
                        .or_insert(TimeSpan::new(sidx.timescale, Some(Duration::ZERO)));

                    if sidx_timespan.timescale != sidx.timescale {
                        return unsupported_error(
                            "isomp4: different sidx timescale for the same track",
                        );
                    }

                    // Matching `reference_id` и timescale делают authored subsegment durations
                    // существующим container authority; сумма всё равно обязана не переполняться.
                    sidx_timespan.duration = Some(
                        sidx_timespan
                            .duration
                            .expect("sidx accumulator always has a duration")
                            .checked_add(Duration::new(sidx.total_duration))
                            .ok_or(Error::DecodeError("isomp4: sidx total duration overflow"))?,
                    )
                }
                AtomType::MediaData | AtomType::MovieFragment => {
                    // The mdat atom contains the codec bitstream data. For fragmented streams, a
                    // moof + mdat pair is required. If the ftyp and moov atoms have been read, then
                    // the top-level atom scan can exit here and begin playback immediately as an
                    // optimization. If not, then the scan must continue.
                    //
                    // The scan must also exit if the source is unseekable because in that case
                    // the format reader cannot skip past these atoms without dropping packets.
                    let is_playable = moov.is_some() && ftyp.is_some();

                    if is_playable || !is_seekable {
                        if !is_playable {
                            warn!("mp4 is not streamable.");
                        }
                        break;
                    }
                }
                AtomType::Meta => {
                    // Read the metadata atom and append it to the log.
                    let mut meta = it.read_atom::<MetaAtom>()?;

                    if let Some(rev) = meta.take_metadata() {
                        metadata.push(rev);
                    }
                }
                AtomType::Free => (),
                AtomType::Skip => (),
                _ => {
                    info!("skipping top-level atom: {:?}.", header.atom_type());
                }
            }
        }

        if ftyp.is_none() {
            return unsupported_error("isomp4: missing ftyp atom");
        }

        if moov.is_none() {
            return unsupported_error("isomp4: missing moov atom");
        }

        // If the top-level atom scan iterated across the entire source (e.g., if moov was the last
        // atom), then the iterator must return to the first moof or mdat atom. This is only
        // possible if the source is seekable. If it's not, then the media will be effectively
        // unplayable.
        if is_seekable && it.pending().is_none() {
            let mut mss = it.into_inner();
            mss.seek(SeekFrom::Start(0))?;

            it = AtomIterator::new(mss, total_len);

            while let Some(header) = it.next_header()? {
                if let AtomType::MovieFragment | AtomType::MediaData = header.atom_type() {
                    break;
                }
            }
        }

        // Fragments (moof + mdat pairs) are streamed. So if the pending atom is a moof, seek the
        // iterator to the start of the moof atom.
        if let Some(atom) = it.pending() {
            if atom.atom_type() == AtomType::MovieFragment {
                it.seek_atom_start()?;
            }
        }

        let mut moov = moov.unwrap();

        if moov.is_fragmented() {
            if !sidx_timespans.is_empty() {
                info!("stream is segmented with a segment index.");
            } else {
                info!("stream is segmented without a segment index.");
            }
        }

        if let Some(rev) = moov.take_metadata() {
            metadata.push(rev);
        }
        append_track_rustiplayer_metadata(&mut metadata, &moov.traks);

        // Create a track and track state for each Track (trak) atom.
        let mut tracks = Vec::with_capacity(moov.traks.len());
        let mut track_states = Vec::with_capacity(moov.traks.len());

        for (t, trak) in moov.traks.iter().enumerate() {
            // Determine the timespan of the track.
            let timespan = if moov.is_fragmented() {
                // If fragmented, prefer the duration from the sidx, if it is provided. Otherwise,
                // fallback to the mdhd.
                sidx_timespans
                    .get(&trak.tkhd.id)
                    .map(|sidx_tspan| TimeSpan::new(sidx_tspan.timescale, sidx_tspan.duration))
                    .unwrap_or_else(|| {
                        let duration = (trak.mdia.mdhd.duration != 0)
                            .then(|| Duration::new(trak.mdia.mdhd.duration));
                        TimeSpan::new(trak.mdia.mdhd.timescale, duration)
                    })
            } else {
                // If non-fragmented, use the total duration (media timescale) from the track's
                // stts atom. Since edits are not currently supported, this is the duration of all
                // samples that will be yielded.
                //
                // TODO: Support edits. Once supported, prefer the tkhd duration.
                let duration = Duration::from(trak.mdia.minf.stbl.stts.total_duration);

                TimeSpan::new(trak.mdia.mdhd.timescale, Some(duration))
            };

            let (track_state, track) = TrackState::make(t, trak, &timespan);

            tracks.push(track);
            track_states.push(track_state);
        }

        // The number of tracks specified in the moov atom must match the number in the mvex atom.
        if let Some(mvex) = &moov.mvex {
            if mvex.trexs.len() != moov.traks.len() {
                return decode_error("isomp4: mvex and moov track number mismatch");
            }
        }

        // The moov atom will be shared among all segments and the demuxer using an Arc.
        let moov = Arc::new(moov);

        let segs: Vec<Box<dyn StreamSegment>> = vec![Box::new(MoovSegment::new(moov.clone()))];

        // Populate media information.
        let mut media_info = if moov.is_fragmented() && moov.mvhd.duration == 0 {
            if let Some(fragment_duration) = moov
                .mvex
                .as_ref()
                .and_then(|mvex| mvex.mehd.as_ref())
                .map(|mehd| mehd.fragment_duration)
            {
                let mut info = MediaInfo::new();
                info.with_time_base(TimeBase::from_recip(moov.mvhd.timescale));
                info.with_duration(Duration::new(fragment_duration));
                info
            } else {
                // Без movie-level duration разрешено публиковать только доказанный track duration
                // из `sidx` или non-zero `mdhd`; иначе длительность остаётся неизвестной.
                MediaInfo::from_tracks(&tracks)
            }
        } else {
            let mut info = MediaInfo::new();
            info.with_time_base(TimeBase::from_recip(moov.mvhd.timescale));
            info.with_duration(Duration::new(moov.mvhd.duration));
            info
        };

        if media_info.time_base.is_none() {
            media_info.with_time_base(TimeBase::from_recip(moov.mvhd.timescale));
        }

        Ok(IsoMp4Reader {
            iter: it,
            media_info,
            tracks,
            metadata,
            track_states,
            segs,
            moov,
            sidx_seek_indexes,
            indexed_seek_sap_track_id: None,
            source_length: total_len,
        })
    }

    /// Читает следующий packet и атомарно возвращает начало его sample span-а.
    ///
    /// В отличие от текущей позиции `MediaSourceStream`, это значение не искажено read-ahead
    /// buffering-ом: оно берётся из container sample tables / fragment run непосредственно перед
    /// exact payload read.
    pub fn next_packet_with_source_offset(
        &mut self,
    ) -> Result<Option<IsoMp4PacketWithSourceOffset>> {
        self.read_next_packet_with_source_offset()
    }

    /// Единая реализация packet read-а для обычного `FormatReader` и opt-in source boundary.
    fn read_next_packet_with_source_offset(
        &mut self,
    ) -> Result<Option<IsoMp4PacketWithSourceOffset>> {
        // Get the index of the track with the next-nearest (minimum) timestamp.
        let next_sample_info = loop {
            // Using the current set of segments, try to get the next sample info.
            if let Some(info) = self.next_sample_info()? {
                break info;
            } else {
                // The inner reader of the atom iterator has been used/seeked around to read
                // packets, so resync the reader and iterator by seeking to the end of the current
                // pending atom. Under regular circumstances, no actual expensive seek operation is
                // performed since the reader should be at the end of the last iterated atom if we
                // are trying to read another.
                match self.iter.seek_atom_end() {
                    Ok(_) | Err(AtomError::NoPendingAtom) => (),
                    Err(_) => return decode_error("sync lost"),
                };

                // No more segments. If the stream is unseekable, it may be the case that there are
                // more segments coming. If the stream is seekable it might be fragmented and no
                // segments are found in the moov atom. Iterate atoms until a new segment is found
                // or the end-of-stream is reached
                if !self.try_read_more_segments()? {
                    return Ok(None);
                }
            }
        };

        // Получаем позицию, длину и duration уже для packet-а, а не обязательно одного sample.
        let packet_span = self.consume_next_packet_span(&next_sample_info)?;

        let data = self
            .iter
            .read_raw_boxed_slice_exact(packet_span.pos, packet_span.len)?;

        let packet = PacketBuilder::new()
            .track_id(next_sample_info.track_id)
            .pts(next_sample_info.pts)
            .dur(packet_span.dur)
            .data(data)
            .dts(next_sample_info.dts)
            .build();

        Ok(Some(IsoMp4PacketWithSourceOffset {
            packet,
            source_offset: packet_span.pos,
        }))
    }

    fn indexed_seek_track_id(&self, requested_track_id: u32) -> u32 {
        if self.tracks.iter().any(|track| {
            track.id == requested_track_id
                && matches!(track.codec_params.as_ref(), Some(CodecParameters::Video(_)))
                && self.sidx_seek_indexes.contains_key(&track.id)
        }) {
            return requested_track_id;
        }

        self.tracks
            .iter()
            .find(|track| {
                matches!(track.codec_params.as_ref(), Some(CodecParameters::Video(_)))
                    && self.sidx_seek_indexes.contains_key(&track.id)
            })
            .map_or(requested_track_id, |track| track.id)
    }

    /// Использует authored direct `sidx` offsets до обычного per-sample seek scan-а.
    fn prepare_indexed_fragment_seek(&mut self, track_id: u32, time: Time) -> Result<()> {
        self.indexed_seek_sap_track_id = None;
        if !self.moov.is_fragmented() {
            return Ok(());
        }
        let track_id = self.indexed_seek_track_id(track_id);
        let Some(seek_point) = self.sidx_seek_indexes.get(&track_id).and_then(|index| {
            let timestamp = TimeBase::from_recip(index.timescale).calc_timestamp(time)?;
            if timestamp.is_negative() {
                return None;
            }
            index.seek_point_for_timestamp(timestamp.get() as u64)
        }) else {
            return Ok(());
        };

        self.iter.seek_top_level(seek_point.byte_offset)?;
        self.segs.truncate(1);
        for track in &mut self.track_states {
            track.cur_seg = 0;
            track.next_sample = 0;
            track.next_sample_pos = 0;
        }
        self.indexed_seek_sap_track_id = Some(track_id);
        debug!(
            "seeking fragmented MP4 through sidx: track_id={track_id}, byte_offset={}",
            seek_point.byte_offset
        );
        Ok(())
    }

    /// Idempotently gets information regarding the next sample of the media stream. This function
    /// selects the next sample with the lowest timestamp of all tracks.
    fn next_sample_info(&self) -> Result<Option<NextSampleInfo>> {
        let mut earliest = None;

        // TODO: Consider returning samples based on lowest byte position in the track instead of
        // timestamp. This may be important if video tracks are ever decoded (i.e., DTS vs. PTS).

        for (state, track) in self.track_states.iter().zip(&self.tracks) {
            // Get the timebase of the track used to calculate the presentation time.
            let tb = track.time_base.unwrap();

            // Get the next timestamp for the next sample of the current track. The next sample may
            // be in a future segment.
            for (seg_idx_delta, seg) in self.segs[state.cur_seg..].iter().enumerate() {
                // Try to get the timestamp for the next sample of the track from the segment.
                if let Some(timing) = seg.sample_timing(state.track_num, state.next_sample)? {
                    // Calculate the decode time used for inter-track sample ordering.
                    let Some(pts) = timing.pts.try_into().ok() else {
                        return Ok(None);
                    };

                    let Some(dts) = timing.dts.try_into().ok() else {
                        return Ok(None);
                    };

                    let Some(sample_time) = tb.calc_time(dts) else {
                        return Ok(None);
                    };

                    // Compare the decode time of the sample from this track to other tracks,
                    // and select the track with the earliest decode time.
                    match earliest {
                        Some(NextSampleInfo { time, .. }) if time <= sample_time => {
                            // Earliest is less than or equal to the track's next sample decode
                            // time. No need to update earliest.
                        }
                        _ => {
                            // Earliest was either None, or greater than the track's next sample
                            // decode time. Update earliest.
                            earliest = Some(NextSampleInfo {
                                track_num: state.track_num,
                                track_id: state.track_id,
                                pts,
                                dts,
                                time: sample_time,
                                dur: Duration::from(timing.dur),
                                seg_idx: seg_idx_delta + state.cur_seg,
                            });
                        }
                    }

                    // Either the next sample of the track had the earliest presentation time seen
                    // thus far, or it was greater than those from other tracks, but there is no
                    // reason to check samples in future segments.
                    break;
                }
            }
        }

        Ok(earliest)
    }

    fn consume_next_sample(&mut self, info: &NextSampleInfo) -> Result<Option<SampleDataInfo>> {
        // Get the track state.
        let track = &mut self.track_states[info.track_num];

        // Get the segment associated with the sample.
        let seg = &self.segs[info.seg_idx];

        // Get the sample data descriptor.
        let sample_data_desc = seg.sample_data(track.track_num, track.next_sample, false)?;

        // The sample base position in the sample data descriptor remains constant if the sample
        // followed immediately after the previous sample. In this case, the track state's
        // next_sample_pos is the position of the current sample. If the base position has jumped,
        // then the base position is the position of current the sample.
        let pos = if sample_data_desc.base_pos > track.next_sample_pos {
            sample_data_desc.base_pos
        } else {
            track.next_sample_pos
        };

        // Advance the track's current segment to the next sample's segment.
        track.cur_seg = info.seg_idx;

        // Advance the track's next sample number and position.
        track.next_sample += 1;
        track.next_sample_pos = pos + u64::from(sample_data_desc.size);

        Ok(Some(SampleDataInfo {
            pos,
            len: sample_data_desc.size,
        }))
    }

    fn consume_next_packet_span(&mut self, info: &NextSampleInfo) -> Result<PacketSampleSpan> {
        if let Some(max_samples_per_packet) =
            pcm_packet_sample_limit(&self.tracks[info.track_num], info.dur)
        {
            return self.consume_next_pcm_packet_span(info, max_samples_per_packet);
        }

        let sample_info = self.consume_next_sample(info)?.unwrap();

        Ok(PacketSampleSpan {
            pos: sample_info.pos,
            len: usize::try_from(sample_info.len)
                .map_err(|_| Error::DecodeError("isomp4: sample size overflow"))?,
            dur: info.dur,
            sample_count: 1,
        })
    }

    fn consume_next_pcm_packet_span(
        &mut self,
        info: &NextSampleInfo,
        max_samples_per_packet: u32,
    ) -> Result<PacketSampleSpan> {
        let track = &self.track_states[info.track_num];
        let seg = &self.segs[info.seg_idx];

        let packet_span = collect_pcm_packet_span(
            seg.as_ref(),
            track.track_num,
            track.next_sample,
            track.next_sample_pos,
            max_samples_per_packet,
        )?;

        let packet_end_pos = packet_span
            .pos
            .checked_add(
                u64::try_from(packet_span.len)
                    .map_err(|_| Error::DecodeError("isomp4: packet size overflow"))?,
            )
            .ok_or(Error::DecodeError("isomp4: packet position overflow"))?;

        let track = &mut self.track_states[info.track_num];

        track.cur_seg = info.seg_idx;
        track.next_sample = track
            .next_sample
            .checked_add(packet_span.sample_count)
            .ok_or(Error::DecodeError("isomp4: sample index overflow"))?;
        track.next_sample_pos = packet_end_pos;

        Ok(packet_span)
    }

    fn try_read_more_segments(&mut self) -> Result<bool> {
        // If all tracks ended in the last segment, then do not try to read anymore segments.
        //
        // Note, there will always be one segment because the moov atom was converted into a segment
        // when the reader was instantiated.
        if self.segs.last().unwrap().all_tracks_ended() {
            return Ok(false);
        }

        // Continue iterating over atoms until a segment (a moof + mdat atom pair) is found. All
        // other atoms will be ignored.
        loop {
            let header = match self.iter.next_header() {
                Ok(Some(header)) => header,
                Ok(None) => break,
                // If fragmented, an EOF is the only way to truly detect the end of stream.
                Err(AtomError::Other(Error::IoError(err)))
                    if self.moov.is_fragmented()
                        && err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                // Passthrough other errors.
                Err(err) => return Err(err.into()),
            };

            match header.atom_type() {
                AtomType::MediaData => {
                    return Ok(true);
                }
                AtomType::MovieFragment => {
                    let moof = self.iter.read_atom::<MoofAtom>()?;

                    // A moof segment can only be created if the media is fragmented.
                    if self.moov.is_fragmented() {
                        // Get the last segment.
                        let last_seg = self.segs.last().unwrap();

                        // Create a new segment for the moof atom.
                        let seg = MoofSegment::new(moof, self.moov.clone(), last_seg.as_ref())?;

                        // Segments should have a monotonic sequence number.
                        if seg.sequence_num() <= last_seg.sequence_num() {
                            warn!("moof fragment has a non-monotonic sequence number.");
                        }

                        // Push the segment.
                        self.segs.push(Box::new(seg));
                    } else {
                        return decode_error("isomp4: moof atom present without mvex atom");
                    }
                }
                AtomType::SegmentIndex => {
                    let sidx = self.iter.read_atom::<SidxAtom>()?;
                    record_sidx_seek_index(&mut self.sidx_seek_indexes, &sidx, self.source_length);
                }
                _ => {
                    trace!("skipping atom: {:?}.", header.atom_type());
                }
            }
        }

        // If no atoms were returned above, then the end-of-stream has been reached.
        Ok(false)
    }

    fn seek_track_by_time(&mut self, track_num: usize, time: Time) -> Result<SeekedTo> {
        // Convert time to timestamp for the track.
        if let Some(track) = self.tracks.get(track_num) {
            let tb = track.time_base.unwrap();
            let ts = tb
                .calc_timestamp(time)
                .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
            self.seek_track_by_ts(track_num, ts)
        } else {
            seek_error(SeekErrorKind::Unseekable)
        }
    }

    fn seek_track_by_ts(&mut self, track_num: usize, ts: Timestamp) -> Result<SeekedTo> {
        debug!("seeking track_num={track_num} to frame_ts={ts}");

        struct SeekLocation {
            seg_idx: usize,
            sample_num: u32,
        }

        // Can only seek to 0 or positive timestamps.
        if ts.is_negative() {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let mut seg_skip = 0;
        let mut best_seek_location = None;

        let seek_loc = 'locate: loop {
            // Iterate over all segments and attempt to find the segment and sample number that
            // contains the desired timestamp. Skip segments already examined.
            for (seg_idx, seg) in self.segs.iter().enumerate().skip(seg_skip) {
                let ts_range = seg.track_ts_range(track_num);
                if !ts_range.is_empty() && ts_range.start > ts.get() as u64 {
                    break 'locate best_seek_location
                        .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
                }

                if let Some(sample_num) = seg.ts_sample(track_num, ts.get() as u64)? {
                    best_seek_location = Some(SeekLocation {
                        seg_idx,
                        sample_num,
                    });
                } else if best_seek_location.is_none()
                    && seg_idx > 0
                    && self.indexed_seek_sap_track_id == Some(self.track_states[track_num].track_id)
                {
                    let mut sample_range = seg.track_sample_range(track_num);
                    if let Some(sample_num) = sample_range.next() {
                        // `sidx` доказывает SAP только для начала выбранного subsegment-а.
                        // Позднейшие segments всё равно обязаны доказать RAP обычными flags.
                        best_seek_location = Some(SeekLocation {
                            seg_idx,
                            sample_num,
                        });
                    }
                }

                // Mark the segment as examined.
                seg_skip = seg_idx + 1;
            }

            // Otherwise, try to read more segments from the stream.
            if !self.try_read_more_segments()? {
                break best_seek_location.ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
            }
        };

        let seg = &self.segs[seek_loc.seg_idx];

        // Get the sample timing.
        let timing = seg.sample_timing(track_num, seek_loc.sample_num)?.unwrap();

        // Try to convert the sample timing to a timestamp.
        let actual_ts = match Timestamp::try_from(timing.dts) {
            Ok(ts) => ts,
            _ => return seek_error(SeekErrorKind::OutOfRange),
        };

        // Get the sample information.
        let data_desc = seg.sample_data(track_num, seek_loc.sample_num, true)?;

        // Update the track's next sample information to point to the seeked sample.
        let track = &mut self.track_states[track_num];

        track.cur_seg = seek_loc.seg_idx;
        track.next_sample = seek_loc.sample_num;
        track.next_sample_pos = data_desc.base_pos + data_desc.offset.unwrap();
        if self.indexed_seek_sap_track_id == Some(track.track_id) {
            self.indexed_seek_sap_track_id = None;
        }

        debug!(
            "seeked track_num={} (track_id={}) to packet_ts={} (delta={})",
            track_num,
            track.track_id,
            actual_ts,
            actual_ts.saturating_delta(ts),
        );

        Ok(SeekedTo {
            track_id: track.track_id,
            required_ts: ts,
            actual_ts,
        })
    }
}

/// Публикует распознанные project-specific video tags как per-track metadata.
///
/// Symphonia 0.6 `Track`/`VideoCodecParameters` не имеют полей для display transform и HDR
/// container side metadata, поэтому локальный MP4 patch передаёт только нейтральные raw tags.
fn append_track_rustiplayer_metadata(metadata: &mut MetadataLog, traks: &[TrakAtom]) {
    let mut metadata_builder = MetadataBuilder::new(ISOMP4_METADATA_INFO);
    let mut has_rustiplayer_metadata = false;

    for trak in traks {
        let mut track_metadata_builder = PerTrackMetadataBuilder::new(u64::from(trak.tkhd.id));
        let mut has_track_metadata = false;

        has_track_metadata |=
            append_track_display_orientation_tags(&mut track_metadata_builder, trak);
        has_track_metadata |=
            append_track_h264_parameter_set_tags(&mut track_metadata_builder, trak);
        has_track_metadata |= append_track_video_color_tags(&mut track_metadata_builder, trak);

        if has_track_metadata {
            metadata_builder.add_track(track_metadata_builder.build());
            has_rustiplayer_metadata = true;
        }
    }

    if has_rustiplayer_metadata {
        metadata.push_front(metadata_builder.build());
    }
}

/// Публикует распознанный `tkhd` display matrix как нормализованный clockwise quarter-turn tag.
fn append_track_display_orientation_tags(
    track_metadata_builder: &mut PerTrackMetadataBuilder,
    trak: &TrakAtom,
) -> bool {
    let Some(clockwise_degrees) = trak.tkhd.display_matrix.quarter_turn_clockwise_degrees() else {
        return false;
    };

    if clockwise_degrees == 0 {
        return false;
    }

    track_metadata_builder.add_tag(Tag::new_from_parts(
        RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG,
        u64::from(clockwise_degrees),
        None,
    ));
    true
}

/// Публикует точную `avc3` семантику, которую generic Symphonia codec id не сохраняет.
fn append_track_h264_parameter_set_tags(
    track_metadata_builder: &mut PerTrackMetadataBuilder,
    trak: &TrakAtom,
) -> bool {
    let Some(visual_sample_entry) = trak.mdia.minf.stbl.stsd.visual_sample_entry() else {
        return false;
    };
    if !visual_sample_entry.parameter_sets_may_be_in_band {
        return false;
    }

    track_metadata_builder.add_tag(Tag::new_from_parts(
        RUSTIPLAYER_H264_PARAMETER_SETS_IN_BAND_TAG,
        true,
        None,
    ));
    true
}

/// Публикует `colr`/`mdcv`/`clli` как стабильные raw tags для neutral demux layer.
fn append_track_video_color_tags(
    track_metadata_builder: &mut PerTrackMetadataBuilder,
    trak: &TrakAtom,
) -> bool {
    let Some(visual_sample_entry) = trak.mdia.minf.stbl.stsd.visual_sample_entry() else {
        return false;
    };

    let mut has_color_metadata = false;

    if let Some(nclx) = visual_sample_entry
        .colour_information
        .and_then(|colour_information| colour_information.nclx)
    {
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG,
            nclx.full_range_flag,
            None,
        ));
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG,
            u64::from(nclx.matrix_coefficients),
            None,
        ));
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG,
            u64::from(nclx.color_primaries),
            None,
        ));
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG,
            u64::from(nclx.transfer_characteristics),
            None,
        ));
        has_color_metadata = true;
    }

    if let Some(mastering_display) = visual_sample_entry.mastering_display_colour_volume {
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG,
            f64::from(mastering_display.max_luminance_nits),
            None,
        ));
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG,
            f64::from(mastering_display.min_luminance_nits),
            None,
        ));
        has_color_metadata = true;
    }

    if let Some(content_light_level) = visual_sample_entry.content_light_level {
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG,
            u64::from(content_light_level.max_content_light_level_nits),
            None,
        ));
        track_metadata_builder.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG,
            u64::from(content_light_level.max_frame_average_light_level_nits),
            None,
        ));
        has_color_metadata = true;
    }

    has_color_metadata
}

impl Scoreable for IsoMp4Reader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for IsoMp4Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(IsoMp4Reader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            ISOMP4_FORMAT_INFO,
            &["mp4", "m4a", "m4p", "m4b", "m4r", "m4v", "mov"],
            &["video/mp4", "audio/m4a"],
            &[b"ftyp"] // Top-level atoms
        )]
    }
}

impl FormatReader for IsoMp4Reader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &ISOMP4_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        self.read_next_packet_with_source_offset()
            .map(|packet| packet.map(IsoMp4PacketWithSourceOffset::into_packet))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        match to {
            SeekTo::Timestamp { ts, track_id } => {
                // The seek timestamp is in timebase units specific to the selected track. Get the
                // selected track and use the timebase to convert the timestamp into time units so
                // that the other tracks can be seeked.
                if let Some((track_num, track)) = self
                    .tracks
                    .iter()
                    .enumerate()
                    .find(|(_, track)| track.id == track_id)
                {
                    // Convert to time units.
                    let time = track
                        .time_base
                        .unwrap()
                        .calc_time(ts)
                        .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

                    self.prepare_indexed_fragment_seek(track_id, time)?;

                    // Seek all tracks excluding the primary track to the desired time.
                    for t in 0..self.track_states.len() {
                        if t != track_num {
                            self.seek_track_by_time(t, time)?;
                        }
                    }

                    // Seek the primary track and return the result.
                    self.seek_track_by_ts(track_num, ts)
                } else {
                    seek_error(SeekErrorKind::InvalidTrack)
                }
            }
            SeekTo::Time { time, track_id } => {
                // If provided, find the track number of the track with the desired track_id, or
                // default to the first track.
                let track_num = match track_id {
                    Some(id) => self
                        .tracks
                        .iter()
                        .position(|track| track.id == id)
                        .ok_or(Error::SeekError(SeekErrorKind::InvalidTrack))?,
                    None => 0,
                };

                self.prepare_indexed_fragment_seek(self.tracks[track_num].id, time)?;

                // Seek all tracks excluding the selected track and discard the result.
                for t in 0..self.track_states.len() {
                    if t != track_num {
                        self.seek_track_by_time(t, time)?;
                    }
                }

                // Seek the primary track and return the result.
                self.seek_track_by_time(track_num, time)
            }
        }
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.iter.into_inner()
    }
}

impl ReadAtom for MediaSourceStream<'_> {}

impl From<AtomError> for Error {
    fn from(value: AtomError) -> Self {
        // Map all atom iteration errors to decode errors.
        let msg = match value {
            AtomError::InvalidAtomSize => "isomp4: invalid atom size",
            AtomError::InvalidUtf8 => "isomp4: invalid utf-8 string",
            AtomError::MaximumDepthReached => "isomp4: maximum recursion depth reached",
            AtomError::NoParentAtom => "isomp4: no parent atom",
            AtomError::NoPendingAtom => "isomp4: no atom pending read",
            AtomError::Overrun => "isomp4: overrun while reading atom",
            AtomError::SeekOutOfRange => "isomp4: out-of-bounds seek for a non-seekable stream",
            AtomError::UnexpectedEndOfAtom => "isomp4: unexpected end of atom",
            AtomError::UnexpectedPosition => "isomp4: unexpected position",
            AtomError::UnexpectedUnknownSizeAtom => "isomp4: unknown size atom has sized parent",
            AtomError::UnexpectedReadOperation => "isomp4: unexpected read operation",
            AtomError::UnknownAtomSize => "isomp4: unknown atom size",
            AtomError::Other(err) => return err,
        };
        Error::DecodeError(msg)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use symphonia_core::codecs::audio::AudioCodecParameters;
    use symphonia_core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_PCM_S16LE};

    use super::*;

    struct FakeSegment {
        sample_range: Range<u32>,
        base_pos: u64,
        sample_size: u32,
        chunk_sample_count: u32,
    }

    impl FakeSegment {
        fn new(sample_range: Range<u32>, base_pos: u64, sample_size: u32) -> Self {
            Self {
                sample_range,
                base_pos,
                sample_size,
                chunk_sample_count: u32::MAX,
            }
        }

        fn with_chunk_sample_count(mut self, chunk_sample_count: u32) -> Self {
            self.chunk_sample_count = chunk_sample_count;
            self
        }
    }

    impl StreamSegment for FakeSegment {
        fn sequence_num(&self) -> u32 {
            0
        }

        fn all_tracks_ended(&self) -> bool {
            true
        }

        fn track_sample_range(&self, _track_num: usize) -> Range<u32> {
            self.sample_range.clone()
        }

        fn track_ts_range(&self, _track_num: usize) -> Range<u64> {
            u64::from(self.sample_range.start)..u64::from(self.sample_range.end)
        }

        fn sample_timing(
            &self,
            _track_num: usize,
            sample_num: u32,
        ) -> Result<Option<SampleTiming>> {
            if !self.sample_range.contains(&sample_num) {
                return Ok(None);
            }

            Ok(Some(SampleTiming {
                pts: u64::from(sample_num),
                dts: u64::from(sample_num),
                dur: 1,
            }))
        }

        fn ts_sample(&self, _track_num: usize, ts: u64) -> Result<Option<u32>> {
            let sample_num =
                u32::try_from(ts).map_err(|_| Error::DecodeError("test timestamp overflow"))?;

            Ok(self
                .sample_range
                .contains(&sample_num)
                .then_some(sample_num))
        }

        fn sample_data(
            &self,
            _track_num: usize,
            sample_num: u32,
            get_offset: bool,
        ) -> Result<SampleDataDesc> {
            if !self.sample_range.contains(&sample_num) {
                return decode_error("test sample out of range");
            }

            let sample_offset = sample_num - self.sample_range.start;
            let chunk_index = sample_offset / self.chunk_sample_count;
            let sample_in_chunk = sample_offset % self.chunk_sample_count;
            let chunk_size = u64::from(self.chunk_sample_count) * u64::from(self.sample_size);
            let base_pos = self.base_pos + u64::from(chunk_index) * chunk_size;
            let offset =
                get_offset.then_some(u64::from(sample_in_chunk) * u64::from(self.sample_size));

            Ok(SampleDataDesc {
                base_pos,
                offset,
                size: self.sample_size,
            })
        }
    }

    fn audio_track(codec: AudioCodecId, max_frames_per_packet: Option<u64>) -> Track {
        let mut params = AudioCodecParameters::new();
        params.codec = codec;
        params.max_frames_per_packet = max_frames_per_packet;

        let mut track = Track::new(1);
        track.with_codec_params(CodecParameters::Audio(params));
        track
    }

    fn direct_sidx() -> SidxAtom {
        SidxAtom {
            reference_id: 7,
            timescale: NonZero::new(1_000).expect("test timescale is non-zero"),
            earliest_pts: 5_000,
            first_offset: 100,
            references: vec![
                crate::atoms::sidx::SidxReference {
                    reference_type: crate::atoms::sidx::ReferenceType::Media,
                    reference_size: 50,
                    subsegment_duration: 10_000,
                    starts_with_sap: true,
                    sap_type: 1,
                    sap_delta_time: 0,
                },
                crate::atoms::sidx::SidxReference {
                    reference_type: crate::atoms::sidx::ReferenceType::Media,
                    reference_size: 60,
                    subsegment_duration: 20_000,
                    starts_with_sap: false,
                    sap_type: 0,
                    sap_delta_time: 0,
                },
                crate::atoms::sidx::SidxReference {
                    reference_type: crate::atoms::sidx::ReferenceType::Media,
                    reference_size: 70,
                    subsegment_duration: 30_000,
                    starts_with_sap: true,
                    sap_type: 1,
                    sap_delta_time: 0,
                },
                crate::atoms::sidx::SidxReference {
                    reference_type: crate::atoms::sidx::ReferenceType::Media,
                    reference_size: 80,
                    subsegment_duration: 40_000,
                    starts_with_sap: false,
                    sap_type: 0,
                    sap_delta_time: 0,
                },
            ],
            total_duration: 100_000,
        }
    }

    #[test]
    fn direct_sidx_seek_uses_authored_subsegment_boundaries() {
        let index = SidxSeekIndex::from_atom(&direct_sidx()).expect("direct index is usable");

        assert_eq!(index.byte_offset_for_timestamp(0), Some(100));
        assert_eq!(index.byte_offset_for_timestamp(14_999), Some(100));
        assert_eq!(index.byte_offset_for_timestamp(15_000), Some(100));
        assert_eq!(index.byte_offset_for_timestamp(34_999), Some(100));
        assert_eq!(index.byte_offset_for_timestamp(35_000), Some(210));
        assert_eq!(index.byte_offset_for_timestamp(35_001), Some(210));
        assert_eq!(index.byte_offset_for_timestamp(65_000), Some(210));
        assert_eq!(index.byte_offset_for_timestamp(65_001), Some(210));
        assert_eq!(index.byte_offset_for_timestamp(105_000), Some(210));
        assert_eq!(index.byte_offset_for_timestamp(105_001), None);
    }

    #[test]
    fn indirect_or_malformed_sidx_is_not_used_for_byte_seek() {
        let mut indirect = direct_sidx();
        indirect.references[1].reference_type = crate::atoms::sidx::ReferenceType::Segment;
        assert!(SidxSeekIndex::from_atom(&indirect).is_none());

        let mut empty_reference = direct_sidx();
        empty_reference.references[0].reference_size = 0;
        assert!(SidxSeekIndex::from_atom(&empty_reference).is_none());

        let mut offset_overflow = direct_sidx();
        offset_overflow.first_offset = u64::MAX;
        assert!(SidxSeekIndex::from_atom(&offset_overflow).is_none());

        let mut unproven_first_sap = direct_sidx();
        unproven_first_sap.references[0].sap_type = 3;
        let index = SidxSeekIndex::from_atom(&unproven_first_sap).expect("index shape is valid");
        assert_eq!(index.byte_offset_for_timestamp(0), None);
    }

    #[test]
    fn ordered_sidx_atoms_extend_the_same_track_index() {
        let mut index = SidxSeekIndex::from_atom(&direct_sidx()).expect("first index is valid");
        let mut continuation = direct_sidx();
        continuation.earliest_pts = 105_000;
        continuation.first_offset = 360;
        let continuation =
            SidxSeekIndex::from_atom(&continuation).expect("continuation index is valid");

        index.append_if_ordered(continuation);

        assert_eq!(index.byte_offset_for_timestamp(106_000), Some(360));
        assert!(index.fits_source_length(620));
        assert!(!index.fits_source_length(619));
    }

    #[test]
    fn pcm_packet_sample_limit_uses_reader_chunk_for_single_frame_pcm() {
        let pcm_track = audio_track(CODEC_ID_PCM_S16LE, Some(1024));
        let single_frame_pcm_track = audio_track(CODEC_ID_PCM_S16LE, Some(1));
        let unknown_packet_pcm_track = audio_track(CODEC_ID_PCM_S16LE, None);
        let aac_track = audio_track(CODEC_ID_AAC, Some(1024));

        assert_eq!(
            pcm_packet_sample_limit(&pcm_track, Duration::from(1_u32)),
            Some(1024)
        );
        assert_eq!(
            pcm_packet_sample_limit(&pcm_track, Duration::from(1024_u32)),
            None
        );
        assert_eq!(
            pcm_packet_sample_limit(&single_frame_pcm_track, Duration::from(1_u32)),
            Some(PCM_FRAMES_PER_READER_PACKET)
        );
        assert_eq!(
            pcm_packet_sample_limit(&unknown_packet_pcm_track, Duration::from(1_u32)),
            Some(PCM_FRAMES_PER_READER_PACKET)
        );
        assert_eq!(
            pcm_packet_sample_limit(&single_frame_pcm_track, Duration::from(1024_u32)),
            None
        );
        assert_eq!(
            pcm_packet_sample_limit(&aac_track, Duration::from(1_u32)),
            None
        );
    }

    #[test]
    fn pcm_packet_span_coalesces_contiguous_frame_samples() {
        let segment = FakeSegment::new(0..1024, 1_000, 4);

        let span = collect_pcm_packet_span(&segment, 0, 0, 1_000, 1024)
            .expect("contiguous PCM frame-samples должны склеиваться");

        assert_eq!(
            span,
            PacketSampleSpan {
                pos: 1_000,
                len: 4_096,
                dur: Duration::from(1024_u32),
                sample_count: 1024,
            }
        );
    }

    #[test]
    fn pcm_packet_span_keeps_tail_packet_duration() {
        let segment = FakeSegment::new(2048..2784, 9_000, 4);

        let span = collect_pcm_packet_span(&segment, 0, 2048, 9_000, 1024)
            .expect("tail PCM packet должен сохранить фактическую длину");

        assert_eq!(
            span,
            PacketSampleSpan {
                pos: 9_000,
                len: 2_944,
                dur: Duration::from(736_u32),
                sample_count: 736,
            }
        );
    }

    #[test]
    fn pcm_packet_span_stops_before_chunk_boundary() {
        let segment = FakeSegment::new(0..1024, 20_000, 4).with_chunk_sample_count(512);

        let span = collect_pcm_packet_span(&segment, 0, 0, 20_000, 1024)
            .expect("PCM packet должен завершиться на границе chunk");

        assert_eq!(
            span,
            PacketSampleSpan {
                pos: 20_000,
                len: 2_048,
                dur: Duration::from(512_u32),
                sample_count: 512,
            }
        );
    }
}

// fn convert_timescale(
//     duration: u64,
//     src_timescale: NonZero<u32>,
//     dst_timescale: NonZero<u32>,
// ) -> Duration {
//     if src_timescale == dst_timescale {
//         return Duration::from(duration);
//     }
//     Duration::from(
//         ((duration as u128 * dst_timescale.get() as u128) / src_timescale.get() as u128) as u64,
//     )
// }
