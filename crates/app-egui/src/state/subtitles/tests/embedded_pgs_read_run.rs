//! Проход чтения демуксера из СНИМКА плеера и устаревшие пакеты трубы (PGS): после seek
//! открытая композиция снимается без ожидания пакета, а хвост трубы от времени до seek не
//! воскрешает её. Тесты идут по настоящей цепочке «пакеты -> владелец -> поток-рисовальщик
//! -> overlay» на фикстуре `pgs-basic.mkv` (картинка 0.5 с без конца, «очистка» 1.5 с,
//! forced-картинка 2.0 с).

use super::embedded_pgs::*;
use super::embedded_support::*;
use super::*;
use crate::state::subtitles::owner_embedded::PgsFeedCounters;
use crate::state::subtitles::render_worker::clock_tracker::{ObservedMode, PlaybackObservation};
use crate::state::subtitles::render_worker::schedule::PlaybackRate;
use subtitle_core::SubtitleTime;

/// Наблюдение плеера с позицией `ms` и проходом чтения `run`.
fn observe_run(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    ms: u64,
    run: u32,
) -> EmbeddedUpdate {
    owner.observe_playback(
        current(media, GENERATION),
        PlaybackObservation {
            mode: ObservedMode::Playing,
            rate: PlaybackRate::NORMAL,
            estimated_step: None,
            position: Some(Duration::from_millis(ms)),
            read_run: read_run(run),
        },
    )
}

fn pgs_counters(owner: &SubtitleOwner) -> PgsFeedCounters {
    owner.media.as_ref().expect("media").embedded_pgs.counters()
}

#[test]
fn a_new_read_run_in_the_snapshot_clears_the_open_picture_without_any_new_packet() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_PICTURE]));
    assert!(
        shows_picture(&mut owner, media, 1_200),
        "открытая картинка видна до seek"
    );

    // Seek вперёд: снимок плеера уже показывает новый проход, пакетов нового прохода нет.
    let update = observe_run(&mut owner, media, 2_300, 1);

    assert_eq!(update, EmbeddedUpdate::Changed, "UI должен перерисоваться");
    assert!(
        !shows_picture(&mut owner, media, 2_300),
        "после seek на новой позиции нет залипшей картинки"
    );
    assert!(!shows_picture(&mut owner, media, 1_200));
    assert_eq!(pgs_counters(&owner).discontinuities, 1);
    assert_eq!(stored_compositions(&owner, track), 0);
}

#[test]
fn an_unchanged_or_older_snapshot_run_does_nothing() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(&fixture, media, track, 1, &[FIRST_PICTURE]));

    assert_eq!(
        observe_run(&mut owner, media, 1_000, 1),
        EmbeddedUpdate::Unchanged
    );
    assert_eq!(
        observe_run(&mut owner, media, 1_000, 0),
        EmbeddedUpdate::Unchanged,
        "устаревший снимок не считается разрывом"
    );

    assert!(shows_picture(&mut owner, media, 1_200));
    assert_eq!(pgs_counters(&owner).discontinuities, 0);
    assert_eq!(pgs_counters(&owner).stale_run_packets, 0);
}

#[test]
fn a_late_packet_of_the_old_run_is_ignored_counted_and_does_not_bring_the_picture_back() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_PICTURE]));
    observe_run(&mut owner, media, 2_300, 1);

    // Хвост трубы от времени до seek: пакет прошлого прохода.
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[FIRST_PICTURE]));

    assert_eq!(pgs_counters(&owner).stale_run_packets, 1);
    assert_eq!(stored_compositions(&owner, track), 0, "в кэш не попало");
    assert!(
        !shows_picture(&mut owner, media, 2_300),
        "устаревшая картинка не вернулась"
    );
    assert!(!shows_picture(&mut owner, media, 1_200));
}

#[test]
fn a_newer_packet_run_resets_every_pgs_track_not_only_the_one_that_got_the_packet() {
    let (fixture, first, second) = fixture_with_second_pgs_track();
    let media = instance(2);
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    let mut messages = blocks(&fixture, media, first, 0, &[FIRST_PICTURE]);
    messages.extend(blocks(&fixture, media, second, 0, &[FIRST_PICTURE]));
    owner.deliver_feed_messages(messages);
    assert_eq!(stored_compositions(&owner, first), 1);
    assert_eq!(stored_compositions(&owner, second), 1);

    // Новый проход приходит пакетом только первой дорожки.
    owner.deliver_feed_messages(blocks(&fixture, media, first, 1, &[SECOND_CLEAR]));

    assert_eq!(
        stored_compositions(&owner, second),
        0,
        "открытая композиция второй дорожки тоже снята"
    );
    assert_eq!(pgs_counters(&owner).discontinuities, 2);
}

#[test]
fn a_new_read_run_also_clears_the_forced_view_of_the_track() {
    let (mut owner, media, fixture) = selected_pgs_owner(&enabled_config());
    let track = fixture.subtitle_row(0).id;
    owner.deliver_feed_messages(blocks(&fixture, media, track, 0, &[SECOND_PICTURE]));
    let id = owner_track_id(&owner, track);
    let forced_active = |owner: &SubtitleOwner| {
        let view = owner
            .media
            .as_ref()
            .and_then(|media| media.embedded_pgs.forced_pictures_view(id))
            .expect("forced view");
        let mut active = Vec::new();
        view.timeline()
            .collect_active_at(SubtitleTime::from_millis(2_300), &mut active);
        active.len()
    };
    assert_eq!(forced_active(&owner), 1, "forced-картинка открыта");

    observe_run(&mut owner, media, 2_300, 1);

    assert_eq!(forced_active(&owner), 0, "вид forced тоже сброшен");
}
