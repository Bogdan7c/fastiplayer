//! Озвучки страницы в меню «♪» (14a): заменяют встроенные дорожки, одна отметка «играет»,
//! клавиша `B` ходит по кругу, строки несут выбор с ревизией меню, а не подпись.

use std::sync::Arc;

use media_core::{TrackId, TrackKind, TrackLabels};
use player_core::{PlayerSnapshot, TrackSummarySnapshot};

use super::control_actions::{NextTrackPlan, plan_next_track};
use super::{AudioTrackMenuTarget, AudioTrackMenuView};
use crate::page_variants::{
    PageVariantAvailability, PageVariantChoice, PageVariantIndex, PageVariantKind,
    PageVariantMenuRevision, PageVariantMenuView,
};

/// Ревизия меню озвучек в фикстуре.
const REVISION: u64 = 7;

/// Снимок плеера с `count` встроенными аудиодорожками (как у потока страницы).
fn snapshot_with_embedded_audio(count: u32) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = (1..=count)
        .map(|raw_id| TrackSummarySnapshot {
            id: TrackId::new(raw_id),
            kind: TrackKind::Audio,
            codec_id: "A_AAC".to_owned(),
            sample_rate: None,
            channels: None,
            duration: None,
            video: None,
            video_color_summary: None,
            labels: TrackLabels::default(),
        })
        .collect();
    snapshot.selected_tracks.audio_track = Some(TrackId::new(1));
    snapshot
}

fn dub_menu(labels: &[&str], selected: Option<usize>) -> PageVariantMenuView {
    PageVariantMenuView::new(
        PageVariantKind::Audio,
        PageVariantMenuRevision::new(REVISION),
        labels.iter().map(|label| Arc::from(*label)).collect(),
        selected.map(PageVariantIndex::new),
    )
}

fn dub_choice(index: usize) -> PageVariantChoice {
    PageVariantChoice {
        kind: PageVariantKind::Audio,
        revision: PageVariantMenuRevision::new(REVISION),
        index: PageVariantIndex::new(index),
    }
}

fn view(snapshot: &PlayerSnapshot, menu: &PageVariantMenuView) -> AudioTrackMenuView {
    AudioTrackMenuView::for_page_dubs_test(snapshot, menu, PageVariantAvailability::Selectable)
}

/// Куда ведёт `B` (`None` — переключаться некуда).
fn next_by_b(view: &AudioTrackMenuView) -> Option<AudioTrackMenuTarget> {
    match plan_next_track(view) {
        NextTrackPlan::Switch { target, .. } => Some(target),
        NextTrackPlan::NoAudio | NextTrackPlan::SingleTrack => None,
    }
}

#[test]
fn page_dubs_replace_the_embedded_tracks_with_one_playing_mark() {
    let snapshot = snapshot_with_embedded_audio(2);
    let menu = dub_menu(&["Original", "Ukrainian (dub)", "English"], Some(1));
    let view = view(&snapshot, &menu);

    let rows: Vec<(&str, bool)> = view
        .tracks()
        .iter()
        .map(|row| (row.label.as_str(), row.is_selected))
        .collect();
    assert_eq!(
        rows,
        [
            ("Original", false),
            ("Ukrainian (dub)", true),
            ("English", false)
        ],
        "the embedded tracks are not listed, only the page's dubs, one of them marked"
    );
    let targets: Vec<AudioTrackMenuTarget> = view.tracks().iter().map(|row| row.target).collect();
    assert_eq!(
        targets,
        [0, 1, 2].map(|index| AudioTrackMenuTarget::PageDub(dub_choice(index)))
    );
    assert_eq!(view.availability(), PageVariantAvailability::Selectable);
}

#[test]
fn the_button_shows_for_two_page_dubs_even_with_one_embedded_track() {
    let one_embedded = snapshot_with_embedded_audio(1);
    assert!(
        !AudioTrackMenuView::from_snapshot(&one_embedded).has_choice(),
        "one embedded track alone has no button"
    );
    assert!(view(&one_embedded, &dub_menu(&["Original", "Dub"], Some(0))).has_choice());
    // Одна озвучка всё равно заменяет встроенные дорожки, но выбирать не из чего.
    assert!(
        !view(
            &snapshot_with_embedded_audio(2),
            &dub_menu(&["Original"], Some(0))
        )
        .has_choice()
    );
}

#[test]
fn b_goes_round_the_page_dubs_and_wraps_from_the_last_to_the_first() {
    let snapshot = snapshot_with_embedded_audio(1);
    let labels = ["Original", "Dub A", "Dub B"];
    assert_eq!(
        next_by_b(&view(&snapshot, &dub_menu(&labels, Some(0)))),
        Some(AudioTrackMenuTarget::PageDub(dub_choice(1)))
    );
    assert_eq!(
        next_by_b(&view(&snapshot, &dub_menu(&labels, Some(1)))),
        Some(AudioTrackMenuTarget::PageDub(dub_choice(2)))
    );
    assert_eq!(
        next_by_b(&view(&snapshot, &dub_menu(&labels, Some(2)))),
        Some(AudioTrackMenuTarget::PageDub(dub_choice(0))),
        "the last dub wraps to the first"
    );
    // Страница ничего не отметила: текущей считается первая, `B` ведёт ко второй.
    assert_eq!(
        next_by_b(&view(&snapshot, &dub_menu(&labels, None))),
        Some(AudioTrackMenuTarget::PageDub(dub_choice(1)))
    );
}

#[test]
fn during_an_ad_the_dubs_stay_listed_but_are_marked_blocked() {
    let menu = dub_menu(&["Original", "Dub"], Some(0));
    let view = AudioTrackMenuView::for_page_dubs_test(
        &snapshot_with_embedded_audio(1),
        &menu,
        PageVariantAvailability::BlockedDuringAd,
    );
    assert_eq!(view.track_count(), 2);
    assert_eq!(
        view.availability(),
        PageVariantAvailability::BlockedDuringAd
    );
    // Встроенные дорожки и web-версии от рекламы не блокируются (это не выбор страницы).
    assert_eq!(
        AudioTrackMenuView::from_snapshot(&snapshot_with_embedded_audio(2)).availability(),
        PageVariantAvailability::Selectable
    );
}
