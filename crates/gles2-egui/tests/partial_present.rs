//! Частичный показ на настоящем EGL: пиксели частичной перерисовки совпадают с полной.
//!
//! Эталон — второй контекст, который каждый кадр перерисовывает целиком. Тестовый контекст
//! имеет кольцо задних буферов (`offscreen_with_back_buffers`) и сообщает возраст буфера как EGL.
//! Сравнивается КАЖДЫЙ кадр побайтно. Без объединения повреждений по истории (мутация
//! `DamageHistory::plan`) тесты с кольцом из 2+ буферов обязаны падать.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use egui::{Color32, Context, Rect, pos2};
use gles2_egui::{
    ActiveFrame, EguiPainterOptions, FrameCompositor, FrameLayerError, FrameLayers, FrameStart,
    FullRepaintReason, Gles2Context, LayersDamage, PartialPresentStats, PixelExtent, PixelRect,
    PresentKind, RepaintPlan, TargetArea, UiContent, UiFrameOutcome, UiFrameRequest,
};
use glow::HasContext;
use support::{egui_frame, offscreen_or_skip, screen_for, size};

const TARGET: (u32, u32) = (192, 128);
const BACKGROUND: [f32; 4] = [0.1, 0.2, 0.3, 1.0];

/// Что рисуют слои под интерфейсом в кадре (видео и субтитры).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayerScene {
    video_generation: u32,
    subtitle: Option<u32>,
}

/// Что рисует интерфейс в кадре.
#[derive(Debug, Clone, Copy)]
struct UiScene {
    moving_x: u32,
    panel_visible: bool,
    tooltip_visible: bool,
}

#[derive(Debug, Clone, Copy)]
enum UiStep {
    Fresh(UiScene),
    Reuse,
}

#[derive(Debug, Clone, Copy)]
struct Step {
    ui: UiStep,
    layers: LayerScene,
    resize_to: Option<(u32, u32)>,
}

/// Слои, которые честно соблюдают область перерисовки кадра (`clip_to_repaint`).
struct SceneLayers {
    scene: LayerScene,
}

/// Залить прямоугольник цели (сверху слева), отсекая его областью перерисовки кадра.
fn fill_clipped(frame: &ActiveFrame<'_>, rect: PixelRect, rgba: [f32; 4]) {
    let Some(rect) = frame.clip_to_repaint(rect) else {
        return;
    };
    let target_height = i32::try_from(frame.size().height()).unwrap();
    let gl = frame.gl();
    let (x, y) = (
        i32::try_from(rect.x).unwrap(),
        i32::try_from(rect.y).unwrap(),
    );
    let (width, height) = (
        i32::try_from(rect.width).unwrap(),
        i32::try_from(rect.height).unwrap(),
    );
    // SAFETY: контекст кадра текущий; аргументы — константы GLES 2.0 и размеры внутри цели.
    unsafe {
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(x, target_height - (y + height), width, height);
        gl.clear_color(rgba[0], rgba[1], rgba[2], rgba[3]);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.disable(glow::SCISSOR_TEST);
    }
}

impl FrameLayers for SceneLayers {
    /// «Видео»: плитки 16×16 в левых двух третях цели, цвет зависит от плитки и «кадра».
    fn draw_underlay(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        let target = frame.size();
        let video_width = target.width() * 2 / 3;
        for tile_y in 0..target.height().div_ceil(16) {
            for tile_x in 0..video_width.div_ceil(16) {
                let seed = tile_x * 7 + tile_y * 13 + self.scene.video_generation * 29;
                let channel = |salt: u32| ((seed * salt) % 200 + 40) as f32 / 255.0;
                let rect = PixelRect {
                    x: tile_x * 16,
                    y: tile_y * 16,
                    width: 16.min(video_width - tile_x * 16),
                    height: 16.min(target.height() - tile_y * 16),
                };
                fill_clipped(frame, rect, [channel(3), channel(5), channel(11), 1.0]);
            }
        }
        Ok(())
    }

