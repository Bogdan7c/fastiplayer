//! `\t` через настоящий поток-рисовальщик: шторка `\t(\clip(...))` идёт кадр за кадром (каждый
//! кадр получает картинку СВОЕГО времени), реплика растеризуется один раз, а законченная
//! анимация больше не перерисовывается.

use subtitle_raster::SubtitleOverlay;

use super::super::SubtitleOwner;
use super::ass_animation_support::*;
use super::render_thread::{FRAME_MS, GENERATION, HEIGHT, WIDTH, free_running};
use super::*;

/// Шторка в 4 секунды: правый край клипа идёт 0 -> 1280 px, то есть `0.32 px/мс` или
/// `12.8 px` за кадр. Текст `WIPE` (кегль 150) стоит на 100..~350 px, так что край клипа
/// проходит по буквам с 0.1 с до 1.1 с.
const WIPE_SCRIPT_TEXT: &str =
    "{\\an7\\pos(100,100)\\fs150\\clip(0,0,0,720)\\t(0,4000,\\clip(0,0,1280,720))}WIPE";

/// Быстрая шторка, которая заканчивается на 0.5 с: дальше сцена статична.
const SHORT_WIPE_TEXT: &str =
    "{\\an7\\pos(100,100)\\fs150\\clip(0,0,0,720)\\t(0,500,\\clip(0,0,1280,720))}WIPE";

fn script_of(text: &str) -> String {
    animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[(
            "0:00:00.00".to_owned(),
            "0:01:00.00".to_owned(),
            text.to_owned(),
        )],
    )
}

/// Правый край картинок реплики в overlay (`None` — картинок нет).
fn right_edge(overlay: &SubtitleOverlay) -> Option<i32> {
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| bitmap.origin().x + i32::try_from(bitmap.width()).unwrap_or(i32::MAX))
        .max()
}

/// Один кадр: выбор картинки как в UI.
fn overlay_at(owner: &mut SubtitleOwner, media: MediaInstanceId, pts_ms: u64) -> Option<i32> {
    let rect = full_rect(WIDTH, HEIGHT);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .and_then(right_edge)
}

#[test]
fn clip_wipe_plays_frame_by_frame_with_a_single_rasterization() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &script_of(WIPE_SCRIPT_TEXT));
    free_running(&mut owner);

    let first_ms = 100_u64;
    let warm_up_frames = 8_u64;
    for index in 0..warm_up_frames {
        overlay_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let stats_before = owner.presentation_stats();
    let counters_before = owner.presenter.queue_counters();

    let frames = 16_u64;
    let mut previous: Option<i32> = None;
    for index in warm_up_frames..warm_up_frames + frames {
        let pts_ms = first_ms + index * FRAME_MS;
        let right = overlay_at(&mut owner, media, pts_ms).expect("the wipe shows letters");
        assert!(owner.presenter.wait_for_idle());
        // Край клипа в этот момент: 1280 * t / 4000 (клип усекается до целого).
        let edge = (1280.0 * pts_ms as f64 / 4000.0) as i32;
        assert!(
            right <= edge,
            "frame at {pts_ms} ms: picture reaches {right}, past its own clip edge {edge}"
        );
        assert!(
            edge - right <= 40,
            "frame at {pts_ms} ms: picture ends at {right}, far behind its own clip edge {edge}"
        );
        if let Some(previous_right) = previous {
            assert!(
                right > previous_right,
                "frame at {pts_ms} ms shows the same picture as the frame before: {right}"
            );
        }
        previous = Some(right);
    }

    // Каждый кадр отдан точной картинкой своего времени, опозданий нет.
    let stats_after = owner.presentation_stats();
    let counters_after = owner.presenter.queue_counters();
    assert_eq!(stats_after.late_pictures, stats_before.late_pictures);
    assert_eq!(
        counters_after.stale_fallbacks,
        counters_before.stale_fallbacks
    );
    assert_eq!(
        counters_after.exact_hits - counters_before.exact_hits,
        frames
    );

    // Картинка на каждый кадр своя, а буквы растеризованы ровно один раз.
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        diagnostics.raster.rasterized_cues, 1,
        "the wipe moves the clip, the letters stay in the cue cache"
    );
    assert!(
        diagnostics.raster.rendered_frames >= warm_up_frames + frames,
        "a picture per frame, drew {}",
        diagnostics.raster.rendered_frames
    );
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);
}

#[test]
fn finished_clip_wipe_stops_being_redrawn() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &script_of(SHORT_WIPE_TEXT));
    free_running(&mut owner);

    // Шторка закончилась на 0.5 с; кадры с 2 с — статичная сцена.
    let first_ms = 2_000_u64;
    for index in 0..12_u64 {
        overlay_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let settled = owner.presenter.worker_diagnostics().expect("worker");

    let mut rights = Vec::new();
    for index in 12..62_u64 {
        rights.push(overlay_at(&mut owner, media, first_ms + index * FRAME_MS));
        assert!(owner.presenter.wait_for_idle());
    }
    // Клип раскрыт полностью: буквы целы и не меняются от кадра к кадру.
    assert!(rights.iter().all(|right| *right == rights[0]));
    assert!(rights[0].is_some());

    let later = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        later.raster.rendered_frames, settled.raster.rendered_frames,
        "a finished animation is a static scene: nothing is drawn any more"
    );
    assert_eq!(later.raster.rasterized_cues, 1);
}

