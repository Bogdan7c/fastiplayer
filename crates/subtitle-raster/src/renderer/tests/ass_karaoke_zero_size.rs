//! Караоке и буквы нулевого размера (стиль `Fontsize: 0`): каждый символ реплики в libass
//! создаёт глиф, который забирает накопленные теги `\k*`/`\kt` (`parse_events`), а нулевые
//! буквы мы не рисуем (решение 10a). Невидимую букву надо вести как символ без ширины:
//! здесь она сравнивается с такой же буквой нулевой ширины (`\fscx0`) обычного размера,
//! которая проходит весь путь libass и закреплена эталонами.

use subtitle_core::SubtitleTime;

use super::ass::fresh_source;
use super::*;
use crate::ass::AssRenderOptions;
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, load_ass_cues};

const ZERO_STYLE_LINE: &str = "Style: Zero,Noto Sans,0,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,3,0,2,40,40,40,1";
/// Размер букв, которые видны.
const VISIBLE: &str = r"\fs60";

fn cues_in_style(style: &str, text: &str) -> Vec<AssTestCue> {
    let line = format!("Dialogue: 0,0:00:00.00,0:00:10.00,{style},,0,0,0,,{text}");
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE, ZERO_STYLE_LINE],
        &[&line],
    );
    load_ass_cues(&file, fresh_source())
}

/// Картинки реплики на момент: вид, цвет, положение и размер каждой.
fn pictures_at(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    moment_ms: i64,
) -> Vec<String> {
    let list: Vec<RenderableCue<'_>> = cues.iter().map(AssTestCue::renderable).collect();
    let appearance = SubtitleAppearance::youtube_like();
    let overlay = rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::from_millis(moment_ms),
            cues: &list,
            canvas: canvas(),
            appearance: &appearance,
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone();
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| {
            format!(
                "{:?} {:?} ({}, {}) {}x{}",
                bitmap.kind(),
                bitmap.color(),
                bitmap.origin().x,
                bitmap.origin().y,
                bitmap.width(),
                bitmap.height()
            )
        })
        .collect()
}

/// Сценарии: текст для стиля `Zero` (невидимая буква в стиле нулевого размера) и тот же текст
/// для стиля `Default`, где невидимая буква сделана нулевой шириной. Видимая часть у обоих
/// одинакова.
fn scenarios() -> Vec<(&'static str, String, String)> {
    let hide = r"\fscx0";
    let show = r"\fscx100";
    vec![
        (
            "слово продолжается видимыми буквами",
            format!(r"{{\an7\pos(100,100)\kf100}}A{{{VISIBLE}}}BCD"),
            format!(r"{{\an7\pos(100,100){VISIBLE}\kf100{hide}}}A{{{show}}}BCD"),
        ),
        (
            "следующий слог получает пропуск",
            format!(r"{{\an7\pos(100,100)\k30}}A{{{VISIBLE}\kf60}}BC{{\kf50}}D"),
            format!(r"{{\an7\pos(100,100){VISIBLE}\k30{hide}}}A{{{show}\kf60}}BC{{\kf50}}D"),
        ),
        (
            "слог внутри уже идущего слова",
            format!(r"{{\an7\pos(100,100){VISIBLE}\k50}}X{{\fs0\k30}}A{{{VISIBLE}}}BCD"),
            format!(r"{{\an7\pos(100,100){VISIBLE}\k50}}X{{\k30{hide}}}A{{{show}}}BCD"),
        ),
        (
            "\\kt после невидимой буквы",
            format!(r"{{\an7\pos(100,100)\kf50}}A{{\kt200{VISIBLE}}}BCD"),
            format!(r"{{\an7\pos(100,100){VISIBLE}\kf50{hide}}}A{{\kt200{show}}}BCD"),
        ),
    ]
}

#[test]
fn zero_size_letters_pass_their_karaoke_tags_like_zero_width_letters() {
    for (name, zero_text, width_text) in scenarios() {
        let zero_cues = cues_in_style("Zero", &zero_text);
        let width_cues = cues_in_style("Default", &width_text);
        for moment in [100_i64, 400, 700, 1200, 2500] {
            let mut zero_rasterizer = rasterizer(ROOMY_LIMITS);
            let mut width_rasterizer = rasterizer(ROOMY_LIMITS);
            let zero_pictures = pictures_at(&mut zero_rasterizer, &zero_cues, moment);
            let width_pictures = pictures_at(&mut width_rasterizer, &width_cues, moment);
            assert!(
                !zero_pictures.is_empty(),
                "{name}, {moment} ms: the visible letters are drawn"
            );
            assert_eq!(
                zero_pictures, width_pictures,
                "{name}, {moment} ms: the zero-size letter must hand its karaoke tags on like a zero-width one"
            );
        }
    }
}