    /// «Субтитры»: белая полоса, положение зависит от номера реплики.
    fn draw_overlays(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        if let Some(line) = self.scene.subtitle {
            let rect = PixelRect {
                x: 6 + (line * 5) % 40,
                y: 30,
                width: 30,
                height: 6,
            };
            fill_clipped(frame, rect, [1.0, 1.0, 1.0, 1.0]);
        }
        Ok(())
    }
}

/// Проход egui для сцены интерфейса.
fn tessellate_scene(
    egui_context: &Context,
    extent: (u32, u32),
    scene: UiScene,
) -> (Vec<egui::epaint::ClippedPrimitive>, egui::TexturesDelta) {
    egui_frame(
        egui_context,
        [extent.0 as f32, extent.1 as f32],
        1.0,
        |ui| {
            let painter = ui.painter();
            if scene.panel_visible {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(0.0, extent.1 as f32 - 14.0),
                        pos2(extent.0 as f32, extent.1 as f32),
                    ),
                    0.0,
                    Color32::from_rgb(40, 40, 48),
                );
            }
            let x = (4 + scene.moving_x) as f32;
            painter.rect_filled(
                Rect::from_min_size(pos2(x, 8.0), egui::vec2(10.0, 10.0)),
                2.0,
                Color32::from_rgb(220, 60, 60),
            );
            if scene.tooltip_visible {
                painter.rect_filled(
                    Rect::from_min_max(pos2(30.0, 40.0), pos2(70.0, 54.0)),
                    3.0,
                    Color32::from_rgba_unmultiplied(250, 250, 200, 230),
                );
                painter.text(
                    pos2(34.0, 42.0),
                    egui::Align2::LEFT_TOP,
                    "tip",
                    egui::FontId::proportional(9.0),
                    Color32::BLACK,
                );
            }
        },
    )
}

/// Сценарий: интерфейс меняется чаще слоёв; есть пауза с наведением, подсказка, панель,
/// смена реплики, смена кадра видео, повторы удержанного UI и изменение размера.
fn scenario() -> Vec<Step> {
    let still = LayerScene {
        video_generation: 0,
        subtitle: None,
    };
    let with_subtitle = LayerScene {
        subtitle: Some(1),
        ..still
    };
    let other_subtitle = LayerScene {
        subtitle: Some(4),
        ..still
    };
    let next_video = LayerScene {
        video_generation: 1,
        subtitle: Some(4),
    };
    let ui = |moving_x: u32, panel_visible: bool, tooltip_visible: bool| {
        UiStep::Fresh(UiScene {
            moving_x,
            panel_visible,
            tooltip_visible,
        })
    };
    let mut steps = Vec::new();
    let mut push = |ui: UiStep, layers: LayerScene| {
        steps.push(Step {
            ui,
            layers,
            resize_to: None,
        });
    };
    // Движущийся прямоугольник при неподвижном видео (пауза + мышь).
    for index in 0..12 {
        push(ui(index * 6, true, false), still);
    }
    push(UiStep::Reuse, still);
    push(UiStep::Reuse, still);
    // Подсказка появляется и исчезает.
    push(ui(48, true, true), still);
    push(ui(48, true, true), still);
    push(ui(48, true, false), still);
    // Панель прячется и возвращается.
    push(ui(48, false, false), still);
    push(ui(54, false, false), still);
    push(ui(54, true, false), still);
    // Смена реплики субтитров, затем UI-кадры.
    push(ui(54, true, false), with_subtitle);
    push(ui(60, true, false), with_subtitle);
    push(ui(60, true, false), other_subtitle);
    push(UiStep::Reuse, other_subtitle);
    push(ui(66, true, true), other_subtitle);
    // Новый кадр видео.
    push(ui(66, true, true), next_video);
    push(ui(72, true, false), next_video);
    push(ui(72, true, false), next_video);
    drop(push);
    // Изменение размера без отметки «слои изменились» — планировщик обязан заметить сам.
    steps.push(Step {
        ui: ui(0, true, false),
        layers: next_video,
        resize_to: Some((160, 112)),
    });
    for index in 0..4 {
        steps.push(Step {
            ui: ui(index * 5, true, index % 2 == 1),
            layers: next_video,
            resize_to: None,
        });
    }
    steps
}

