//! Управляемая EOF publication доводит настоящий VP9 кадр до WGPU readback.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// Источник уже отдал все packets; downstream обязан забрать EOF tail.
struct EndedDemuxer;

impl Demuxer for EndedDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }
    fn duration(&self) -> Option<Duration> {
        None
    }
    fn seekability(&self) -> media_core::DemuxSeekability {
        media_core::DemuxSeekability::Seekable
    }
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }
    fn seek(&mut self, _: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        panic!("EOF consumer must not seek")
    }
}

/// Управляет только очередностью публикации; ресурс и release принадлежат real backend.
struct ScheduledEofDecoder {
    decoder: Box<VideoBackendDecoderThreadHandle>,
    tail_decoder: Box<VideoBackendDecoderThreadHandle>,
    frames: Mutex<VecDeque<DecodedFrame>>,
    released_tail_frames: AtomicUsize,
    state_reads: AtomicUsize,
    begin_calls: AtomicUsize,
}

impl video_core::VideoDecoderThreadHandle for ScheduledEofDecoder {
    type ResourceProvider = PresentFrameResourceProviderHandle;
    fn backend_name(&self) -> &'static str {
        "scheduled real VP9 EOF publication"
    }
    fn send_packet(&self, _: DecodePacket) -> Result<(), DecodeSendError> {
        panic!("all packets already consumed")
    }
    fn configure_stream(&self, _: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        panic!("decoder already configured")
    }
    fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        assert_eq!(generation, DECODE_GENERATION);
        assert_eq!(self.begin_calls.fetch_add(1, Ordering::SeqCst), 0);
        VideoDecoderEndOfStreamDrainResult::Started(VideoDecoderEndOfStreamDrainState::Draining {
            generation,
        })
    }
    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        if self.state_reads.fetch_add(1, Ordering::SeqCst) == 0 {
            VideoDecoderEndOfStreamDrainState::Draining {
                generation: DECODE_GENERATION,
            }
        } else {
            assert!(self.frames.lock().expect("frame mailbox").is_empty());
            VideoDecoderEndOfStreamDrainState::Drained {
                generation: DECODE_GENERATION,
            }
        }
    }
    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        if self.state_reads.load(Ordering::SeqCst) == 0 {
            return None;
        }
        self.frames.lock().expect("frame mailbox").pop_front()
    }
    fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        // Лишний кадр принадлежит второму настоящему decoder pool.
        self.tail_decoder.release_frame(handle);
        self.released_tail_frames.fetch_add(1, Ordering::SeqCst);
    }
    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        self.decoder.try_recv_diagnostic_event()
    }
    fn try_recv_error(&self) -> Option<video_core::DecodeThreadError> {
        self.decoder.try_recv_error()
    }
    fn flush(&self) -> anyhow::Result<()> {
        panic!("EOF must not flush retained frame")
    }
    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        self.decoder.resource_provider()
    }
    fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
        self.decoder.decoder_resource_snapshot()
    }
    fn packet_queue_depth(&self) -> usize {
        self.decoder.packet_queue_depth()
    }
    fn drain_completed_packet_count(&self) -> usize {
        self.decoder.drain_completed_packet_count()
    }
}

#[test]
fn pending_eof_publication_reaches_real_wgpu_submit_readback_and_release() {
    let webm = base64::engine::general_purpose::STANDARD
        .decode(MUXED_WEBM_BASE64)
        .expect("WebM fixture");
    let origin = RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::Ogg(webm));
    let classified = crate::direct_progressive_open::classify_direct_media_url(
        &origin.media_url_with_extension("webm"),
    )
    .expect("direct URL");
    let config = rustiplayer_config::AppConfig::default();
    let opened = crate::direct_progressive_open::open_direct_media(
        &classified,
        &config.network,
        &config.player.demux,
        CancellationToken::new(),
    )
    .expect("open real WebM");
    let (mut demuxer, _recovery) = opened.into_runtime_parts();
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("VP9 track");
    let mut renderer = OffscreenWgpuHarness::new();
    let (decoder, provider) = open_decoder(track, &renderer.queue, VideoCodec::Vp9);
    let (tail_decoder, _) = open_decoder(track, &renderer.queue, VideoCodec::Vp9);
    let frame = decode_first_frame(demuxer.as_mut(), decoder.as_ref());
    // Первый helper полностью drain-ит fixture; второй независимый decoder
    // даёт настоящий дополнительный ресурс, не повторяя EOS завершённого worker-а.
    let reopened = crate::direct_progressive_open::open_direct_media(
        &classified,
        &config.network,
        &config.player.demux,
        CancellationToken::new(),
    )
    .expect("reopen WebM for second real frame");
    let (mut tail_demuxer, _tail_recovery) = reopened.into_runtime_parts();
    let second_frame = decode_first_frame(tail_demuxer.as_mut(), tail_decoder.as_ref());
    // Два реальных кадра гарантируют release лишнего EOF tail, а первый доходит до render.
    let scheduled = ScheduledEofDecoder {
        decoder,
        tail_decoder,
        frames: Mutex::new(VecDeque::from([frame, second_frame])),
        released_tail_frames: AtomicUsize::new(0),
        state_reads: AtomicUsize::new(0),
        begin_calls: AtomicUsize::new(0),
    };
    let tail = decode_first_frame(&mut EndedDemuxer, &scheduled);
    let materializer =
        HostPlanarWgpuFrameMaterializer::new(&renderer.device, &renderer.queue, provider.clone());
    assert!(renderer.submit_and_release(&materializer, &provider, tail));
    assert_eq!(scheduled.state_reads.load(Ordering::SeqCst), 2);
    assert_eq!(scheduled.begin_calls.load(Ordering::SeqCst), 1);
    assert_eq!(scheduled.released_tail_frames.load(Ordering::SeqCst), 1);
}
