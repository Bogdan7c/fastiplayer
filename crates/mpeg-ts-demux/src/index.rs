use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use media_core::{DemuxReadEvent, DemuxSeekMode, TrackInfo};

use super::{AudioAccumulator, AudioTrackEvidence, MpegTsDemuxer, StreamTimestamps};
use crate::MpegTsDemuxError;
use crate::pes::PesAssembler;
use crate::psi::{ProgramMap, ProgramMapEntry, PsiSectionAssembler, StreamKind};
use crate::timestamps::TimestampUnwrapper;
use crate::video_assembler::VideoAccessUnitAssembler;

/// Sparse PCR/keyframe anchor с явным decode-safety evidence.
#[derive(Debug, Clone, Copy)]
pub(super) struct SeekAnchor {
    pub(super) timestamp_90khz: i64,
    pub(super) byte_offset: u64,
    pub(super) decode_safe: bool,
}

/// Полный parser snapshot для transactional on-demand index scan.
struct DemuxStateSnapshot {
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
    duration: Option<Duration>,
    reached_end: bool,
}

/// Committed parser continuation между bounded index windows.
#[derive(Clone)]
pub(super) struct IndexContinuationState {
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
}

impl MpegTsDemuxer {
    pub(super) fn observe_pcr(&mut self, raw_pcr: u64, byte_offset: u64) {
        let timestamp = self.pcr_timestamp.unwrap(raw_pcr);
        if self
            .seek_index
            .iter()
            .filter(|anchor| !anchor.decode_safe)
            .max_by_key(|anchor| anchor.byte_offset)
            .is_none_or(|anchor| byte_offset.saturating_sub(anchor.byte_offset) >= 188 * 64)
        {
            self.add_index_anchor(timestamp, byte_offset, false);
        }
    }

    pub(super) fn add_seek_anchor(&mut self, timestamp_90khz: i64, byte_offset: u64) {
        self.add_index_anchor(timestamp_90khz, byte_offset, true);
    }

    fn add_index_anchor(&mut self, timestamp_90khz: i64, byte_offset: u64, decode_safe: bool) {
        if self.seek_index.len() >= self.options.index_entries.get() {
            return;
        }
        self.seek_index.push(SeekAnchor {
            timestamp_90khz,
            byte_offset,
            decode_safe,
        });
        self.seek_index.sort_by_key(|anchor| anchor.timestamp_90khz);
        self.seek_index.dedup_by_key(|anchor| anchor.byte_offset);
    }

    pub(super) fn build_bounded_initial_index(&mut self) -> Result<(), MpegTsDemuxError> {
        if !self.reader.is_seekable() {
            return Ok(());
        }
        let resume_offset = self.reader.position();
        let mut reached_end = false;
        for _ in 0..self.options.seek_scan_packets.get() {
            match self.reader.next_packet()? {
                Some(packet) => self.process_transport_packet(packet, false)?,
                None => {
                    self.finish_pending_elementary_streams(false)?;
                    reached_end = true;
                    break;
                }
            }
        }
        if !reached_end {
            self.duration = None;
        }
        self.index_scan_offset = self.reader.position();
        self.index_reached_end = reached_end;
        self.index_continuation = (!reached_end).then(|| self.capture_index_continuation());
        self.reader.restore_after_index_scan(resume_offset)?;
        self.reset_elementary_state();
        self.pending_events.clear();
        self.reached_end = false;
        self.rebuild_tracks();
        Ok(())
    }

    fn snapshot_parser_state(&mut self) -> DemuxStateSnapshot {
        DemuxStateSnapshot {
            pat_assembler: self.pat_assembler.clone(),
            pmt_assemblers: self.pmt_assemblers.clone(),
            pat_version: self.pat_version,
            pat_programs: self.pat_programs.clone(),
            program_maps: self.program_maps.clone(),
            selected_program: self.selected_program.clone(),
            stream_by_pid: self.stream_by_pid.clone(),
            continuity_by_pid: self.continuity_by_pid.clone(),
            pes_by_pid: self.pes_by_pid.clone(),
            timestamps_by_pid: self.timestamps_by_pid.clone(),
            pcr_timestamp: self.pcr_timestamp.clone(),
            audio_by_pid: self.audio_by_pid.clone(),
            video_assembler: self.video_assembler.clone(),
            audio_evidence_by_pid: self.audio_evidence_by_pid.clone(),
            config_evidence_by_pid: self.config_evidence_by_pid.clone(),
            tracks: self.tracks.clone(),
            pending_events: std::mem::take(&mut self.pending_events),
            seek_index: self.seek_index.clone(),
            duration: self.duration,
            reached_end: self.reached_end,
        }
    }

    fn restore_parser_state(&mut self, snapshot: DemuxStateSnapshot) {
        self.pat_assembler = snapshot.pat_assembler;
        self.pmt_assemblers = snapshot.pmt_assemblers;
        self.pat_version = snapshot.pat_version;
        self.pat_programs = snapshot.pat_programs;
        self.program_maps = snapshot.program_maps;
        self.selected_program = snapshot.selected_program;
        self.stream_by_pid = snapshot.stream_by_pid;
        self.continuity_by_pid = snapshot.continuity_by_pid;
        self.pes_by_pid = snapshot.pes_by_pid;
        self.timestamps_by_pid = snapshot.timestamps_by_pid;
        self.pcr_timestamp = snapshot.pcr_timestamp;
        self.audio_by_pid = snapshot.audio_by_pid;
        self.video_assembler = snapshot.video_assembler;
        self.audio_evidence_by_pid = snapshot.audio_evidence_by_pid;
        self.config_evidence_by_pid = snapshot.config_evidence_by_pid;
        self.tracks = snapshot.tracks;
        self.pending_events = snapshot.pending_events;
        self.seek_index = snapshot.seek_index;
        self.duration = snapshot.duration;
        self.reached_end = snapshot.reached_end;
    }