struct Rig {
    context: Gles2Context,
    compositor: FrameCompositor,
}

fn rig_from(context: Gles2Context) -> Rig {
    let compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    Rig {
        context,
        compositor,
    }
}

/// Результат прогона: пиксели каждого кадра, итоги и счётчики.
struct Run {
    frames: Vec<Vec<u8>>,
    plans: Vec<RepaintPlan>,
    kinds: Vec<PresentKind>,
    stats: PartialPresentStats,
}

/// Прогнать сценарий. `partial` — сообщать ли `LayersDamage::Unchanged` для неизменившихся
/// слоёв (тестовый контекст) или всегда перерисовывать целиком (эталон).
fn run_scenario(rig: &mut Rig, partial: bool) -> Run {
    run_steps(rig, partial, scenario())
}

fn run_steps(rig: &mut Rig, partial: bool, steps: Vec<Step>) -> Run {
    let egui_context = Context::default();
    let mut extent = TARGET;
    let mut previous_layers: Option<LayerScene> = None;
    let mut frames = Vec::new();
    let mut plans = Vec::new();
    let mut kinds = Vec::new();
    for step in steps {
        if let Some(new_extent) = step.resize_to {
            extent = new_extent;
            rig.context
                .resize(PixelExtent {
                    width: extent.0,
                    height: extent.1,
                })
                .unwrap();
        }
        let content = match step.ui {
            UiStep::Fresh(scene) => {
                let (primitives, textures_delta) = tessellate_scene(&egui_context, extent, scene);
                UiContent::Fresh {
                    primitives,
                    textures_delta,
                }
            }
            UiStep::Reuse => UiContent::ReusePrevious,
        };
        let layers_damage = if partial && previous_layers == Some(step.layers) {
            LayersDamage::Unchanged
        } else {
            LayersDamage::Changed
        };
        previous_layers = Some(step.layers);
        let mut layers = SceneLayers { scene: step.layers };
        let screen = screen_for(&rig.context, 1.0);
        let outcome = rig
            .compositor
            .render_with_layers_damage(
                &mut rig.context,
                UiFrameRequest {
                    screen,
                    content,
                    background: BACKGROUND,
                    layers: &mut layers,
                },
                layers_damage,
            )
            .unwrap();
        let UiFrameOutcome::Presented(presented) = outcome else {
            panic!("frame must be presented, got {outcome:?}");
        };
        plans.push(presented.repaint);
        kinds.push(presented.present_kind);
        frames.push(
            rig.context
                .read_offscreen_pixels()
                .unwrap()
                .as_rgba()
                .to_vec(),
        );
    }
    Run {
        frames,
        plans,
        kinds,
        stats: rig.compositor.partial_present_stats(),
    }
}

fn assert_same_pixels(actual: &Run, reference: &Run, what: &str) {
    assert_eq!(actual.frames.len(), reference.frames.len());
    for (index, (got, want)) in actual.frames.iter().zip(&reference.frames).enumerate() {
        if got != want {
            let first_difference = got.iter().zip(want).position(|(a, b)| a != b).unwrap();
            panic!(
                "{what}: frame {index} differs from the full repaint at byte {first_difference} \
                 (plan {:?})",
                actual.plans[index]
            );
        }
    }
}

fn reference_run(test_name: &str) -> Option<Run> {
    let context = offscreen_or_skip(test_name, size(TARGET.0, TARGET.1))?;
    let mut rig = rig_from(context);
    let run = run_scenario(&mut rig, false);
    assert_eq!(
        run.stats.partial_presents, 0,
        "reference always repaints fully"
    );
    Some(run)
}

