use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use demux_api::DemuxInput;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaMetadata, Packet, PacketKeyframe,
    TimelineNotSeekableReason, TrackInfo, TrackKind,
};
use source_core::CancellationToken;

use crate::codec::{
    AUDIO_TRACK_ID, CodecTagEvent, EncodedTagPacket, TrackConfiguration, VIDEO_TRACK_ID,
    legacy_audio_configuration, parse_audio_tag, parse_video_tag,
};
use crate::framing::{FlvTag, FlvTagKind, previous_tag_size_bytes, tag_header_bytes};
use crate::input::{FlvInput, InputDiscontinuity, InputTag};
use crate::metadata::{FlvMetadata, MetadataAnchor, parse_on_metadata};
use crate::timestamp::MillisecondTimestampUnwrapper;
use crate::{FlvDemuxError, FlvDemuxOptions};

#[derive(Debug, Clone)]
struct SeekAnchor {
    timestamp: Duration,
    byte_offset: u64,
    video_configuration: TrackConfiguration,
    audio_configuration: Option<TrackConfiguration>,
    video_clock: MillisecondTimestampUnwrapper,
    audio_clock: MillisecondTimestampUnwrapper,
}

#[derive(Clone)]
struct ParserSnapshot {
    video_configuration: Option<TrackConfiguration>,
    audio_configuration: Option<TrackConfiguration>,
    tracks: Vec<TrackInfo>,
    pending_events: VecDeque<DemuxReadEvent>,
    video_clock: MillisecondTimestampUnwrapper,
    audio_clock: MillisecondTimestampUnwrapper,
    video_sequence_active: bool,
    audio_sequence_active: bool,
    recovery_gate: bool,
    recovery_remaining_bytes: usize,
    force_tracks_changed_on_next_config: bool,
    config_generation: u64,
    seek_index: Vec<SeekAnchor>,
    metadata_anchors: Vec<MetadataAnchor>,
    duration: Option<Duration>,
    media_metadata: Option<MediaMetadata>,
    reached_end: bool,
}

/// Stateful FLV/F4F demuxer; все mutable container invariants живут здесь.
pub struct FlvDemuxer {
    input: FlvInput,
    options: FlvDemuxOptions,
    video_configuration: Option<TrackConfiguration>,
    audio_configuration: Option<TrackConfiguration>,
    tracks: Vec<TrackInfo>,
    pending_events: VecDeque<DemuxReadEvent>,
    video_clock: MillisecondTimestampUnwrapper,
    audio_clock: MillisecondTimestampUnwrapper,
    video_sequence_active: bool,
    audio_sequence_active: bool,
    recovery_gate: bool,
    recovery_remaining_bytes: usize,
    force_tracks_changed_on_next_config: bool,
    config_generation: u64,
    seek_index: Vec<SeekAnchor>,
    metadata_anchors: Vec<MetadataAnchor>,
    duration: Option<Duration>,
    media_metadata: Option<MediaMetadata>,
    reached_end: bool,
}

impl FlvDemuxer {
    /// Открывает exact selected container и выполняет bounded initial track discovery.
    pub(crate) fn open(
        input: DemuxInput,
        is_f4f: bool,
        cancellation: CancellationToken,
        options: FlvDemuxOptions,
    ) -> Result<Self, FlvDemuxError> {
        let input = if is_f4f {
            FlvInput::open_f4f(input, cancellation, options)?
        } else {
            FlvInput::open_raw(input, cancellation, options)?
        };
        let mut demuxer = Self {
            input,
            options,
            video_configuration: None,
            audio_configuration: None,
            tracks: Vec::new(),
            pending_events: VecDeque::new(),
            video_clock: MillisecondTimestampUnwrapper::default(),
            audio_clock: MillisecondTimestampUnwrapper::default(),
            video_sequence_active: false,
            audio_sequence_active: false,
            recovery_gate: false,
            recovery_remaining_bytes: 0,
            force_tracks_changed_on_next_config: false,
            config_generation: 0,
            seek_index: Vec::new(),
            metadata_anchors: Vec::new(),
            duration: None,
            media_metadata: None,
            reached_end: false,
        };
        for _ in 0..options.initial_tags.get() {
            let Some(input_tag) = demuxer.input.next_tag()? else {
                break;
            };
            demuxer.process_input_tag(input_tag, false)?;
            if !demuxer.tracks.is_empty() {
                demuxer
                    .pending_events
                    .retain(|event| !matches!(event, DemuxReadEvent::TracksChanged(_)));
                return Ok(demuxer);
            }
        }
        Err(FlvDemuxError::NoPlayableTrack)
    }

