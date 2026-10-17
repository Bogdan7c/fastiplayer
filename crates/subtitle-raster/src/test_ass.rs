//! Помощники тестов ASS: файл `.ass` из строк -> реплики с настоящими идентификаторами.
//!
//! Реплики проходят через тот же `subtitle_formats::load_text_subtitle`, что и в продакшене,
//! поэтому тесты рисования проверяют связку «разбор + растеризатор», а не ручную сборку тегов.

use subtitle_core::{
    AssCue, SubtitleContent, SubtitleEventId, SubtitleLayer, SubtitleTimeline,
    TimelineInsertOutcome,
};
use subtitle_formats::{
    AssParseLimits, LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};

use crate::renderer::{CueKey, CueSourceId, RenderableCue};

/// Лимиты, с которыми разбор не мешает тестам.
fn parse_limits() -> SubtitleParseLimits {
    SubtitleParseLimits::new(1 << 22, 100_000, 100).with_ass_limits(AssParseLimits {
        max_header_bytes: 1 << 20,
        max_styles: 64,
        max_override_tags_per_event: 256,
        max_tracked_unknown_tag_names: 16,
    })
}

/// Реплика ASS теста: ключ, слой и содержимое.
pub(crate) struct AssTestCue {
    pub(crate) key: CueKey,
    pub(crate) layer: SubtitleLayer,
    pub(crate) cue: AssCue,
}

impl AssTestCue {
    /// Запись для запроса на кадр.
    pub(crate) fn renderable(&self) -> RenderableCue<'_> {
        RenderableCue::ass(self.key, self.layer, &self.cue)
    }
}

/// Стиль `Default` теста: Noto Sans 48, белый, контур 3, тень 0, снизу по центру, поля 40.
pub(crate) const DEFAULT_STYLE_LINE: &str = "Style: Default,Noto Sans,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,3,0,2,40,40,40,1";

/// Собирает файл ASS: заголовок, стили и события (строки `Dialogue:` целиком).
pub(crate) fn ass_file(
    play_res: (u32, u32),
    scaled_border_and_shadow: bool,
    style_lines: &[&str],
    dialogue_lines: &[&str],
) -> String {
    let scaled = if scaled_border_and_shadow {
        "yes"
    } else {
        "no"
    };
    let mut text = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {}\nPlayResY: {}\nScaledBorderAndShadow: {scaled}\nWrapStyle: 0\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
        play_res.0, play_res.1
    );
    for line in style_lines {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    for line in dialogue_lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// Строка `Dialogue:` стиля `Default` на слое `layer` с текстом `text`.
pub(crate) fn dialogue(layer: i32, text: &str) -> String {
    format!("Dialogue: {layer},0:00:00.00,0:00:10.00,Default,,0,0,0,,{text}")
}

/// Загружает реплики ASS из текста файла; события получают ключи источника `source`.
pub(crate) fn load_ass_cues(file_text: &str, source: u64) -> Vec<AssTestCue> {
    let loaded = load_text_subtitle(
        file_text.as_bytes(),
        TextSubtitleFormat::Ass,
        LegacyEncodingHint::Generic,
        &parse_limits(),
    )
    .expect("ASS test file must load");
    let mut timeline = SubtitleTimeline::new();
    let mut ids: Vec<(SubtitleEventId, SubtitleLayer)> = Vec::new();
    for event in loaded.document().events() {
        match timeline.insert(event.clone()) {
            TimelineInsertOutcome::Inserted(id) => ids.push((id, event.layer())),
            other => panic!("test events must be distinct: {other:?}"),
        }
    }
    ids.into_iter()
        .map(|(id, layer)| {
            let event = timeline.event(id).expect("inserted event");
            let SubtitleContent::Ass(cue) = event.content() else {
                panic!("ASS file must produce ASS events");
            };
            AssTestCue {
                key: CueKey::new(CueSourceId::new(source), id),
                layer,
                cue: cue.clone(),
            }
        })
        .collect()
}
