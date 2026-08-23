use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use media_core::{TimeBase, TrackId, TrackTimestamp};

use super::*;
use crate::ffi::packet::PaddedPacketBytes;

/// Test-only thin wrappers that drive send/drain with an unbounded pool budget,
/// so existing tests keep asserting pure send/receive behaviour.
#[cfg(test)]
impl<A: SendReceiveCodecApi> SendReceiveDecodeLoop<A> {
    pub(super) fn send_packet_for_test(
        &mut self,
        packet: DecodePacket,
    ) -> Result<DecodeProgressReport, FfmpegDecoderThreadError> {
        match self.send_packet(packet, usize::MAX)? {
            SendPacketOutcome::Consumed(progress) => Ok(progress),
            SendPacketOutcome::Deferred { .. } => {
                panic!("unbounded test budget must never defer a packet")
            }
        }
    }

    pub(super) fn begin_end_of_stream_drain_for_test(
        &mut self,
        generation: u64,
    ) -> Result<EofDrainProgressReport, FfmpegDecoderThreadError> {
        self.begin_end_of_stream_drain(generation, usize::MAX)
    }
}

#[cfg(all(test, feature = "ffmpeg"))]
impl FfmpegHostResourceProvider {
    /// Builds a provider with a detached release wake-up channel for tests.
    pub(super) fn new_for_test(upload_slots_capacity: usize) -> Self {
        let (release_notify_tx, _release_notify_rx) = bounded(1);
        Self::new(upload_slots_capacity, release_notify_tx)
    }
}

#[derive(Debug, Clone)]
pub(super) struct ScriptedDecodeApi {
    pub(super) next_packet_id: u64,
    pub(super) created_packets: Vec<FakePacket>,
    pub(super) sent_packet_ids: Vec<u64>,
    pub(super) send_results: VecDeque<FakeSendResult>,
    pub(super) receive_results: VecDeque<FakeReceiveResult>,
    pub(super) end_of_stream_send_count: usize,
    pub(super) flush_buffers_count: usize,
}

impl Default for ScriptedDecodeApi {
    fn default() -> Self {
        Self {
            next_packet_id: 1,
            created_packets: Vec::new(),
            sent_packet_ids: Vec::new(),
            send_results: VecDeque::new(),
            receive_results: VecDeque::new(),
            end_of_stream_send_count: 0,
            flush_buffers_count: 0,
        }
    }
}

impl ScriptedDecodeApi {
    pub(super) fn with_send_results(
        mut self,
        send_results: impl IntoIterator<Item = FakeSendResult>,
    ) -> Self {
        self.push_send_results(send_results);
        self
    }

    pub(super) fn with_receive_results(
        mut self,
        receive_results: impl IntoIterator<Item = FakeReceiveResult>,
    ) -> Self {
        self.push_receive_results(receive_results);
        self
    }

    pub(super) fn push_send_results(
        &mut self,
        send_results: impl IntoIterator<Item = FakeSendResult>,
    ) {
        self.send_results.extend(send_results);
    }

    pub(super) fn push_receive_results(
        &mut self,
        receive_results: impl IntoIterator<Item = FakeReceiveResult>,
    ) {
        self.receive_results.extend(receive_results);
    }
}

impl SendReceiveCodecApi for ScriptedDecodeApi {
    type Packet = FakePacket;

    fn create_packet(
        &mut self,
        packet: &DecodePacket,
    ) -> Result<Self::Packet, FfmpegDecoderThreadError> {
        let fake_packet = FakePacket {
            id: self.next_packet_id,
            padded_bytes: PaddedPacketBytes::new(packet.encoded_bytes.as_ref()),
        };
        self.next_packet_id = self.next_packet_id.saturating_add(1);
        self.created_packets.push(fake_packet.clone());
        Ok(fake_packet)
    }

    fn send_packet(&mut self, packet: &Self::Packet) -> Result<(), DecodeApiError> {
        self.sent_packet_ids.push(packet.id);

        match self
            .send_results
            .pop_front()
            .unwrap_or(FakeSendResult::Accepted)
        {
            FakeSendResult::Accepted => Ok(()),
            FakeSendResult::Again => Err(DecodeApiError::Again),
            FakeSendResult::EndOfFile => Err(DecodeApiError::EndOfFile),
        }
    }

    fn send_end_of_stream(&mut self) -> Result<(), DecodeApiError> {
        self.end_of_stream_send_count = self.end_of_stream_send_count.saturating_add(1);

        match self
            .send_results
            .pop_front()
            .unwrap_or(FakeSendResult::Accepted)
        {
            FakeSendResult::Accepted => Ok(()),
            FakeSendResult::Again => Err(DecodeApiError::Again),
            FakeSendResult::EndOfFile => Err(DecodeApiError::EndOfFile),
        }
    }