/// Непрозрачность самой плотной картинки реплики в overlay (`None` — картинок нет).
fn peak_opacity(overlay: &SubtitleOverlay) -> Option<u8> {
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| bitmap.color().alpha)
        .max()
}

/// Одна картинка кадра: непрозрачность цвета, положение и размер.
type BitmapSignature = (u8, i32, i32, u32, u32);

/// Что показал кадр: пик непрозрачности и снимок каждой картинки; `None` — overlay не было.
type ShownFrame = (u64, Option<(Option<u8>, Vec<BitmapSignature>)>);

/// Снимок картинки реплики: цвет, положение и размер каждой картинки (для сравнения кадров).
fn picture_signature(overlay: &SubtitleOverlay) -> Vec<BitmapSignature> {
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| {
            (
                bitmap.color().alpha,
                bitmap.origin().x,
                bitmap.origin().y,
                bitmap.width(),
                bitmap.height(),
            )
        })
        .collect()
}

/// Проигрывает кадры подряд (как видео) и возвращает снимок картинки каждого кадра.
/// Первые `warm_up_frames` кадров поток догоняет часы и в результат не входят.
fn play_frames(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    first_ms: u64,
    frames: u64,
    warm_up_frames: u64,
) -> Vec<ShownFrame> {
    let rect = full_rect(WIDTH, HEIGHT);
    let mut shown = Vec::new();
    for index in 0..frames {
        let pts_ms = first_ms + index * FRAME_MS;
        let picture = owner
            .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
            .map(|overlay| (peak_opacity(overlay), picture_signature(overlay)));
        assert!(owner.presenter.wait_for_idle());
        if index >= warm_up_frames {
            shown.push((pts_ms, picture));
        }
    }
    shown
}

/// Непрозрачность кадра с заданным pts (`None` — кадр без картинок).
fn opacity_shown(shown: &[ShownFrame], pts_ms: u64) -> Option<u8> {
    let (_, picture) = shown
        .iter()
        .find(|(frame_pts, _)| *frame_pts == pts_ms)
        .unwrap_or_else(|| panic!("кадра {pts_ms} мс нет в проигрывании"));
    picture.as_ref().and_then(|(opacity, _)| *opacity)
}

/// `\t(500,1500,0,\alpha&H00&)`: `pow(0, 0) = 1`, так что на самом `t1 = 500` мс вес уже полный.
/// До `t1` текст невидим (`\alpha&HFF&`), с `t1` и дальше кадры одинаковые.
#[test]
fn zero_accel_transform_is_at_full_weight_exactly_at_t1_through_the_render_thread() {
    let media = instance(1);
    let text = "{\\an7\\pos(100,100)\\fs150\\alpha&HFF&\\t(500,1500,0,\\alpha&H00&)}WIPE";
    let (mut owner, _dir) = owner_with_script(media, &script_of(text));
    free_running(&mut owner);

    // Кадры 220, 260, ..., 1020 мс: есть и 460 (до t1), и 500 (ровно t1), и 540 (после).
    let shown = play_frames(&mut owner, media, 220, 21, 4);
    assert_eq!(opacity_shown(&shown, 460), None, "до t1 текст невидим");
    assert_eq!(
        opacity_shown(&shown, 500),
        Some(255),
        "на t1 при accel 0 вес уже 1: текст непрозрачен"
    );
    let at_t1 = shown.iter().find(|(pts, _)| *pts == 500).map(|(_, p)| p);
    for (pts_ms, picture) in shown.iter().filter(|(pts, _)| *pts > 500) {
        assert_eq!(
            Some(picture),
            at_t1,
            "кадр {pts_ms} мс после t1 не должен отличаться от кадра на t1"
        );
    }
}

/// `\t(500,1500,-1,\alpha&HFF&)` от `\alpha&H80&`: на самом `t1` вес `pow(0, -1) = inf`, смесь
/// `(1 - inf) * 128 + 255 * inf` даёт NaN, `dtoi32(NaN) = INT32_MIN`, младший байт 0:
/// прозрачность 0 (полная непрозрачность). Это то же, что делает libass (эталон
/// `ass_t_accel_edge`). Следующий кадр (вес 25) даёт 0xE7, то есть непрозрачность 24; после
/// `t2` вес 1 и прозрачность `0xFF`: текста нет. Картинка не залипает на значении `t1`.
#[test]
fn negative_accel_transform_follows_libass_arithmetic_at_t1_and_after_through_the_render_thread() {
    let media = instance(1);
    let text = "{\\an7\\pos(100,100)\\fs150\\alpha&H80&\\t(500,1500,-1,\\alpha&HFF&)}WIPE";
    let (mut owner, _dir) = owner_with_script(media, &script_of(text));
    free_running(&mut owner);

    // Кадры 220 .. 1620 мс шагом 40: включают 460, 500, 540 и кадры после t2 = 1500.
    let shown = play_frames(&mut owner, media, 220, 36, 4);
    assert_eq!(
        opacity_shown(&shown, 460),
        Some(127),
        "до t1 прозрачность 0x80"
    );
    assert_eq!(opacity_shown(&shown, 500), Some(255), "t1: NaN -> байт 0");
    assert_eq!(opacity_shown(&shown, 540), Some(24), "вес 25: байт 0xE7");
    assert_eq!(
        opacity_shown(&shown, 1540),
        None,
        "после t2 прозрачность 0xFF"
    );
    assert_eq!(opacity_shown(&shown, 1580), None);
}
