// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

mod support;

use anyhow::{Result, ensure};
use media_core::Demuxer;
use support::manual_media::{report_selected_media, selected_media_path};
use symphonia_demux::SymphoniaDemuxer;

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn selected_media_is_openable_and_reports_detected_tracks() -> Result<()> {
    let path = selected_media_path()?;
    let scenario = std::env::var("FASTIPLAYER_MEDIA_SCENARIO")
        .unwrap_or_else(|_| "manual-inspection".to_string());
    let demuxer = SymphoniaDemuxer::from_file(&path)?;
    ensure!(
        !demuxer.tracks().is_empty(),
        "selected media has no public tracks: {}",
        path.display()
    );
    report_selected_media(&scenario, &path, demuxer.tracks())
}