    fn receive_frame(&mut self) -> Result<ReceivedFrameMetadata, DecodeApiError> {
        match self
            .receive_results
            .pop_front()
            .unwrap_or(FakeReceiveResult::Again)
        {
            FakeReceiveResult::Frame(timestamps) => Ok(ReceivedFrameMetadata {
                timestamps,
                #[cfg(feature = "ffmpeg")]
                frame_ref: None,
                #[cfg(feature = "ffmpeg")]
                color: None,
            }),
            FakeReceiveResult::Again => Err(DecodeApiError::Again),
            FakeReceiveResult::EndOfFile => Err(DecodeApiError::EndOfFile),
            FakeReceiveResult::Fatal(reason) => Err(DecodeApiError::Fatal(
                FfmpegDecoderThreadError::ProtocolViolation {
                    reason: reason.to_owned(),
                },
            )),
        }
    }

    fn flush_buffers(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        self.flush_buffers_count = self.flush_buffers_count.saturating_add(1);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FakePacket {
    id: u64,
    padded_bytes: PaddedPacketBytes,
}

impl FakePacket {
    pub(super) fn payload(&self) -> &[u8] {
        self.padded_bytes.payload()
    }

    pub(super) fn padded_bytes(&self) -> &[u8] {
        self.padded_bytes.padded_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeSendResult {
    Accepted,
    Again,
    EndOfFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeReceiveResult {
    Frame(FrameTimestamps),
    Again,
    EndOfFile,
    Fatal(&'static str),
}

pub(super) fn fake_loop(
    send_results: impl IntoIterator<Item = FakeSendResult>,
    receive_results: impl IntoIterator<Item = FakeReceiveResult>,
) -> SendReceiveDecodeLoop<ScriptedDecodeApi> {
    let fake_api = ScriptedDecodeApi::default()
        .with_send_results(send_results)
        .with_receive_results(receive_results);
    let (activity_notifier, _activity_subscription) = VideoDecoderActivityNotifier::new();

    SendReceiveDecodeLoop::new(fake_api, activity_notifier, shared_idle_drain_state())
}

#[cfg(feature = "ffmpeg")]
pub(super) fn host_planar_contract(
    pixel_layout: VideoFramePixelLayout,
) -> video_frame_contract::VideoFrameContract {
    video_frame_contract::VideoFrameContract {
        pixel_layout,
        transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
    }
}

#[cfg(feature = "ffmpeg")]
pub(super) fn extradata_test_stream_config(
    codec: codec_core::VideoCodec,
    codec_private: Option<Bytes>,
    packetization: Option<VideoStreamPacketization>,
) -> VideoStreamDecodeConfig {
    let requirement = VideoDecodeRequirement::new(codec);
    VideoStreamDecodeConfig::from_requirement(
        TrackId::new(1),
        &requirement,
        host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
    )
    .with_codec_private(codec_private)
    .with_packetization(packetization)
}

#[cfg(feature = "ffmpeg")]
pub(super) fn test_yuv420_frame(width: i32, height: i32, alignment: i32) -> OwnedAvFrame {
    OwnedAvFrame::new_test_video_frame(SoftwarePixelFormat::Yuv420Planar8, width, height, alignment)
        .expect("test AVFrame allocation should succeed")
}

#[cfg(feature = "ffmpeg")]
pub(super) fn lookup_host_planar_descriptor(
    provider: &FfmpegHostResourceProvider,
    handle: FrameResourceHandle,
) -> HostPlanarFrameDescriptor {
    match provider.resource_descriptor_lookup(handle) {
        PresentFrameResourceDescriptorLookup::Ready {
            descriptor: FrameResourceDescriptor::HostPlanar(descriptor),
            ..
        } => descriptor,
        other => panic!("expected ready host-planar descriptor lookup, got {other:?}"),
    }
}

pub(super) fn shared_idle_drain_state() -> Arc<Mutex<VideoDecoderEndOfStreamDrainState>> {
    Arc::new(Mutex::new(VideoDecoderEndOfStreamDrainState::Idle))
}

pub(super) fn frame_timestamps(
    best_effort_timestamp: i64,
    pts: i64,
    duration: i64,
) -> FrameTimestamps {
    FrameTimestamps {
        best_effort_timestamp,
        pts,
        packet_dts: NO_TIMESTAMP,
        duration,
    }
}

pub(super) fn decode_packet_with_pts(
    generation: u64,
    dts_units: i64,
    pts: Duration,
) -> DecodePacket {
    let track_id = TrackId::new(1);

    DecodePacket {
        track_id,
        pts,
        dts: None,
        track_pts: Some(TrackTimestamp::new(
            track_id,
            dts_units,
            TimeBase::new(1, 1_000).expect("test time base is valid"),
        )),
        track_dts: Some(TrackTimestamp::new(
            track_id,
            dts_units,
            TimeBase::new(1, 1_000).expect("test time base is valid"),
        )),
        generation,
        encoded_bytes: Bytes::from(vec![1, 2, 3, generation as u8]),
        keyframe: true,
        resolved_color: Some(VideoColorMetadata::sdr_bt709_limited()),
    }
}