    fn process_input_tag(
        &mut self,
        input_tag: InputTag,
        emit_updates: bool,
    ) -> Result<(), FlvDemuxError> {
        if input_tag.discontinuity == InputDiscontinuity::StartsNewTimeline {
            self.begin_discontinuity();
        }
        self.process_tag(input_tag.tag, emit_updates)
    }

    fn process_tag(&mut self, tag: FlvTag, emit_updates: bool) -> Result<(), FlvDemuxError> {
        self.charge_recovery_budget(&tag)?;
        match tag.kind {
            FlvTagKind::Script => {
                if let Some(metadata) = parse_on_metadata(&tag.payload, self.options)? {
                    self.apply_metadata(metadata, emit_updates);
                }
                Ok(())
            }
            FlvTagKind::Audio => {
                let implicit_configuration = legacy_audio_configuration(&tag.payload)?;
                let event = parse_audio_tag(&tag.payload)?;
                if let Some(configuration) = implicit_configuration {
                    self.apply_configuration(configuration, emit_updates);
                }
                self.apply_codec_event(event, tag.timestamp_ms, tag.byte_offset, emit_updates)
            }
            FlvTagKind::Video => {
                let event = parse_video_tag(&tag.payload, self.video_configuration.as_ref())?;
                self.apply_codec_event(event, tag.timestamp_ms, tag.byte_offset, emit_updates)
            }
        }
    }

    fn apply_codec_event(
        &mut self,
        event: CodecTagEvent,
        timestamp_ms: u32,
        byte_offset: u64,
        emit_updates: bool,
    ) -> Result<(), FlvDemuxError> {
        match event {
            CodecTagEvent::Configuration(configuration) => {
                self.apply_configuration(configuration, emit_updates);
                Ok(())
            }
            CodecTagEvent::SequenceEnd { track_id } => {
                if track_id == VIDEO_TRACK_ID {
                    self.video_sequence_active = false;
                } else if track_id == AUDIO_TRACK_ID {
                    self.audio_sequence_active = false;
                }
                Ok(())
            }
            CodecTagEvent::Packet(packet) => self.emit_packet(packet, timestamp_ms, byte_offset),
        }
    }

    fn apply_configuration(&mut self, configuration: TrackConfiguration, emit_updates: bool) {
        let is_video = configuration.track.kind == TrackKind::Video;
        let slot = if is_video {
            &mut self.video_configuration
        } else {
            &mut self.audio_configuration
        };
        let changed = slot.as_ref() != Some(&configuration);
        if changed {
            *slot = Some(configuration);
            self.config_generation = self.config_generation.saturating_add(1);
        }
        if is_video {
            self.video_sequence_active = true;
        } else {
            self.audio_sequence_active = true;
        }
        self.rebuild_tracks();
        let forced = self.force_tracks_changed_on_next_config;
        if forced {
            self.force_tracks_changed_on_next_config = false;
        }
        if emit_updates && (changed || forced) {
            self.pending_events.push_back(DemuxReadEvent::TracksChanged(
                DemuxTrackListUpdate::new(self.tracks.clone(), self.duration),
            ));
        }
    }

