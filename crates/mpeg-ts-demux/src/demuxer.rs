use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use codec_core::{H264Packetization, H265Packetization, h264_nal_units, h265_nal_units};
use demux_api::DemuxInput;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaTime, Packet, TimeBase, TimelineNotSeekableReason,
    TrackDuration, TrackId, TrackInfo, TrackKind, TrackTimestamp, VideoPacketFraming,
    VideoTrackMetadata,
};
use source_core::CancellationToken;

use crate::elementary::{ElementaryPacket, drain_adts_frames, drain_mpeg_audio_frames};
use crate::framing::{TransportPacket, TransportPacketReader, TransportReadOutcome};
use crate::pes::{PesAssembler, PesPacket};
use crate::psi::{
    ElementaryStream, ProgramMap, ProgramMapEntry, PsiSectionAssembler, StreamKind, parse_pat,
    parse_pmt, select_program,
};
use crate::timestamps::TimestampUnwrapper;
use crate::video_assembler::{TimedVideoAccessUnit, VideoAccessUnitAssembler};
use crate::{MpegTsDemuxError, MpegTsDemuxOptions};

#[path = "index.rs"]
mod index;
use index::{IndexContinuationState, SeekAnchor};

/// Возвращает общую MPEG clock time base для PTS/DTS/PCR base.
fn mpeg_clock() -> TimeBase {
    TimeBase::new(1, 90_000).expect("MPEG clock denominator is non-zero")
}

/// Независимые unwrap states одного elementary PID.
#[derive(Debug, Default, Clone)]
struct StreamTimestamps {
    pts: TimestampUnwrapper,
    dts: TimestampUnwrapper,
}

/// Elementary audio bytes могут пересекать PES boundaries.
#[derive(Debug, Default, Clone)]
struct AudioAccumulator {
    bytes: Vec<u8>,
    next_pts: Option<i64>,
    dts: Option<i64>,
    byte_offset: u64,
}

/// Decoder-critical audio evidence, доказанный elementary frame header-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioTrackEvidence {
    codec_id: &'static str,
    sample_rate: Option<u32>,
    channels: Option<u32>,
}

/// First-party MPEG-TS runtime owner.
pub struct MpegTsDemuxer {
    reader: TransportPacketReader,
    options: MpegTsDemuxOptions,
    pat_assembler: PsiSectionAssembler,
    pmt_assemblers: HashMap<u16, PsiSectionAssembler>,
    pat_version: Option<u8>,
    pat_programs: Vec<ProgramMapEntry>,
    program_maps: BTreeMap<u16, ProgramMap>,
    selected_program: Option<ProgramMap>,
    stream_by_pid: HashMap<u16, StreamKind>,
    continuity_by_pid: HashMap<u16, u8>,
    pes_by_pid: HashMap<u16, PesAssembler>,
    timestamps_by_pid: HashMap<u16, StreamTimestamps>,
    pcr_timestamp: TimestampUnwrapper,
    audio_by_pid: HashMap<u16, AudioAccumulator>,
    video_assembler: VideoAccessUnitAssembler,
    audio_evidence_by_pid: HashMap<u16, AudioTrackEvidence>,
    config_evidence_by_pid: HashMap<u16, Vec<u8>>,
    tracks: Vec<TrackInfo>,
    pending_events: VecDeque<DemuxReadEvent>,
    seek_index: Vec<SeekAnchor>,
    index_scan_offset: u64,
    index_reached_end: bool,
    index_continuation: Option<IndexContinuationState>,
    duration: Option<Duration>,
    reached_end: bool,
}

impl MpegTsDemuxer {
    #[cfg(test)]
    pub(crate) fn test_index_entries(&self) -> usize {
        self.seek_index.len()
    }

    #[cfg(test)]
    pub(crate) const fn test_reader_position(&self) -> u64 {
        self.reader.position()
    }