fn partial_run(test_name: &str, back_buffers: usize) -> Option<Run> {
    // Проверка на EGL и пропуск — через общий стенд.
    offscreen_or_skip(test_name, size(TARGET.0, TARGET.1))?;
    let context =
        Gles2Context::offscreen_with_back_buffers(size(TARGET.0, TARGET.1), back_buffers).unwrap();
    let mut rig = rig_from(context);
    Some(run_scenario(&mut rig, true))
}

fn check_ring(back_buffers: usize, test_name: &str) {
    let Some(reference) = reference_run(test_name) else {
        return;
    };
    let Some(run) = partial_run(test_name, back_buffers) else {
        return;
    };
    // 1. Пиксели совпадают с полной перерисовкой в КАЖДОМ кадре.
    assert_same_pixels(&run, &reference, &format!("ring of {back_buffers}"));
    // 2. Трюк реально работает: частичные кадры были и перерисовали заметно меньше цели.
    let stats = run.stats;
    assert!(
        stats.partial_presents >= 3,
        "ring of {back_buffers}: partial presents expected, got {stats:?}"
    );
    assert!(
        stats.repainted_pixels * 10 < stats.target_pixels * 9,
        "ring of {back_buffers}: repainted {} of {} pixels",
        stats.repainted_pixels,
        stats.target_pixels
    );
    // Первые N кадров идут в ещё не показанные буферы, и ещё N раз возраст сбрасывает resize
    // (все буферы переоткрыты): возраст 0.
    assert_eq!(
        stats.full_presents_for(FullRepaintReason::BufferAgeUndefined),
        2 * back_buffers as u64,
        "{stats:?}"
    );
    // Смена слоёв и размера дают полные кадры с правильной причиной.
    assert!(
        stats.full_presents_for(FullRepaintReason::LayersChanged) >= 3,
        "{stats:?}"
    );
    // Кадры «движущийся прямоугольник над неподвижным видео» (шаги 0..12) после прогрева кольца
    // перерисовывают малую часть цели: это и есть выигрыш трюка (без него — вся цель).
    let target_pixels = u64::from(TARGET.0) * u64::from(TARGET.1);
    // Первые N кадров идут без отпечатка интерфейса (возраст 0), следующий строит базу
    // (`UiBaselineUnknown`), потом ещё N−1 кадров видят в истории «всю цель».
    for index in (2 * back_buffers)..12 {
        let RepaintPlan::Partial { repaint, .. } = run.plans[index] else {
            panic!(
                "ring of {back_buffers}: UI-only frame {index} must be partial: {:?}",
                run.plans[index]
            );
        };
        let pixels = repaint.pixels(size(TARGET.0, TARGET.1));
        assert!(
            pixels * 4 < target_pixels,
            "ring of {back_buffers}: frame {index} repainted {pixels} of {target_pixels} pixels"
        );
    }
    assert_eq!(
        stats.full_presents_for(FullRepaintReason::BufferAgeUnsupported),
        0
    );
    // Offscreen никому ничего не сообщает: damage-swap нет.
    assert_eq!(stats.damage_swaps, 0);
    assert!(
        run.kinds
            .iter()
            .all(|kind| *kind == PresentKind::OffscreenFlush)
    );
}

#[test]
fn partial_present_matches_full_repaint_with_one_back_buffer() {
    check_ring(
        1,
        "partial_present_matches_full_repaint_with_one_back_buffer",
    );
}

#[test]
fn partial_present_matches_full_repaint_with_two_back_buffers() {
    check_ring(
        2,
        "partial_present_matches_full_repaint_with_two_back_buffers",
    );
}

#[test]
fn partial_present_matches_full_repaint_with_three_back_buffers() {
    check_ring(
        3,
        "partial_present_matches_full_repaint_with_three_back_buffers",
    );
}

#[test]
fn partial_present_matches_full_repaint_with_four_back_buffers() {
    check_ring(
        4,
        "partial_present_matches_full_repaint_with_four_back_buffers",
    );
}

