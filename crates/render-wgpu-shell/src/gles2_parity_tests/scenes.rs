//! Эталонные кадры egui для паритета wgpu ↔ GLES2: один настоящий проход egui на кадр,
//! одинаковые примитивы и изменения текстур уходят в оба рендера.
//!
//! Последовательность закрывает пункты плана сессии 04:
//! 1. окно egui с тенью (плавная альфа → дизеринг), полупрозрачные панели поверх цветного фона,
//!    скругления, текст трёх размеров, иконки `ui-artwork-egui`, отсечённая область;
//! 2. новые глифы (частичная правка атласа шрифта) + своя текстура с линейной фильтрацией;
//! 3. кадр после `free` своей текстуры;
//! 4. повтор UI (`ReusePrevious`) поверх нового фона.

use egui::epaint::ClippedPrimitive;
use egui::{Color32, ColorImage, Context, Pos2, RawInput, Rect, TextureOptions, pos2, vec2};
use ui_artwork_egui::{ArtworkPainter, ButtonVisualState, PlaybackGlyph, PlaybackStyle};

/// Один кадр паритета.
pub(super) struct ParityFrame {
    pub(super) name: &'static str,
    pub(super) pixels_per_point: f32,
    /// Фон (очистка «видео»), RGBA 0..=1.
    pub(super) background: [f32; 4],
    pub(super) corner_radius_points: f32,
    /// Свежий проход egui; `None` — повтор прошлого UI.
    pub(super) ui: Option<(Vec<ClippedPrimitive>, egui::TexturesDelta)>,
}

impl Drop for ParityFrame {
    /// Кадр — шаблон: каждый рендер получает свою копию изменений текстур и применяет её.
    /// Сам шаблон никто не применяет, а egui 0.36 в debug паникует на непустой `TexturesDelta`
    /// при drop — поэтому очищаем явно.
    fn drop(&mut self) {
        if let Some((_, textures_delta)) = &mut self.ui {
            textures_delta.clear();
        }
    }
}

/// Состояние виджетов между кадрами.
struct WidgetState {
    checkbox: bool,
    slider: f32,
}

/// Кадры паритета для цели `size_in_pixels` при масштабе `pixels_per_point`.
pub(super) fn parity_frames(size_in_pixels: [u32; 2], pixels_per_point: f32) -> Vec<ParityFrame> {
    let context = Context::default();
    let size_in_points = vec2(
        size_in_pixels[0] as f32 / pixels_per_point,
        size_in_pixels[1] as f32 / pixels_per_point,
    );
    let mut widgets = WidgetState {
        checkbox: true,
        slider: 0.4,
    };
    let background = [0.25, 0.5, 0.75, 1.0];
    let corner_radius_points = 12.0;

    // Прогон «измерения»: окна egui в первом проходе невидимы (sizing pass). Его изменения
    // текстур (полный атлас) — часть первого кадра, примитивы выбрасываются.
    let (_, mut warm_up_textures) = run_pass(&context, size_in_points, pixels_per_point, |ui| {
        panels_text_icons(ui, &mut widgets)
    });
    let (panels, panels_textures) = run_pass(&context, size_in_points, pixels_per_point, |ui| {
        panels_text_icons(ui, &mut widgets)
    });
    warm_up_textures.append(panels_textures);

    let checker = context.load_texture("parity checker", checker_image(), TextureOptions::LINEAR);
    let (glyphs, glyphs_textures) = run_pass(&context, size_in_points, pixels_per_point, |ui| {
        panels_text_icons(ui, &mut widgets);
        new_glyphs_and_texture(ui, &checker);
    });
    drop(checker);
    let (after_free, after_free_textures) =
        run_pass(&context, size_in_points, pixels_per_point, |ui| {
            panels_text_icons(ui, &mut widgets)
        });

    vec![
        ParityFrame {
            name: "panels, text, icons, clip",
            pixels_per_point,
            background,
            corner_radius_points,
            ui: Some((panels, warm_up_textures)),
        },
        ParityFrame {
            name: "new glyphs (partial atlas update) and a linear user texture",
            pixels_per_point,
            background,
            corner_radius_points,
            ui: Some((glyphs, glyphs_textures)),
        },
        ParityFrame {
            name: "frame after freeing the user texture",
            pixels_per_point,
            background,
            corner_radius_points,
            ui: Some((after_free, after_free_textures)),
        },
        ParityFrame {
            name: "reused UI over a new background",
            pixels_per_point,
            background: [0.1, 0.6, 0.2, 1.0],
            corner_radius_points,
            ui: None,
        },
    ]
}