    /// Открывает neutral input и выполняет только bounded PAT/PMT discovery.
    pub fn open(
        input: DemuxInput,
        cancellation: CancellationToken,
        options: MpegTsDemuxOptions,
    ) -> Result<Self, MpegTsDemuxError> {
        let mut demuxer = Self {
            reader: TransportPacketReader::new(input, cancellation, options),
            options,
            pat_assembler: PsiSectionAssembler::default(),
            pmt_assemblers: HashMap::new(),
            pat_version: None,
            pat_programs: Vec::new(),
            program_maps: BTreeMap::new(),
            selected_program: None,
            stream_by_pid: HashMap::new(),
            continuity_by_pid: HashMap::new(),
            pes_by_pid: HashMap::new(),
            timestamps_by_pid: HashMap::new(),
            pcr_timestamp: TimestampUnwrapper::default(),
            audio_by_pid: HashMap::new(),
            video_assembler: VideoAccessUnitAssembler::new(options.video_access_unit_bytes.get()),
            audio_evidence_by_pid: HashMap::new(),
            config_evidence_by_pid: HashMap::new(),
            tracks: Vec::new(),
            pending_events: VecDeque::new(),
            seek_index: Vec::new(),
            index_scan_offset: 0,
            index_reached_end: false,
            index_continuation: None,
            duration: None,
            reached_end: false,
        };
        let mut probed_packets = 0_usize;
        while probed_packets < options.initial_probe_packets.get() {
            match demuxer.reader.next_packet()? {
                TransportReadOutcome::Packet(packet) => {
                    probed_packets = probed_packets.saturating_add(1);
                    demuxer.process_transport_packet(packet, false)?;
                    if demuxer.initial_topology_ready() {
                        demuxer.build_bounded_initial_index()?;
                        return Ok(demuxer);
                    }
                }
                TransportReadOutcome::EndResource(_metadata) => {
                    demuxer.finish_streamed_resource(false)?;
                    if demuxer.initial_topology_ready() {
                        demuxer.build_bounded_initial_index()?;
                        return Ok(demuxer);
                    }
                }
                TransportReadOutcome::EndOfInput => {
                    demuxer.finish_pending_elementary_streams(false)?;
                    break;
                }
            }
        }
        if !demuxer.reader.requires_explicit_resource_end() {
            // Legacy byte/OrderedSegments paths сохраняют прежний bounded-probe fallback.
            demuxer.finish_pending_elementary_streams(false)?;
        }
        if demuxer.initial_topology_ready() {
            demuxer.build_bounded_initial_index()?;
            return Ok(demuxer);
        }
        Err(MpegTsDemuxError::NoPlayableProgram)
    }

    fn initial_topology_ready(&self) -> bool {
        let Some(program) = &self.selected_program else {
            return false;
        };
        program.streams.iter().all(|stream| {
            !matches!(stream.kind, StreamKind::AacAdts | StreamKind::MpegAudio)
                || self.audio_evidence_by_pid.contains_key(&stream.pid)
        }) && !self.tracks.is_empty()
    }

    fn process_transport_packet(
        &mut self,
        packet: TransportPacket,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        if packet.starts_new_segment {
            // Ordered resource boundary завершает предыдущий самостоятельный TS segment.
            // Pending PES/AU нужно опубликовать до transport reset: иначе первый video RAP
            // каждого segment-а молча терялся при continuity-counter restart-е.
            self.finish_pending_elementary_streams(publish_lifecycle)?;
            self.reset_ordered_segment_state();
        }
        let relevant_pid = packet.pid == 0
            || self
                .pat_programs
                .iter()
                .any(|program| program.pmt_pid == packet.pid)
            || self
                .selected_program
                .as_ref()
                .is_some_and(|program| program.pcr_pid == packet.pid)
            || self.stream_by_pid.contains_key(&packet.pid);
        if !relevant_pid {
            return Ok(());
        }
        if packet.scrambling != 0 {
            return Err(MpegTsDemuxError::Scrambled { pid: packet.pid });
        }
        if packet.transport_error {
            self.reset_pid(packet.pid);
            return Ok(());
        }
        if packet.discontinuity {
            self.reset_pid(packet.pid);
            // MPEG-TS `discontinuity_indicator` сообщает о разрыве transport continuity и
            // timestamps, но сам по себе не меняет состав или конфигурацию треков. HLS
            // сегменты часто ставят этот флаг на первый audio/video packet, поэтому
            // публикация `TracksChanged` здесь пересоздавала pipeline на каждом сегменте.
            // Lifecycle-событие принадлежит явной границе новой timeline от контейнера.
            if packet.starts_new_timeline && self.stream_by_pid.contains_key(&packet.pid) {
                self.publish_tracks_changed();
            }
        }
        if !packet.payload.is_empty()
            && !packet.discontinuity
            && let Some(previous) = self.continuity_by_pid.get(&packet.pid).copied()
        {
            if packet.continuity_counter == previous {
                return Ok(());
            }
            if packet.continuity_counter != (previous + 1) & 0x0f {
                self.reset_pid(packet.pid);
                if publish_lifecycle && self.stream_by_pid.contains_key(&packet.pid) {
                    self.publish_tracks_changed();
                }
            }
        }
        if !packet.payload.is_empty() {
            self.continuity_by_pid
                .insert(packet.pid, packet.continuity_counter);
        }
        if packet.pid == 0 {
            return self.process_pat(packet, publish_lifecycle);
        }
        if self
            .pat_programs
            .iter()
            .any(|program| program.pmt_pid == packet.pid)
        {
            return self.process_pmt(packet, publish_lifecycle);
        }
        if let Some(raw_pcr) = packet.pcr_base {
            self.observe_pcr(raw_pcr, packet.byte_offset);
        }
        if self.stream_by_pid.contains_key(&packet.pid) {
            let assembler = self
                .pes_by_pid
                .entry(packet.pid)
                .or_insert_with(|| PesAssembler::new(packet.pid, self.options.pes_bytes.get()));
            if let Some(pes) = assembler.push(
                packet.payload_unit_start,
                &packet.payload,
                packet.byte_offset,
            )? {
                self.process_pes(pes, publish_lifecycle)?;
            }
        }
        Ok(())
    }

