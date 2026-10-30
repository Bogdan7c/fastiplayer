// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! iPhone HEVC MOV, у которого в `stsd` после детей `hvc1` лежит QuickTime-терминатор
//! (4 нулевых байта), открывается и отдаёт первый видеопакет. Нужен локальный ассет
//! из `test-assets/` (в репозиторий не входит), поэтому тест `#[ignore]`.

use std::path::Path;

use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

#[test]
#[ignore = "needs local test-assets/h265/4k60fps/IMG_0002 (1).mov"]
fn iphone_hevc_mov_with_quicktime_terminator_yields_first_video_packet() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-assets/h265/4k60fps/IMG_0002 (1).mov");
    let mut demuxer = SymphoniaDemuxer::from_file(&path).expect("iPhone MOV must open");
    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("MOV has a video track")
        .clone();
    assert!(
        video_track.codec_private.is_some(),
        "hvcC must reach the track"
    );
    loop {
        match demuxer.next_event().expect("read event") {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track.id =>
            {
                assert!(!packet.data.is_empty(), "first video packet carries data");
                return;
            }
            DemuxReadEvent::EndOfStream => panic!("MOV ended without a video packet"),
            _ => {}
        }
    }
}
