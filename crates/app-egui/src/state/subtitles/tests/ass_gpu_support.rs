//! Общие помощники GPU-тестов ASS слоя 1: настоящий демукс MKV (или файл рядом) -> владелец ->
//! размещение -> настоящий GPU-проход -> readback; поиск пикселей по цвету.

use player_core::SubtitleFeedMessage;

use super::embedded_support::*;
use super::gpu_chain::Harness;
use super::*;
use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;

/// Размер «окна» тестов: 16:9, равно PlayRes фикстуры (640x360): масштаб 1, текст достаточно крупный для сплошных пикселей.
pub(super) const WINDOW_WIDTH: u32 = 640;
pub(super) const WINDOW_HEIGHT: u32 = 360;
/// Момент, когда активны все события фикстуры `ass-layer1.mkv` ([1.0 с; 3.0 с)).
pub(super) const INSIDE_EVENTS_MILLIS: u64 = 1_500;
/// Момент без реплик.
pub(super) const OUTSIDE_EVENTS_MILLIS: u64 = 4_000;

/// Прямоугольник пикселей, включительно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PixelBounds {
    pub(super) min_x: u32,
    pub(super) min_y: u32,
    pub(super) max_x: u32,
    pub(super) max_y: u32,
}

impl PixelBounds {
    pub(super) fn center_x(&self) -> f32 {
        (self.min_x + self.max_x) as f32 / 2.0
    }
    pub(super) fn center_y(&self) -> f32 {
        (self.min_y + self.max_y) as f32 / 2.0
    }
    pub(super) fn height(&self) -> u32 {
        self.max_y - self.min_y + 1
    }
}

/// Чистый красный (знак фикстуры; на видео-фоне такого цвета нет).
pub(super) fn is_red(pixel: [u8; 4]) -> bool {
    pixel[0] >= 200 && pixel[1] <= 40 && pixel[2] <= 40
}

/// Белый (первая нижняя реплика).
pub(super) fn is_white(pixel: [u8; 4]) -> bool {
    pixel[0] >= 235 && pixel[1] >= 235 && pixel[2] >= 235
}

/// Жёлтый (вторая нижняя реплика).
pub(super) fn is_yellow(pixel: [u8; 4]) -> bool {
    pixel[0] >= 235 && pixel[1] >= 235 && pixel[2] <= 40
}

/// Цвет пользователя «зелёный» (`#00FF00`), которого нет ни в видео, ни в сценарии.
pub(super) fn is_green(pixel: [u8; 4]) -> bool {
    pixel[0] <= 40 && pixel[1] >= 235 && pixel[2] <= 40
}

/// Границы пикселей кадра, подходящих под `predicate`.
pub(super) fn bounds_of(
    pixels: &[u8],
    width: u32,
    predicate: impl Fn([u8; 4]) -> bool,
) -> Option<PixelBounds> {
    let mut bounds: Option<PixelBounds> = None;
    for (index, chunk) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if !predicate(*chunk) {
            continue;
        }
        let (x, y) = (index as u32 % width, index as u32 / width);
        bounds = Some(match bounds {
            None => PixelBounds {
                min_x: x,
                min_y: y,
                max_x: x,
                max_y: y,
            },
            Some(old) => PixelBounds {
                min_x: old.min_x.min(x),
                min_y: old.min_y.min(y),
                max_x: old.max_x.max(x),
                max_y: old.max_y.max(y),
            },
        });
    }
    bounds
}

/// Сколько пикселей кадра подходят под `predicate`.
pub(super) fn count_of(pixels: &[u8], predicate: impl Fn([u8; 4]) -> bool) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| predicate(**p))
        .count()
}

/// Кадр окна по картинке `rect` на всю область видео.
pub(super) fn window_frame(
    rect: render_core::PictureRect,
    pts_millis: u64,
) -> SubmittedFrameForSubtitles {
    SubmittedFrameForSubtitles::for_test_with_viewport(
        Duration::from_millis(pts_millis),
        GENERATION,
        rect,
        RenderViewport::full_surface(WINDOW_WIDTH, WINDOW_HEIGHT),
    )
}

/// Кадр окна: размещение владельцем -> GPU -> пиксели. `None`, если на кадре слоя нет.
pub(super) fn render_placed(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    rect: render_core::PictureRect,
    pts_millis: u64,
) -> Vec<u8> {
    let frame = window_frame(rect, pts_millis);
    let placed = owner.placed_overlay_for_frame(current(media, GENERATION), &frame);
    let Some(placed) = placed else {
        return harness.frame(rect, None).1;
    };
    let (overlay, offsets) = (placed.overlay.clone(), placed.layer_offsets);
    harness.frame_with_offsets(rect, Some(&overlay), offsets).1
}

/// Картинка 16:9 на всё окно.
pub(super) fn full_window_picture() -> render_core::PictureRect {
    full_rect(WINDOW_WIDTH, WINDOW_HEIGHT)
}

/// Картинка 2.39:1 с чёрными полосами сверху и снизу.
pub(super) fn letterboxed_window_picture() -> render_core::PictureRect {
    letterboxed_rect(WINDOW_WIDTH, WINDOW_HEIGHT)
}

/// Настоящий порядок трубы: `MediaStarted` с заголовком, затем пакеты.
pub(super) fn feed_in_real_order(
    fixture: &DemuxedFixture,
    media: MediaInstanceId,
) -> Vec<SubtitleFeedMessage> {
    let mut messages = vec![fixture.media_started(media)];
    messages.extend(fixture.feed_messages(media));
    messages
}

/// Владелец с дорожкой фикстуры `ass-layer1.mkv`, прошедшей весь путь demux -> feed.
pub(super) fn layer1_owner(config: &SubtitlesConfig, media: MediaInstanceId) -> SubtitleOwner {
    let fixture = demux_fixture("ass-layer1.mkv");
    let mut owner = embedded_owner(config, media, &fixture);
    owner.deliver_feed_messages(feed_in_real_order(&fixture, media));
    owner
}

/// Панель управления, закрывающая нижнюю часть окна от `top_px`.
pub(super) fn controls_from(top_px: i32) -> BottomControlsObstruction {
    BottomControlsObstruction::TopEdge {
        top_px,
        clearance_px: 8,
    }
}

/// Любой пиксель строки `y` отличается от чёрной полосы?
pub(super) fn row_has_non_black(pixels: &[u8], y: u32) -> bool {
    let start = (y * WINDOW_WIDTH * 4) as usize;
    pixels[start..start + (WINDOW_WIDTH * 4) as usize]
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[..3] != [0, 0, 0])
}