fn run_pass(
    context: &Context,
    size_in_points: egui::Vec2,
    pixels_per_point: f32,
    paint: impl FnMut(&mut egui::Ui),
) -> (Vec<ClippedPrimitive>, egui::TexturesDelta) {
    context.set_pixels_per_point(pixels_per_point);
    let raw_input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size_in_points)),
        max_texture_side: Some(4096),
        ..Default::default()
    };
    let output = context.run_ui(raw_input, paint);
    let primitives = context.tessellate(output.shapes, output.pixels_per_point);
    (primitives, output.textures_delta)
}

/// Окно с тенью и виджетами, полупрозрачная панель, текст, иконки, отсечённый круг.
fn panels_text_icons(ui: &mut egui::Ui, widgets: &mut WidgetState) {
    let painter = ui.painter().clone();
    painter.rect_filled(
        Rect::from_min_max(pos2(6.0, 6.0), pos2(150.0, 70.0)),
        10.0,
        Color32::from_black_alpha(150),
    );
    for (row, size) in [(0.0, 10.0), (1.0, 14.0), (2.0, 20.0)] {
        painter.text(
            pos2(12.0, 10.0 + row * 18.0),
            egui::Align2::LEFT_TOP,
            "Fastiplayer GLES2",
            egui::FontId::proportional(size),
            Color32::from_rgb(240, 240, 230),
        );
    }
    ArtworkPainter::new(&painter).playback_button(
        Rect::from_min_size(pos2(160.0, 10.0), vec2(36.0, 36.0)),
        PlaybackGlyph::Play,
        ButtonVisualState::Hovered,
        PlaybackStyle {
            diameter: 34.0,
            icon_extent: 14.0,
            stroke_width: 1.5,
            color: Color32::WHITE,
            hover_fill: Color32::from_white_alpha(40),
        },
    );
    ArtworkPainter::new(&painter).volume_slider(
        Rect::from_min_size(pos2(160.0, 52.0), vec2(40.0, 12.0)),
        0.6,
        ButtonVisualState::Idle,
        Color32::from_rgb(90, 160, 255),
        1.5,
    );
    // Отсечённый круг: видна только верхняя половина.
    painter
        .with_clip_rect(Rect::from_min_max(pos2(0.0, 74.0), pos2(200.0, 92.0)))
        .circle_filled(pos2(40.0, 92.0), 18.0, Color32::from_rgb(250, 120, 40));
    egui::Window::new("Settings")
        .fixed_pos(pos2(96.0, 80.0))
        .resizable(false)
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            ui.label("Renderer: OpenGL ES 2.0");
            ui.checkbox(&mut widgets.checkbox, "Rounded corners");
            ui.add(egui::Slider::new(&mut widgets.slider, 0.0..=1.0));
        });
}

/// Новые глифы (кириллица и цифры другого размера) + своя текстура 8×8, растянутая с линейной
/// фильтрацией.
fn new_glyphs_and_texture(ui: &mut egui::Ui, checker: &egui::TextureHandle) {
    let painter = ui.painter();
    painter.text(
        pos2(8.0, 96.0),
        egui::Align2::LEFT_TOP,
        "Жёлтый 0123 ÿ",
        egui::FontId::monospace(17.0),
        Color32::YELLOW,
    );
    painter.image(
        checker.id(),
        Rect::from_min_size(pos2(4.0, 116.0), vec2(48.0, 48.0)),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// Шахматка 8×8 из четырёх цветов с полупрозрачной клеткой (premultiplied через `Color32`).
fn checker_image() -> ColorImage {
    let colors = [
        Color32::from_rgb(255, 0, 0),
        Color32::from_rgb(0, 200, 0),
        Color32::from_rgb(0, 0, 255),
        Color32::from_white_alpha(100),
    ];
    let pixels = (0..64)
        .map(|index| {
            let (x, y) = (index % 8, index / 8);
            colors[(x / 2 + y / 2) % 4]
        })
        .collect();
    ColorImage::new([8, 8], pixels)
}