    fn emit_packet(
        &mut self,
        encoded: EncodedTagPacket,
        raw_timestamp_ms: u32,
        byte_offset: u64,
    ) -> Result<(), FlvDemuxError> {
        let sequence_active = match encoded.kind {
            TrackKind::Video => self.video_sequence_active,
            TrackKind::Audio => self.audio_sequence_active,
        };
        if self.recovery_gate && !sequence_active {
            return Ok(());
        }
        if !sequence_active {
            return Err(FlvDemuxError::InvalidConfiguration {
                codec: if encoded.kind == TrackKind::Video {
                    "video"
                } else {
                    "audio"
                },
                reason: "packet получен после SequenceEnd/discontinuity без новой SequenceStart"
                    .to_owned(),
            });
        }
        if self.recovery_gate {
            let can_release = if self.video_configuration.is_some() {
                encoded.kind == TrackKind::Video
                    && encoded.keyframe == PacketKeyframe::Keyframe
                    && self.video_sequence_active
            } else {
                encoded.kind == TrackKind::Audio && self.audio_sequence_active
            };
            if !can_release {
                return Ok(());
            }
            self.recovery_gate = false;
            self.recovery_remaining_bytes = 0;
        }
        let decoded_timestamp_ms = match encoded.kind {
            TrackKind::Video => self.video_clock.unwrap(raw_timestamp_ms),
            TrackKind::Audio => self.audio_clock.unwrap(raw_timestamp_ms),
        };
        let presentation_ms = i128::from(decoded_timestamp_ms)
            .saturating_add(i128::from(encoded.composition_offset_ms));
        let presentation_ms = u64::try_from(presentation_ms.max(0)).unwrap_or(u64::MAX);
        let dts = Duration::from_millis(decoded_timestamp_ms);
        let packet = Packet::new_with_keyframe_unbounded(
            encoded.track_id,
            encoded.kind,
            Duration::from_millis(presentation_ms),
            (encoded.kind == TrackKind::Video).then_some(dts),
            encoded.keyframe,
            encoded.bytes,
        )
        .with_byte_offset(byte_offset);
        if encoded.kind == TrackKind::Video
            && encoded.keyframe == PacketKeyframe::Keyframe
            && let Some(video_configuration) = self.video_configuration.clone()
        {
            self.push_seek_anchor(SeekAnchor {
                timestamp: Duration::from_millis(presentation_ms),
                byte_offset,
                video_configuration,
                audio_configuration: self.audio_configuration.clone(),
                video_clock: self.video_clock,
                audio_clock: self.audio_clock,
            });
        }
        self.pending_events
            .push_back(DemuxReadEvent::Packet(packet));
        Ok(())
    }

    fn push_seek_anchor(&mut self, anchor: SeekAnchor) {
        if self.seek_index.len() >= self.options.index_entries.get() {
            return;
        }
        if self
            .seek_index
            .last()
            .is_some_and(|previous| previous.byte_offset == anchor.byte_offset)
        {
            return;
        }
        self.seek_index.push(anchor);
    }

    fn apply_metadata(&mut self, metadata: FlvMetadata, emit_updates: bool) {
        self.duration = metadata.duration.or(self.duration);
        self.metadata_anchors = metadata.anchors;
        self.media_metadata = Some(metadata.media_metadata.clone());
        if emit_updates {
            self.pending_events
                .push_back(DemuxReadEvent::MediaMetadataChanged(
                    metadata.media_metadata,
                ));
        }
    }

    fn rebuild_tracks(&mut self) {
        self.tracks.clear();
        if let Some(configuration) = &self.video_configuration {
            self.tracks.push(configuration.track.clone());
        }
        if let Some(configuration) = &self.audio_configuration {
            self.tracks.push(configuration.track.clone());
        }
    }

    fn begin_discontinuity(&mut self) {
        let starts_new_recovery = !self.recovery_gate;
        self.video_clock = MillisecondTimestampUnwrapper::default();
        self.audio_clock = MillisecondTimestampUnwrapper::default();
        self.video_sequence_active = false;
        self.audio_sequence_active = false;
        self.recovery_gate = true;
        if starts_new_recovery {
            self.recovery_remaining_bytes = self.options.recovery_bytes.get();
        }
        self.force_tracks_changed_on_next_config = true;
        self.pending_events.clear();
    }

