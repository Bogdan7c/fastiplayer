//! Общие помощники функциональных тестов анимации ASS (слой 2): сценарий-«сайдкар» рядом с
//! «видео», владелец с загруженной дорожкой и разбор того, что показал кадр.

use std::collections::BTreeMap;

use subtitle_raster::SubtitleOverlay;

use super::super::SubtitleOwner;
use super::*;

/// Ширина и высота PlayRes сценариев: равны холсту тестов потока (`render_thread::WIDTH/HEIGHT`).
pub(super) const SCRIPT_WIDTH: u32 = 1280;
pub(super) const SCRIPT_HEIGHT: u32 = 720;

/// Сценарий ASS: один стиль без обводки и тени, красный текст, события вида
/// `(начало, конец, текст_с_тегами)`; время — в формате ASS `h:mm:ss.cc`.
pub(super) fn animated_script(play_res: (u32, u32), events: &[(String, String, String)]) -> String {
    let mut script = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {}\nPlayResY: {}\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,{},&H000000FF,&H000000FF,&H00000000,&H00000000,1,0,0,0,100,100,0,0,1,0,0,5,0,0,0,1\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        play_res.0,
        play_res.1,
        play_res.1 / 12,
    );
    for (start, end, text) in events {
        script.push_str(&format!(
            "Dialogue: 0,{start},{end},Default,,0,0,0,,{text}\n"
        ));
    }
    script
}

/// Владелец с загруженной дорожкой из `script` (файл `Movie.ass` рядом с «видео»).
pub(super) fn owner_with_script(
    media: MediaInstanceId,
    script: &str,
) -> (SubtitleOwner, tempfile::TempDir) {
    let (directory, video) = super::track_selection::movie_with(&[("Movie.ass", script)]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, media, &video);
    super::track_selection::settle(&mut owner);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    (owner, directory)
}

/// Левый край картинок каждой реплики (по идентификатору события) в overlay.
pub(super) fn left_edges_by_event(overlay: &SubtitleOverlay) -> BTreeMap<u32, i32> {
    let mut edges: BTreeMap<u32, i32> = BTreeMap::new();
    for bitmap in overlay.bitmaps() {
        let Some(key) = bitmap.cue_key() else {
            continue;
        };
        let left = bitmap.origin().x;
        edges
            .entry(key.event().get())
            .and_modify(|known| *known = (*known).min(left))
            .or_insert(left);
    }
    edges
}