    fn process_pat(
        &mut self,
        packet: TransportPacket,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        for section in self
            .pat_assembler
            .push(packet.payload_unit_start, &packet.payload)?
        {
            let (version, programs) = parse_pat(&section)?;
            if self.pat_version == Some(version) && self.pat_programs == programs {
                continue;
            }
            let topology_changed = self.pat_version.is_some();
            self.pat_version = Some(version);
            self.pat_programs = programs;
            self.program_maps.clear();
            self.pmt_assemblers.clear();
            for program in &self.pat_programs {
                self.pmt_assemblers.entry(program.pmt_pid).or_default();
            }
            if topology_changed && publish_lifecycle {
                self.reset_elementary_state();
                self.publish_tracks_changed();
            }
        }
        Ok(())
    }

    fn process_pmt(
        &mut self,
        packet: TransportPacket,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let assembler = self.pmt_assemblers.entry(packet.pid).or_default();
        let sections = assembler.push(packet.payload_unit_start, &packet.payload)?;
        for section in sections {
            let program = parse_pmt(&section)?;
            self.program_maps
                .insert(program.program_number, program.clone());
            if self.program_maps.len() < self.pat_programs.len() {
                continue;
            }
            let selected = select_program(&self.program_maps)?;
            if selected != self.selected_program {
                let had_program = self.selected_program.is_some();
                self.apply_program(selected);
                if had_program && publish_lifecycle {
                    self.publish_tracks_changed();
                }
            }
        }
        Ok(())
    }

    fn apply_program(&mut self, selected: Option<ProgramMap>) {
        self.reset_elementary_state();
        // Audio evidence принадлежит конкретной PMT topology: новый PID или stream type
        // обязан заново доказать codec/sample-rate/channels своим elementary header-ом.
        self.audio_evidence_by_pid.clear();
        self.selected_program = selected;
        self.stream_by_pid.clear();
        if let Some(program) = &self.selected_program {
            for stream in &program.streams {
                self.stream_by_pid.insert(stream.pid, stream.kind);
            }
        }
        self.rebuild_tracks();
    }

    fn reset_elementary_state(&mut self) {
        self.pes_by_pid.clear();
        self.timestamps_by_pid.clear();
        self.pcr_timestamp.reset();
        self.audio_by_pid.clear();
        self.video_assembler.reset_all();
        self.continuity_by_pid.clear();
        // Уже опубликованное track evidence переживает seek/index rewind той же topology.
        // Иначе короткий seekable TS после initial index терял AAC track до первого нового frame-а.
        self.config_evidence_by_pid.clear();
    }

    /// Новый ordered segment может начать continuity counters заново без смены media timeline.
    fn reset_ordered_segment_state(&mut self) {
        self.continuity_by_pid.clear();
        self.pat_assembler.reset();
        for assembler in self.pmt_assemblers.values_mut() {
            assembler.reset();
        }
        self.pes_by_pid.clear();
        self.audio_by_pid.clear();
        self.video_assembler.reset_all();
    }