    /// Ограничивает config/keyframe reacquisition тем же именованным byte budget.
    fn charge_recovery_budget(&mut self, tag: &FlvTag) -> Result<(), FlvDemuxError> {
        if !self.recovery_gate {
            return Ok(());
        }
        let wire_bytes = tag_header_bytes()
            .checked_add(tag.payload.len())
            .and_then(|bytes| bytes.checked_add(previous_tag_size_bytes()))
            .ok_or(FlvDemuxError::RecoveryGateBudgetExhausted {
                processed_bytes: self
                    .options
                    .recovery_bytes
                    .get()
                    .saturating_sub(self.recovery_remaining_bytes),
                next_tag_bytes: usize::MAX,
                limit_bytes: self.options.recovery_bytes.get(),
            })?;
        let Some(remaining_bytes) = self.recovery_remaining_bytes.checked_sub(wire_bytes) else {
            return Err(FlvDemuxError::RecoveryGateBudgetExhausted {
                processed_bytes: self
                    .options
                    .recovery_bytes
                    .get()
                    .saturating_sub(self.recovery_remaining_bytes),
                next_tag_bytes: wire_bytes,
                limit_bytes: self.options.recovery_bytes.get(),
            });
        };
        self.recovery_remaining_bytes = remaining_bytes;
        Ok(())
    }

    fn recover_after_framing_loss(&mut self) -> Result<bool, FlvDemuxError> {
        let recovered = self.input.recover_raw_tag()?;
        let Some(tag) = recovered else {
            return Ok(false);
        };
        self.process_input_tag(tag, true)?;
        Ok(true)
    }

    fn parser_snapshot(&self) -> ParserSnapshot {
        ParserSnapshot {
            video_configuration: self.video_configuration.clone(),
            audio_configuration: self.audio_configuration.clone(),
            tracks: self.tracks.clone(),
            pending_events: self.pending_events.clone(),
            video_clock: self.video_clock,
            audio_clock: self.audio_clock,
            video_sequence_active: self.video_sequence_active,
            audio_sequence_active: self.audio_sequence_active,
            recovery_gate: self.recovery_gate,
            recovery_remaining_bytes: self.recovery_remaining_bytes,
            force_tracks_changed_on_next_config: self.force_tracks_changed_on_next_config,
            config_generation: self.config_generation,
            seek_index: self.seek_index.clone(),
            metadata_anchors: self.metadata_anchors.clone(),
            duration: self.duration,
            media_metadata: self.media_metadata.clone(),
            reached_end: self.reached_end,
        }
    }

    fn restore_parser_snapshot(&mut self, snapshot: ParserSnapshot) {
        self.video_configuration = snapshot.video_configuration;
        self.audio_configuration = snapshot.audio_configuration;
        self.tracks = snapshot.tracks;
        self.pending_events = snapshot.pending_events;
        self.video_clock = snapshot.video_clock;
        self.audio_clock = snapshot.audio_clock;
        self.video_sequence_active = snapshot.video_sequence_active;
        self.audio_sequence_active = snapshot.audio_sequence_active;
        self.recovery_gate = snapshot.recovery_gate;
        self.recovery_remaining_bytes = snapshot.recovery_remaining_bytes;
        self.force_tracks_changed_on_next_config = snapshot.force_tracks_changed_on_next_config;
        self.config_generation = snapshot.config_generation;
        self.seek_index = snapshot.seek_index;
        self.metadata_anchors = snapshot.metadata_anchors;
        self.duration = snapshot.duration;
        self.media_metadata = snapshot.media_metadata;
        self.reached_end = snapshot.reached_end;
    }

    fn build_index_until(&mut self, target: Duration) -> Result<(), FlvDemuxError> {
        let Some(first_tag_offset) = self.input.first_tag_offset() else {
            return Err(FlvDemuxError::NotSeekable);
        };
        self.input.seek_raw_tag(first_tag_offset)?;
        self.video_configuration = None;
        self.audio_configuration = None;
        self.video_sequence_active = false;
        self.audio_sequence_active = false;
        self.video_clock = MillisecondTimestampUnwrapper::default();
        self.audio_clock = MillisecondTimestampUnwrapper::default();
        self.pending_events.clear();
        self.seek_index.clear();
        let scan_limit = self.options.seek_scan_tags.get();
        let mut scanned_tags = 0_usize;
        let mut reached_end = false;
        let mut target_covered = false;
        for _ in 0..scan_limit {
            let Some(tag) = self.input.next_tag()? else {
                reached_end = true;
                break;
            };
            scanned_tags += 1;
            self.process_input_tag(tag, false)?;
            self.pending_events.clear();
            if self
                .seek_index
                .last()
                .is_some_and(|anchor| anchor.timestamp >= target)
            {
                target_covered = true;
                break;
            }
        }
        if !reached_end && !target_covered {
            return Err(FlvDemuxError::SeekScanBudgetExhausted {
                target,
                scanned_tags,
            });
        }
        Ok(())
    }

