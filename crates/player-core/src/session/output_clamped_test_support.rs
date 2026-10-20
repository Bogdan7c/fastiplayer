//! Test-only сборка установившегося воспроизведения, зажатого очередью показа.
//!
//! Worker-level тесты не видят `PlaybackPipeline`, поэтому состояние
//! «Playing, очередь показа набрана, следующий packet ждёт слота» собирается
//! здесь, у владельца session state.

use std::time::Duration;

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use media_core::TrackId;
use video_core::VideoDecoderThreadHandle;

use super::PlayerSession;
use crate::decoder_boundary::PresentFrameResourceProviderHandle;
use crate::pipeline::PendingVideoPacket;

impl PlayerSession {
    /// Переводит session в Playing с выбранным video track-ом, кадрами `queued_frames`
    /// в очереди показа и одним pending packet-ом с PTS `pending_packet_pts`.
    pub(crate) fn install_output_clamped_playback_for_tests(
        &mut self,
        decoder_thread: impl VideoDecoderThreadHandle<
            ResourceProvider = PresentFrameResourceProviderHandle,
        > + 'static,
        queued_frames: Vec<video_core::DecodedFrame>,
        pending_packet_pts: Duration,
    ) {
        self.begin_playback_without_media_for_test()
            .expect("test session without media must start playback");
        self.update_current_position(Duration::ZERO);
        self.pipeline.set_video_decoder_thread(decoder_thread);
        self.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        for frame in queued_frames {
            self.pipeline.enqueue_queued_video_frame(frame);
        }
        self.pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                pending_packet_pts,
                self.pipeline.seek_generation(),
                Bytes::from_static(b"packet-waiting-for-present-slot"),
                // keyframe: не-ключевой packet без bootstrap-а tick просто списал бы.
                true,
            ));
    }
}