/// Кольцо глубже истории повреждений: возраст не покрыт — полный кадр, пиксели те же.
#[test]
fn ring_deeper_than_the_damage_history_falls_back_to_full_repaints() {
    let name = "ring_deeper_than_the_damage_history_falls_back_to_full_repaints";
    let Some(reference) = reference_run(name) else {
        return;
    };
    let Some(run) = partial_run(name, 7) else {
        return;
    };
    assert_same_pixels(&run, &reference, "ring of 7");
    assert!(
        run.stats.full_presents_for(FullRepaintReason::BufferTooOld)
            + run
                .stats
                .full_presents_for(FullRepaintReason::BufferAgeUndefined)
            > 0
    );
}

/// Без расширений (обычная offscreen-цель не сообщает возраст) каждый кадр полный, пиксели те же.
#[test]
fn without_buffer_age_every_frame_is_a_full_repaint_with_identical_pixels() {
    let name = "without_buffer_age_every_frame_is_a_full_repaint_with_identical_pixels";
    let Some(reference) = reference_run(name) else {
        return;
    };
    let Some(context) = offscreen_or_skip(name, size(TARGET.0, TARGET.1)) else {
        return;
    };
    let mut rig = rig_from(context);
    let run = run_scenario(&mut rig, true);
    assert_same_pixels(&run, &reference, "no buffer age");
    assert_eq!(run.stats.partial_presents, 0);
    assert_eq!(
        run.stats
            .full_presents_for(FullRepaintReason::BufferAgeUnsupported),
        run.frames.len() as u64
    );
    assert_eq!(run.stats.repainted_pixels, run.stats.target_pixels);
}

/// Кадр, где нарисованное не было показано (ошибка слоя), не ломает следующие кадры.
#[test]
fn abandoned_frame_forces_a_full_repaint_next() {
    let name = "abandoned_frame_forces_a_full_repaint_next";
    let Some(context) = offscreen_or_skip(name, size(TARGET.0, TARGET.1)) else {
        return;
    };
    drop(context);
    let context = Gles2Context::offscreen_with_back_buffers(size(TARGET.0, TARGET.1), 2).unwrap();
    let mut rig = rig_from(context);
    let egui_context = Context::default();
    let scene = UiScene {
        moving_x: 0,
        panel_visible: true,
        tooltip_visible: false,
    };
    let layer_scene = LayerScene {
        video_generation: 0,
        subtitle: None,
    };
    let present = |rig: &mut Rig, layers: &mut dyn FrameLayers| {
        let (primitives, textures_delta) = tessellate_scene(&egui_context, TARGET, scene);
        let screen = screen_for(&rig.context, 1.0);
        rig.compositor.render_with_layers_damage(
            &mut rig.context,
            UiFrameRequest {
                screen,
                content: UiContent::Fresh {
                    primitives,
                    textures_delta,
                },
                background: BACKGROUND,
                layers,
            },
            LayersDamage::Unchanged,
        )
    };
    let mut good = SceneLayers { scene: layer_scene };
    for _ in 0..3 {
        present(&mut rig, &mut good).unwrap();
    }
    struct FailingLayers;
    impl FrameLayers for FailingLayers {
        fn draw_underlay(&mut self, _frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
            Err(FrameLayerError {
                message: "broken".to_owned(),
            })
        }
    }
    assert!(present(&mut rig, &mut FailingLayers).is_err());
    let UiFrameOutcome::Presented(next) = present(&mut rig, &mut good).unwrap() else {
        panic!("next frame must be presented");
    };
    assert_eq!(
        next.repaint,
        RepaintPlan::Full(FullRepaintReason::PreviousFrameAbandoned)
    );
}