    fn seek_transactional(&mut self, target: Duration) -> Result<DemuxSeekResult, FlvDemuxError> {
        if !self.input.is_seekable() {
            return Err(FlvDemuxError::NotSeekable);
        }
        let old_position = self.input.position();
        let snapshot = self.parser_snapshot();
        let scan_result = self.build_index_until(target);
        if let Err(error) = scan_result {
            return Err(self.rollback_failed_seek(old_position, snapshot, error));
        }
        let selected = self
            .seek_index
            .iter()
            .rev()
            .find(|anchor| anchor.timestamp <= target)
            .cloned();
        let Some(anchor) = selected else {
            return Err(self.rollback_failed_seek(
                old_position,
                snapshot,
                FlvDemuxError::SeekAnchorUnavailable { target },
            ));
        };
        if let Err(error) = self.input.seek_raw_tag(anchor.byte_offset) {
            return Err(self.rollback_failed_seek(old_position, snapshot, error));
        }
        let configuration_changed = snapshot.video_configuration.as_ref()
            != Some(&anchor.video_configuration)
            || snapshot.audio_configuration != anchor.audio_configuration;
        self.video_configuration = Some(anchor.video_configuration);
        self.audio_configuration = anchor.audio_configuration;
        self.video_sequence_active = true;
        self.audio_sequence_active = self.audio_configuration.is_some();
        self.video_clock = anchor.video_clock;
        self.audio_clock = anchor.audio_clock;
        self.pending_events.clear();
        self.recovery_gate = false;
        self.recovery_remaining_bytes = 0;
        self.force_tracks_changed_on_next_config = false;
        self.reached_end = false;
        self.rebuild_tracks();
        if configuration_changed {
            self.pending_events.push_back(DemuxReadEvent::TracksChanged(
                DemuxTrackListUpdate::new(self.tracks.clone(), self.duration),
            ));
        }
        Ok(DemuxSeekResult {
            requested_position: target.into(),
            actual_position: anchor.timestamp.into(),
            actual_track_timestamp: None,
        })
    }

    fn rollback_failed_seek(
        &mut self,
        old_position: u64,
        snapshot: ParserSnapshot,
        original_error: FlvDemuxError,
    ) -> FlvDemuxError {
        let rollback_result = self.input.seek_raw_tag(old_position);
        self.restore_parser_snapshot(snapshot);
        match rollback_result {
            Ok(()) => original_error,
            Err(rollback_error) => FlvDemuxError::Source {
                reason: format!(
                    "seek failure `{original_error}`; rollback к offset {old_position} также failed: {rollback_error}"
                ),
            },
        }
    }
}

impl Demuxer for FlvDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.media_metadata.clone()
    }

    fn seekability(&self) -> DemuxSeekability {
        if !self.input.is_seekable() {
            return DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            };
        }
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(event);
        }
        if self.reached_end {
            return Ok(DemuxReadEvent::EndOfStream);
        }
        loop {
            match self.input.next_tag() {
                Ok(Some(tag)) => self.process_input_tag(tag, true)?,
                Ok(None) => {
                    self.reached_end = true;
                    return Ok(DemuxReadEvent::EndOfStream);
                }
                Err(
                    FlvDemuxError::MalformedTag { .. }
                    | FlvDemuxError::Source { .. }
                    | FlvDemuxError::TagTooLarge { .. },
                ) if self.input.can_recover_raw() => {
                    if !self.recover_after_framing_loss()? {
                        self.reached_end = true;
                        return Ok(DemuxReadEvent::EndOfStream);
                    }
                }
                Err(error) => return Err(error.into()),
            }
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(event);
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_transactional(timestamp).map_err(Into::into)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        match request.mode {
            DemuxSeekMode::Accurate | DemuxSeekMode::DecodePointBefore | DemuxSeekMode::Preview => {
                self.seek_transactional(request.timestamp)
                    .map_err(Into::into)
            }
        }
    }
}