    fn capture_index_continuation(&self) -> IndexContinuationState {
        IndexContinuationState {
            pat_assembler: self.pat_assembler.clone(),
            pmt_assemblers: self.pmt_assemblers.clone(),
            pat_version: self.pat_version,
            pat_programs: self.pat_programs.clone(),
            program_maps: self.program_maps.clone(),
            selected_program: self.selected_program.clone(),
            stream_by_pid: self.stream_by_pid.clone(),
            continuity_by_pid: self.continuity_by_pid.clone(),
            pes_by_pid: self.pes_by_pid.clone(),
            timestamps_by_pid: self.timestamps_by_pid.clone(),
            pcr_timestamp: self.pcr_timestamp.clone(),
            audio_by_pid: self.audio_by_pid.clone(),
            video_assembler: self.video_assembler.clone(),
            audio_evidence_by_pid: self.audio_evidence_by_pid.clone(),
            config_evidence_by_pid: self.config_evidence_by_pid.clone(),
        }
    }

    fn install_index_continuation(&mut self, continuation: IndexContinuationState) {
        self.pat_assembler = continuation.pat_assembler;
        self.pmt_assemblers = continuation.pmt_assemblers;
        self.pat_version = continuation.pat_version;
        self.pat_programs = continuation.pat_programs;
        self.program_maps = continuation.program_maps;
        self.selected_program = continuation.selected_program;
        self.stream_by_pid = continuation.stream_by_pid;
        self.continuity_by_pid = continuation.continuity_by_pid;
        self.pes_by_pid = continuation.pes_by_pid;
        self.timestamps_by_pid = continuation.timestamps_by_pid;
        self.pcr_timestamp = continuation.pcr_timestamp;
        self.audio_by_pid = continuation.audio_by_pid;
        self.video_assembler = continuation.video_assembler;
        self.audio_evidence_by_pid = continuation.audio_evidence_by_pid;
        self.config_evidence_by_pid = continuation.config_evidence_by_pid;
    }

    pub(super) fn expand_index_towards(
        &mut self,
        target_90khz: i64,
        mode: DemuxSeekMode,
    ) -> Result<(), MpegTsDemuxError> {
        if self.index_reached_end || self.index_covers(target_90khz, mode) {
            return Ok(());
        }
        if self.seek_index.len() >= self.options.index_entries.get() {
            return Ok(());
        }

        let resume_offset = self.reader.position();
        let snapshot = self.snapshot_parser_state();
        let committed_continuation = self.index_continuation.clone();
        let scan_result = (|| {
            self.reader.begin_index_scan(self.index_scan_offset)?;
            if let Some(continuation) = committed_continuation {
                self.install_index_continuation(continuation);
            } else {
                self.reset_elementary_state();
            }
            if self.index_continuation.is_none()
                && let Some(reference) = self
                    .seek_index
                    .iter()
                    .map(|anchor| anchor.timestamp_90khz)
                    .max()
            {
                self.pcr_timestamp = TimestampUnwrapper::from_unwrapped_reference(reference);
                for pid in self.stream_by_pid.keys().copied() {
                    self.timestamps_by_pid.insert(
                        pid,
                        StreamTimestamps {
                            pts: TimestampUnwrapper::from_unwrapped_reference(reference),
                            dts: TimestampUnwrapper::from_unwrapped_reference(reference),
                        },
                    );
                }
            }
            self.pending_events.clear();
            self.reached_end = false;
            let mut reached_end = false;
            for _ in 0..self.options.seek_scan_packets.get() {
                let Some(packet) = self.reader.next_packet()? else {
                    self.finish_pending_elementary_streams(false)?;
                    reached_end = true;
                    break;
                };
                self.process_transport_packet(packet, false)?;
                if self.index_covers(target_90khz, mode)
                    || self.seek_index.len() >= self.options.index_entries.get()
                {
                    break;
                }
            }
            let continuation = (!reached_end).then(|| self.capture_index_continuation());
            Ok((
                self.seek_index.clone(),
                self.reader.position(),
                reached_end,
                continuation,
            ))
        })();
        let rollback_result = self.reader.restore_after_index_scan(resume_offset);
        self.restore_parser_state(snapshot);
        rollback_result?;
        let (expanded_index, coverage_offset, reached_end, continuation) = scan_result?;
        self.seek_index = expanded_index;
        self.index_scan_offset = coverage_offset;
        self.index_reached_end = reached_end;
        self.index_continuation = continuation;
        Ok(())
    }

    pub(super) fn index_covers(&self, target_90khz: i64, mode: DemuxSeekMode) -> bool {
        let reached_target = self
            .seek_index
            .iter()
            .any(|anchor| anchor.timestamp_90khz >= target_90khz);
        if !reached_target {
            return false;
        }
        mode != DemuxSeekMode::DecodePointBefore
            || self
                .seek_index
                .iter()
                .any(|anchor| anchor.decode_safe && anchor.timestamp_90khz <= target_90khz)
    }
}