/// Заливка кадра и художник egui, ограниченные областью, не трогают пиксели вне неё.
#[test]
fn restricted_frame_clear_and_egui_paint_stay_inside_the_region() {
    let name = "restricted_frame_clear_and_egui_paint_stay_inside_the_region";
    let Some(mut context) = offscreen_or_skip(name, size(64, 64)) else {
        return;
    };
    let mut painter =
        gles2_egui::EguiPainter::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [64.0, 64.0], 1.0, |ui| {
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(64.0, 64.0)),
            0.0,
            Color32::WHITE,
        );
    });
    painter.apply_textures(&context, &textures_delta).unwrap();
    let screen = screen_for(&context, 1.0);
    let region = PixelRect {
        x: 16,
        y: 24,
        width: 20,
        height: 12,
    };
    let FrameStart::Ready(mut frame) = context.begin_frame().unwrap() else {
        panic!("frame must start");
    };
    // Вся цель красная, затем кадр ограничен областью: заливка и egui видят только её.
    frame.clear([1.0, 0.0, 0.0, 1.0]);
    frame.restrict_to_partial_repaint(TargetArea::Region(region), TargetArea::Region(region));
    frame.clear([0.0, 0.0, 1.0, 1.0]);
    painter.prepare_fresh(&frame, screen, primitives).unwrap();
    let work = painter.paint_retained(&frame, screen).unwrap();
    frame.present().unwrap();
    painter.free_textures(&context, textures_delta).unwrap();
    assert!(work.draw_calls > 0, "the mesh intersects the region");
    let pixels = context.read_offscreen_pixels().unwrap();
    for y in 0..64 {
        for x in 0..64 {
            let inside = x >= region.x
                && x < region.x + region.width
                && y >= region.y
                && y < region.y + region.height;
            let [red, green, blue, _] = pixels.pixel(x, y).unwrap();
            if inside {
                assert!(
                    red > 240 && green > 240 && blue > 240,
                    "({x},{y}) inside the region must be painted by egui: {red},{green},{blue}"
                );
            } else {
                assert_eq!(
                    [red, green, blue],
                    [255, 0, 0],
                    "({x},{y}) outside the region must keep the old pixels"
                );
            }
        }
    }
    painter.destroy(&context).unwrap();
}

/// Область «ничего»: ни заливка, ни egui ничего не рисуют.
#[test]
fn nothing_area_draws_no_commands() {
    let name = "nothing_area_draws_no_commands";
    let Some(mut context) = offscreen_or_skip(name, size(32, 32)) else {
        return;
    };
    let mut painter =
        gles2_egui::EguiPainter::new(&context, EguiPainterOptions::default()).unwrap();
    let egui_context = Context::default();
    let (primitives, textures_delta) = egui_frame(&egui_context, [32.0, 32.0], 1.0, |ui| {
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(32.0, 32.0)),
            0.0,
            Color32::WHITE,
        );
    });
    painter.apply_textures(&context, &textures_delta).unwrap();
    let screen = screen_for(&context, 1.0);
    let FrameStart::Ready(mut frame) = context.begin_frame().unwrap() else {
        panic!("frame must start");
    };
    frame.clear([1.0, 0.0, 0.0, 1.0]);
    frame.restrict_to_partial_repaint(TargetArea::Nothing, TargetArea::Nothing);
    frame.clear([0.0, 0.0, 1.0, 1.0]);
    painter.prepare_fresh(&frame, screen, primitives).unwrap();
    let work = painter.paint_retained(&frame, screen).unwrap();
    frame.present().unwrap();
    painter.free_textures(&context, textures_delta).unwrap();
    assert_eq!(work.draw_calls, 0);
    assert!(work.skipped_outside_repaint > 0);
    let pixels = context.read_offscreen_pixels().unwrap();
    assert!(
        pixels
            .as_rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3] == [255, 0, 0])
    );
    painter.destroy(&context).unwrap();
}

