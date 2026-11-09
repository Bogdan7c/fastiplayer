//! Общая заготовка тестов меню качества: снимок плеера с кадром и готовый вид меню.

use std::time::Duration;

use media_core::{TrackId, TrackKind, TrackLabels};
use player_core::{PlayerSnapshot, TrackSummarySnapshot, VideoFrameSnapshot};
use video_core::{DecodedPixelFormat, FrameMemoryPath};

use super::QualityMenuView;
use crate::page_quality::{PageQualityFacts, PageViewOffer};

/// Снимок плеера с кадром `render` и видеодорожкой VP9 (60 кадров/с).
pub(crate) fn snapshot_with_frame(render: (u32, u32)) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.current_video_frame = Some(VideoFrameSnapshot {
        render_generation: 1,
        handle: 1,
        pts: Duration::ZERO,
        // Декодированная поверхность выровнена по блокам: она НЕ то, что видит зритель.
        width: render.0,
        height: render.1 + 8,
        render_width: render.0,
        render_height: render.1,
        format: DecodedPixelFormat::Nv12,
        memory_path: FrameMemoryPath::CpuUpload,
    });
    snapshot.selected_tracks.video_track = Some(TrackId::new(1));
    snapshot.tracks = vec![TrackSummarySnapshot {
        id: TrackId::new(1),
        kind: TrackKind::Video,
        codec_id: "V_VP9".to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
        labels: TrackLabels::default(),
    }];
    snapshot.video_frame_duration_estimate = Duration::from_micros(16_683);
    snapshot
}

/// Вид меню для кадра `render` без битрейта и с заданным предложением страницы.
pub(crate) fn sample_quality_view(render: (u32, u32), page_view: PageViewOffer) -> QualityMenuView {
    QualityMenuView::from_snapshot(
        &snapshot_with_frame(render),
        &PageQualityFacts {
            bitrate: None,
            page_view,
        },
    )
}
