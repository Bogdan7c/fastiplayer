//! ASS/SSA рядом с видео и общие помощники ASS-тестов: файл `Movie.ass` выбираем, читается,
//! события — ASS, и реплика доходит до кадра (а не просто «читается»).

use subtitle_core::{SubtitleContent, SubtitleEvent};

use super::super::menu_view::{
    SubtitleFileProblem, SubtitleTrackAvailability, SubtitleUnavailableReason,
};
use super::super::track_list::TrackLoadState;

use super::track_selection::{movie_with, overlay_present, settle};
use super::*;

/// Минимальный настоящий ASS: PlayRes 384x288, один стиль, одна реплика 1.0–3.0 с с тегом `\b`.
pub(super) const SAMPLE_ASS: &str = "[Script Info]\n\
ScriptType: v4.00+\n\
PlayResX: 384\n\
PlayResY: 288\n\
\n\
[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,2,2,10,10,10,1\n\
\n\
[Events]\n\
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\b1}First{\\b0} cue\n";

/// События дорожки, которую владелец показывает сейчас (копия: вызывающему не нужен борроу).
pub(super) fn displayed_events(owner: &SubtitleOwner) -> Vec<SubtitleEvent> {
    let media = owner.media.as_ref().expect("media");
    let id = media.display_track_id().expect("a track is displayed");
    match &media.tracks.get(id).expect("entry").load {
        TrackLoadState::Ready(track) => track
            .timeline()
            .iter()
            .map(|(_, event)| event.clone())
            .collect(),
        _ => panic!("the displayed track must be loaded"),
    }
}

/// `Movie.ass` рядом с видео не серый: выбран автовыбором, прочитан, события — ASS со
/// стилем и тегом, и реплика даёт пиксели на кадре без «неподдержанных» реплик.
#[test]
fn movie_ass_next_to_the_video_is_selected_loaded_as_ass_and_rendered() {
    let (_dir, video) = movie_with(&[("Movie.ass", SAMPLE_ASS)]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, instance(1), &video);
    settle(&mut owner);

    let view = owner.menu_view();
    assert_eq!(view.tracks.len(), 1);
    assert_eq!(
        view.tracks[0].availability,
        SubtitleTrackAvailability::Available,
        "ASS is no longer greyed out"
    );
    assert!(view.tracks[0].is_selected, "auto-selected like an SRT");

    let events = displayed_events(&owner);
    assert_eq!(events.len(), 1);
    let SubtitleContent::Ass(cue) = events[0].content() else {
        panic!(
            "the sidecar event must be ASS content, got {:?}",
            events[0].content()
        );
    };
    assert_eq!(cue.style().name, "Default");
    assert_eq!(cue.style().font_name, "Arial");
    assert!(
        cue.text.iter().any(|item| matches!(
            item,
            subtitle_core::AssTextItem::Overrides(tags)
                if tags.iter().any(|tag| matches!(tag, subtitle_core::AssOverride::Bold(_)))
        )),
        "the \\b override is parsed"
    );

    assert!(
        overlay_present(&mut owner, instance(1), 1_500),
        "pixels at 1.5 s"
    );
    assert!(
        !overlay_present(&mut owner, instance(1), 5_000),
        "nothing after the cue"
    );
    assert_eq!(
        owner.presentation_stats().skipped_unsupported_cues,
        0,
        "ASS cues are rendered, not skipped"
    );
}

/// Мусор вместо сценария в `.ass` не роняет приложение: разбор ASS снисходителен (как у
/// libass), но файл без единой реплики — недоступная дорожка с понятной причиной в меню.
#[test]
fn garbage_ass_file_is_an_unavailable_track_without_a_crash() {
    let (_dir, video) = movie_with(&[("Movie.ass", "this is not a subtitle script at all")]);
    let mut owner = owner_with_config(&enabled_config());
    begin_and_wait(&mut owner, instance(1), &video);
    settle(&mut owner);

    let view = owner.menu_view();
    assert_eq!(
        view.tracks[0].availability,
        SubtitleTrackAvailability::Unavailable(SubtitleUnavailableReason::LoadFailed(
            SubtitleFileProblem::CannotParse
        ))
    );
    assert!(!overlay_present(&mut owner, instance(1), 1_500));
}