/// Воспроизведение (каждый кадр видео новый, интерфейс свежий), затем пауза: слои стоят,
/// интерфейс меняется (мышь, подсказка).
fn playback_then_pause(playback_frames: u32, pause_frames: u32) -> Vec<Step> {
    let mut steps = Vec::new();
    for generation in 0..playback_frames {
        steps.push(Step {
            ui: UiStep::Fresh(UiScene {
                moving_x: generation % 5,
                panel_visible: true,
                tooltip_visible: false,
            }),
            layers: LayerScene {
                video_generation: generation,
                subtitle: None,
            },
            resize_to: None,
        });
    }
    let paused = LayerScene {
        video_generation: playback_frames.saturating_sub(1),
        subtitle: None,
    };
    for index in 0..pause_frames {
        steps.push(Step {
            ui: UiStep::Fresh(UiScene {
                moving_x: index * 7,
                panel_visible: true,
                tooltip_visible: index % 4 == 3,
            }),
            layers: paused,
            resize_to: None,
        });
    }
    steps
}

fn check_playback_then_pause(back_buffers: usize, name: &str) {
    let Some(reference_context) = offscreen_or_skip(name, size(TARGET.0, TARGET.1)) else {
        return;
    };
    let mut reference_rig = rig_from(reference_context);
    let steps = playback_then_pause(10, 14);
    let reference = run_steps(&mut reference_rig, false, steps.clone());
    let context =
        Gles2Context::offscreen_with_back_buffers(size(TARGET.0, TARGET.1), back_buffers).unwrap();
    let mut rig = rig_from(context);
    let run = run_steps(&mut rig, true, steps);
    assert_same_pixels(&run, &reference, &format!("ring of {back_buffers}"));
    // Воспроизведение (слои меняются каждый кадр) не строит отпечаток интерфейса вовсе; он
    // появляется на одном кадре базы после паузы и дальше на частичных кадрах.
    let computed = rig.compositor.painter().stats().ui_footprints_computed;
    let pause_frames = 14;
    assert!(
        computed <= pause_frames,
        "ring of {back_buffers}: footprints computed {computed}"
    );
    let baseline_frames = run
        .plans
        .iter()
        .filter(|plan| **plan == RepaintPlan::Full(FullRepaintReason::UiBaselineUnknown))
        .count();
    assert_eq!(
        baseline_frames, 1,
        "ring of {back_buffers}: {:?}",
        run.plans
    );
    assert!(
        run.stats.partial_presents >= 3,
        "partial presents must resume after the baseline frame: {:?}",
        run.stats
    );
    let last = run.plans.last().copied().unwrap();
    assert!(matches!(last, RepaintPlan::Partial { .. }), "{last:?}");
}

#[test]
fn playback_then_pause_is_pixel_exact_with_one_back_buffer() {
    check_playback_then_pause(1, "playback_then_pause_one");
}

#[test]
fn playback_then_pause_is_pixel_exact_with_two_back_buffers() {
    check_playback_then_pause(2, "playback_then_pause_two");
}

#[test]
fn playback_then_pause_is_pixel_exact_with_three_back_buffers() {
    check_playback_then_pause(3, "playback_then_pause_three");
}

/// Корень регрессии: кадры, которые заведомо полные (слои меняются, возраст неизвестен), не
/// считают отпечаток интерфейса.
#[test]
fn frames_that_are_full_anyway_compute_no_ui_footprint() {
    let name = "frames_that_are_full_anyway_compute_no_ui_footprint";
    let Some(plain_context) = offscreen_or_skip(name, size(TARGET.0, TARGET.1)) else {
        return;
    };
    // Возраст неизвестен: ни один кадр не окупает отпечаток, даже при неизменных слоях.
    let mut plain = rig_from(plain_context);
    run_steps(&mut plain, true, playback_then_pause(4, 6));
    assert_eq!(plain.compositor.painter().stats().ui_footprints_computed, 0);

    // Возраст есть, но слои менялись каждый кадр: отпечатков нет.
    let ring = Gles2Context::offscreen_with_back_buffers(size(TARGET.0, TARGET.1), 2).unwrap();
    let mut rig = rig_from(ring);
    let run = run_steps(&mut rig, true, playback_then_pause(12, 0));
    assert_eq!(rig.compositor.painter().stats().ui_footprints_computed, 0);
    assert_eq!(run.stats.partial_presents, 0);
}