    /// Настоящий streamed resource EOF завершает PES/AU и затем сбрасывает boundary state.
    pub(super) fn finish_streamed_resource(
        &mut self,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        self.finish_pending_elementary_streams(publish_lifecycle)?;
        self.reset_ordered_segment_state();
        Ok(())
    }

    fn reset_pid(&mut self, pid: u16) {
        self.continuity_by_pid.remove(&pid);
        if pid == 0 {
            self.pat_assembler.reset();
        }
        if let Some(assembler) = self.pmt_assemblers.get_mut(&pid) {
            assembler.reset();
        }
        if let Some(assembler) = self.pes_by_pid.get_mut(&pid) {
            assembler.reset();
        }
        if let Some(timestamps) = self.timestamps_by_pid.get_mut(&pid) {
            timestamps.pts.reset();
            timestamps.dts.reset();
        }
        if self
            .selected_program
            .as_ref()
            .is_some_and(|program| program.pcr_pid == pid)
        {
            self.pcr_timestamp.reset();
        }
        self.audio_by_pid.remove(&pid);
        self.video_assembler.reset_pid(pid);
    }

    fn process_pes(
        &mut self,
        pes: PesPacket,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let Some(kind) = self.stream_by_pid.get(&pes.pid).copied() else {
            return Ok(());
        };
        let timestamps = self.timestamps_by_pid.entry(pes.pid).or_default();
        let raw_pts =
            timestamps
                .pts
                .unwrap(pes.pts.ok_or_else(|| MpegTsDemuxError::Malformed {
                    reason: format!("PES на PID {} не содержит PTS", pes.pid),
                })?);
        let raw_dts = pes.dts.map(|timestamp| timestamps.dts.unwrap(timestamp));
        if matches!(kind, StreamKind::H264 | StreamKind::H265) {
            let access_units = self.video_assembler.push(
                pes.pid,
                &pes.payload,
                raw_pts,
                raw_dts,
                pes.byte_offset,
                kind == StreamKind::H265,
            )?;
            for access_unit in access_units {
                self.emit_video_access_unit(pes.pid, kind, access_unit, publish_lifecycle)?;
            }
            return Ok(());
        }

        let accumulator = self.audio_by_pid.entry(pes.pid).or_default();
        if accumulator.bytes.is_empty() {
            accumulator.next_pts = Some(raw_pts);
            accumulator.dts = raw_dts;
            accumulator.byte_offset = pes.byte_offset;
        }
        accumulator.bytes.extend_from_slice(&pes.payload);
        let frames = if kind == StreamKind::AacAdts {
            drain_adts_frames(&mut accumulator.bytes)?
        } else {
            drain_mpeg_audio_frames(&mut accumulator.bytes)?
        };
        let mut frame_pts = accumulator.next_pts.unwrap_or(raw_pts);
        let frame_dts = accumulator.dts;
        let frame_byte_offset = accumulator.byte_offset;
        for frame in frames {
            let track_changed = self.update_audio_evidence(pes.pid, &frame);
            if track_changed && publish_lifecycle {
                self.publish_tracks_changed();
            }
            let packet = self.make_packet(
                pes.pid,
                kind,
                frame_pts,
                frame_dts,
                frame_byte_offset,
                frame,
            );
            if packet.keyframe.is_known_keyframe() {
                self.add_seek_anchor(frame_pts, frame_byte_offset);
            }
            let packet_end = packet
                .duration
                .map_or(packet.pts, |duration| packet.pts.saturating_add(duration));
            self.duration = Some(self.duration.unwrap_or_default().max(packet_end));
            frame_pts = frame_pts.saturating_add(
                packet
                    .track_duration
                    .map_or(0, |duration| duration.units.get() as i64),
            );
            self.pending_events
                .push_back(DemuxReadEvent::Packet(packet));
        }
        if matches!(kind, StreamKind::AacAdts | StreamKind::MpegAudio)
            && let Some(accumulator) = self.audio_by_pid.get_mut(&pes.pid)
        {
            if accumulator.bytes.is_empty() {
                accumulator.next_pts = None;
                accumulator.dts = None;
            } else {
                accumulator.next_pts = Some(frame_pts);
            }
        }
        Ok(())
    }

