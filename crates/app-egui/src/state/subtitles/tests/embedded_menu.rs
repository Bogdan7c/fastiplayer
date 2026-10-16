//! Встроенные дорожки в меню CC: подписи из снимка плеера, серые неподдерживаемые,
//! идемпотентная синхронизация списка.

use media_core::{TrackId, TrackKind, subtitle_codec};
use subtitle_core::SubtitleFormat;

use super::super::menu_view::SubtitleFileOrigin;
use super::embedded_support::*;
use super::*;

#[test]
fn labels_in_the_menu_come_from_the_player_snapshot() {
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let owner = embedded_owner(&enabled_config(), media, &fixture);

    let view = owner.menu_view();

    assert!(
        view.has_tracks,
        "embedded tracks alone make the CC button appear"
    );
    assert_eq!(
        view.tracks.len(),
        2,
        "video and audio rows are not in the menu"
    );
    let english = &view.tracks[0].label;
    assert_eq!(english.language.as_ref().map(|l| l.as_str()), Some("eng"));
    assert_eq!(english.title.as_deref(), Some("English"));
    assert_eq!(english.origin, SubtitleFileOrigin::Embedded);
    assert_eq!(english.format, Some(SubtitleFormat::Srt));
    assert!(!english.is_forced && !english.is_hearing_impaired);
    let russian = &view.tracks[1].label;
    assert_eq!(russian.language.as_ref().map(|l| l.as_str()), Some("rus"));
    assert_eq!(russian.title.as_deref(), Some("Russian SDH"));
    assert!(russian.is_forced, "forced flag comes from the container");
    assert!(russian.is_hearing_impaired);
    assert!(
        view.tracks
            .iter()
            .all(|track| matches!(track.availability, SubtitleTrackAvailability::Available))
    );
}

#[test]
fn bitmap_and_unknown_codecs_are_listed_greyed_and_cannot_be_selected() {
    let media = instance(2);
    let mut fixture = demux_fixture("two-srt-tracks.mkv");
    for (raw_id, codec) in [
        (80, subtitle_codec::HDMV_PGS),
        (81, subtitle_codec::VOBSUB),
        (82, subtitle_codec::DVBSUB),
    ] {
        let mut row = fixture.subtitle_row(0).clone();
        row.id = TrackId::new(raw_id);
        row.codec_id = codec.to_owned();
        fixture.tracks.push(row);
    }
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);

    let view = owner.menu_view();

    assert_eq!(view.tracks.len(), 5);
    for greyed in &view.tracks[2..] {
        assert_eq!(
            greyed.availability,
            SubtitleTrackAvailability::Unavailable(
                SubtitleUnavailableReason::FormatNotSupportedYet
            )
        );
    }
    assert_eq!(view.tracks[2].label.format, Some(SubtitleFormat::Pgs));
    assert_eq!(view.tracks[3].label.format, None);
    let pgs_menu_id = view.tracks[2].id;
    assert_eq!(
        owner.select_track(pgs_menu_id),
        SelectTrackOutcome::TrackUnavailable
    );
    assert!(
        !owner.menu_view().tracks[2].is_selected,
        "a greyed track never becomes selected"
    );
    assert!(
        matches!(
            owner.select_next_track(),
            CycleOutcome::Track(_) | CycleOutcome::Off
        ),
        "cycling skips unavailable tracks without panicking"
    );
}

#[test]
fn syncing_an_unchanged_snapshot_changes_nothing_and_keeps_ids_and_cached_cues() {
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let english = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    let ids_before: Vec<_> = owner.menu_view().tracks.iter().map(|t| t.id).collect();

    let update = owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    assert_eq!(update, EmbeddedUpdate::Unchanged);
    let ids_after: Vec<_> = owner.menu_view().tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids_before, ids_after);
    assert_eq!(
        cached_cues(&owner, english),
        2,
        "the cache survives a no-op sync"
    );
}

#[test]
fn snapshot_of_another_media_never_touches_the_list() {
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let mut owner = embedded_owner(&enabled_config(), instance(2), &fixture);

    let update = owner.sync_embedded_tracks(Some(instance(9)), &[]);

    assert_eq!(update, EmbeddedUpdate::Unchanged);
    assert_eq!(owner.menu_view().tracks.len(), 2);
    assert_eq!(
        owner.sync_embedded_tracks(None, &[]),
        EmbeddedUpdate::Unchanged
    );
}

#[test]
fn tracks_changed_adds_new_removes_gone_and_keeps_the_others() {
    let media = instance(2);
    let mut fixture = demux_fixture("two-srt-tracks.mkv");
    let english = fixture.subtitle_row(0).id;
    let russian = fixture.subtitle_row(1).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    let english_menu_id = owner_track_id(&owner, english);

    // Русская дорожка исчезла, появилась новая.
    let mut added = fixture.subtitle_row(0).clone();
    added.id = TrackId::new(70);
    added.labels.title = Some("Added".to_owned());
    fixture.tracks.retain(|row| row.id != russian);
    fixture.tracks.push(added);
    let update = owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    assert_eq!(update, EmbeddedUpdate::Changed);
    let view = owner.menu_view();
    let titles: Vec<_> = view.tracks.iter().map(|t| t.label.title.clone()).collect();
    assert_eq!(
        titles,
        [Some("English".to_owned()), Some("Added".to_owned())]
    );
    assert_eq!(owner_track_id(&owner, english), english_menu_id);
    assert_eq!(cached_cues(&owner, english), 2, "kept track keeps its cues");
    assert!(
        fixture
            .tracks
            .iter()
            .filter(|row| row.kind == TrackKind::Subtitle)
            .count()
            == 2
    );
}

#[test]
fn removing_the_selected_track_clears_the_selection() {
    let media = instance(2);
    let mut fixture = demux_fixture("two-srt-tracks.mkv");
    let english = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    assert!(
        owner.menu_view().tracks[0].is_selected,
        "default English is auto-selected"
    );

    fixture.tracks.retain(|row| row.id != english);
    owner.sync_embedded_tracks(Some(media), &fixture.tracks);

    // Автовыбор переподбирает оставшуюся дорожку, удалённая не остаётся выбранной.
    let view = owner.menu_view();
    assert_eq!(view.tracks.len(), 1);
    assert_eq!(view.tracks[0].label.title.as_deref(), Some("Russian SDH"));
}