    fn emit_video_access_unit(
        &mut self,
        pid: u16,
        kind: StreamKind,
        access_unit: TimedVideoAccessUnit,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        if let Some(evidence) = video_config_evidence(kind, &access_unit.packet.bytes)? {
            let changed = self
                .config_evidence_by_pid
                .insert(pid, evidence.clone())
                .is_some_and(|previous| previous != evidence);
            if changed && publish_lifecycle {
                self.publish_tracks_changed();
            }
        }
        let packet = self.make_packet(
            pid,
            kind,
            access_unit.pts,
            access_unit.dts,
            access_unit.byte_offset,
            access_unit.packet,
        );
        if packet.keyframe.is_known_keyframe() {
            self.add_seek_anchor(access_unit.pts, access_unit.byte_offset);
        }
        self.duration = Some(self.duration.unwrap_or_default().max(packet.pts));
        self.pending_events
            .push_back(DemuxReadEvent::Packet(packet));
        Ok(())
    }

    fn update_audio_evidence(&mut self, pid: u16, frame: &ElementaryPacket) -> bool {
        let Some(codec_id) = frame.audio_codec_id else {
            return false;
        };
        let evidence = AudioTrackEvidence {
            codec_id,
            sample_rate: frame.sample_rate,
            channels: frame.channels,
        };
        let changed = self.audio_evidence_by_pid.insert(pid, evidence) != Some(evidence);
        if changed {
            self.rebuild_tracks();
        }
        changed
    }

    fn make_packet(
        &self,
        pid: u16,
        kind: StreamKind,
        pts: i64,
        dts: Option<i64>,
        byte_offset: u64,
        frame: ElementaryPacket,
    ) -> Packet {
        let track_id = TrackId::new(u32::from(pid));
        let track_pts = TrackTimestamp::new(track_id, pts, mpeg_clock());
        let track_dts = dts.map(|value| TrackTimestamp::new(track_id, value, mpeg_clock()));
        let media_pts = track_pts.to_media_time().as_duration();
        let media_dts = track_dts.map(|timestamp| timestamp.to_media_time().as_duration());
        let track_kind = if matches!(kind, StreamKind::H264 | StreamKind::H265) {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let mut packet = Packet::new_with_keyframe_unbounded(
            track_id,
            track_kind,
            media_pts,
            media_dts,
            frame.keyframe,
            frame.bytes,
        )
        .with_track_timestamps(Some(track_pts), track_dts)
        .with_decode_start_initialization(frame.decode_start_initialization)
        .with_byte_offset(byte_offset);
        if let Some(duration_units) = frame.duration_90khz {
            let track_duration = TrackDuration::new(track_id, duration_units, mpeg_clock());
            packet = packet.with_track_duration(track_duration);
        }
        packet
    }

    fn rebuild_tracks(&mut self) {
        let Some(program) = &self.selected_program else {
            self.tracks.clear();
            return;
        };
        self.tracks = program
            .streams
            .iter()
            .filter_map(|stream| self.track_from_stream(*stream))
            .collect();
    }

    fn track_from_stream(&self, stream: ElementaryStream) -> Option<TrackInfo> {
        let audio_evidence = self.audio_evidence_by_pid.get(&stream.pid);
        let (kind, codec_id) = match stream.kind {
            StreamKind::H264 => (TrackKind::Video, "V_MPEG4/ISO/AVC"),
            StreamKind::H265 => (TrackKind::Video, "V_MPEGH/ISO/HEVC"),
            StreamKind::AacAdts | StreamKind::MpegAudio => {
                (TrackKind::Audio, audio_evidence?.codec_id)
            }
        };
        Some(TrackInfo {
            id: TrackId::new(u32::from(stream.pid)),
            kind,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: Some(mpeg_clock()),
            duration: self.duration,
            sample_rate: audio_evidence.and_then(|evidence| evidence.sample_rate),
            channels: audio_evidence.and_then(|evidence| evidence.channels),
            video: matches!(stream.kind, StreamKind::H264 | StreamKind::H265).then(|| {
                let mut metadata = VideoTrackMetadata::empty();
                metadata.packet_framing = VideoPacketFraming::AnnexB;
                metadata
            }),
        })
    }

    fn publish_tracks_changed(&mut self) {
        self.rebuild_tracks();
        self.pending_events
            .push_back(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
                self.tracks.clone(),
                self.duration,
            )));
    }

    /// Завершает pending PES/AU на доказанной границе ordered resource или всего input-а.
    fn finish_pending_elementary_streams(
        &mut self,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let pids: Vec<u16> = self.pes_by_pid.keys().copied().collect();
        for pid in pids {
            let completed = self
                .pes_by_pid
                .get_mut(&pid)
                .expect("PID collected from map")
                .finish()?;
            if let Some(pes) = completed {
                self.process_pes(pes, publish_lifecycle)?;
            }
        }
        for pid in self.video_assembler.active_pids() {
            let Some(access_unit) = self.video_assembler.finish_pid(pid)? else {
                continue;
            };
            let Some(kind) = self.stream_by_pid.get(&pid).copied() else {
                continue;
            };
            self.emit_video_access_unit(pid, kind, access_unit, publish_lifecycle)?;
        }
        if let Some((&pid, _)) = self
            .audio_by_pid
            .iter()
            .find(|(_, accumulator)| !accumulator.bytes.is_empty())
        {
            return Err(MpegTsDemuxError::Malformed {
                reason: format!("оборван elementary audio frame на PID {pid}"),
            });
        }
        Ok(())
    }
}

impl Demuxer for MpegTsDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seekability(&self) -> DemuxSeekability {
        if self.reader.is_seekable() {
            DemuxSeekability::Seekable
        } else {
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            }
        }
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(event);
            }
            if self.reached_end {
                return Ok(DemuxReadEvent::EndOfStream);
            }
            match self.reader.next_packet()? {
                TransportReadOutcome::Packet(packet) => {
                    self.process_transport_packet(packet, true)?;
                }
                TransportReadOutcome::EndResource(_metadata) => {
                    self.finish_streamed_resource(true)?;
                }
                TransportReadOutcome::EndOfInput => {
                    self.finish_pending_elementary_streams(true)?;
                    self.reached_end = true;
                }
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        if !self.reader.is_seekable() {
            return Err(MpegTsDemuxError::NotSeekable.into());
        }
        let target_units = mpeg_clock()
            .duration_to_track_units_saturating(request.timestamp.into())
            .get();
        self.expand_index_towards(target_units, request.mode)?;
        if !self.index_reached_end && !self.index_covers(target_units, request.mode) {
            return Err(MpegTsDemuxError::SeekAnchorUnavailable {
                target: request.timestamp,
            }
            .into());
        }
        let anchor = self
            .seek_index
            .iter()
            .rev()
            .filter(|anchor| request.mode != DemuxSeekMode::DecodePointBefore || anchor.decode_safe)
            .find(|anchor| anchor.timestamp_90khz <= target_units)
            .copied()
            .or_else(|| {
                (request.mode == DemuxSeekMode::Accurate).then_some(SeekAnchor {
                    timestamp_90khz: 0,
                    byte_offset: 0,
                    decode_safe: false,
                })
            })
            .ok_or(MpegTsDemuxError::SeekAnchorUnavailable {
                target: request.timestamp,
            })?;
        self.reader.seek_absolute(anchor.byte_offset)?;
        self.reset_elementary_state();
        self.pending_events.clear();
        self.reached_end = false;
        let actual_timestamp =
            TrackTimestamp::new(TrackId::new(0), anchor.timestamp_90khz, mpeg_clock());
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: actual_timestamp.to_media_time(),
            actual_track_timestamp: Some(actual_timestamp),
        })
    }
}

fn video_config_evidence(
    kind: StreamKind,
    payload: &[u8],
) -> Result<Option<Vec<u8>>, MpegTsDemuxError> {
    let mut evidence = Vec::new();
    match kind {
        StreamKind::H264 => {
            let nal_units =
                h264_nal_units(payload, H264Packetization::AnnexB).map_err(|error| {
                    MpegTsDemuxError::Malformed {
                        reason: format!("H.264 Annex-B config scan: {error}"),
                    }
                })?;
            for nal in nal_units {
                if matches!(nal.nal_unit_type(), 7 | 8) {
                    evidence.extend_from_slice(nal.bytes());
                }
            }
        }
        StreamKind::H265 => {
            let nal_units =
                h265_nal_units(payload, H265Packetization::AnnexB).map_err(|error| {
                    MpegTsDemuxError::Malformed {
                        reason: format!("H.265 Annex-B config scan: {error}"),
                    }
                })?;
            for nal in nal_units {
                if matches!(nal.nal_unit_type(), 32..=34) {
                    evidence.extend_from_slice(nal.bytes());
                }
            }
        }
        StreamKind::AacAdts | StreamKind::MpegAudio => return Ok(None),
    }
    Ok((!evidence.is_empty()).then_some(evidence))
}
