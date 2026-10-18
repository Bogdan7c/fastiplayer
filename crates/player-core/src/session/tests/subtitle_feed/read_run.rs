//! Номер прохода чтения (`SubtitleReadRun`) на пакетах субтитров: растёт при seek, иначе стоит.

use super::*;
use crate::subtitle_feed::SubtitleReadRun;

fn runs_of(messages: &[SubtitleFeedMessage]) -> Vec<SubtitleReadRun> {
    packets_of(messages)
        .iter()
        .map(|packet| packet.read_run)
        .collect()
}

fn seek_to(harness: &mut FeedHarness, position_ms: u64) {
    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_millis(position_ms),
        )))
        .expect("seek is accepted");
}

#[test]
fn packets_without_a_seek_all_share_one_read_run_across_ticks() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);

    run_ticks(&mut harness.session, 1);
    let first = runs_of(&drain(&harness.receiver));
    run_ticks(&mut harness.session, 5);
    let later = runs_of(&drain(&harness.receiver));

    assert_eq!(first.len(), 3, "все три субтитровых пакета получены");
    assert!(
        first
            .iter()
            .chain(&later)
            .all(|run| *run == SubtitleReadRun::INITIAL),
        "без seek номер прохода не меняется: {first:?} / {later:?}"
    );
}

#[test]
fn seek_starts_a_new_read_run_for_the_packets_read_after_it() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);
    run_ticks(&mut harness.session, 3);
    let before_seek = runs_of(&drain(&harness.receiver));

    seek_to(&mut harness, 10);
    run_ticks(&mut harness.session, 6);
    let after_seek = runs_of(&drain(&harness.receiver));

    assert_eq!(before_seek, vec![SubtitleReadRun::INITIAL; 3]);
    let expected_run = SubtitleReadRun::INITIAL.next();
    assert_eq!(
        after_seek,
        vec![expected_run; 3],
        "перечитанные после seek пакеты идут в следующем проходе"
    );
}

#[test]
fn every_seek_advances_the_read_run_by_one() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);
    run_ticks(&mut harness.session, 3);
    drain(&harness.receiver);

    let mut seen = Vec::new();
    for _ in 0..2 {
        seek_to(&mut harness, 10);
        run_ticks(&mut harness.session, 6);
        let runs = runs_of(&drain(&harness.receiver));
        seen.push(runs.first().copied().map(SubtitleReadRun::get));
    }

    assert_eq!(seen, vec![Some(1), Some(2)]);
}

#[test]
fn real_mkv_packets_keep_one_run_until_a_seek_and_get_the_next_one_after_it() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../symphonia-demux/tests/fixtures/subtitles/two-srt-tracks.mkv");
    let demuxer =
        symphonia_demux::SymphoniaDemuxer::from_file(&fixture).expect("subtitle fixture must open");
    let mut harness = harness_with_demuxer(1_024, Box::new(demuxer));

    run_ticks(&mut harness.session, 20);
    let first_pass = runs_of(&drain(&harness.receiver));
    seek_to(&mut harness, 0);
    run_ticks(&mut harness.session, 20);
    let second_pass = runs_of(&drain(&harness.receiver));

    assert!(!first_pass.is_empty(), "реальный MKV отдал субтитры");
    assert!(
        first_pass
            .iter()
            .all(|run| *run == SubtitleReadRun::INITIAL)
    );
    assert!(
        !second_pass.is_empty(),
        "после seek субтитры прочитаны снова"
    );
    assert!(
        second_pass
            .iter()
            .all(|run| *run == SubtitleReadRun::INITIAL.next())
    );
}

#[test]
fn snapshot_shows_the_current_read_run_before_and_after_a_seek_even_without_new_packets() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);
    run_ticks(&mut harness.session, 3);
    drain(&harness.receiver);
    assert_eq!(
        harness
            .session
            .snapshot_with_frame_counters(FrameCounters::default())
            .subtitle_read_run,
        SubtitleReadRun::INITIAL
    );

    // Seek без единого тика после него: пакетов нового прохода ещё нет, а снимок уже новый.
    seek_to(&mut harness, 10);
    assert!(packets_of(&drain(&harness.receiver)).is_empty());
    assert_eq!(
        harness
            .session
            .snapshot_with_frame_counters(FrameCounters::default())
            .subtitle_read_run,
        SubtitleReadRun::INITIAL.next(),
        "приложение должно узнать о разрыве по снимку, не дожидаясь пакета"
    );
}

#[test]
fn a_full_feed_drops_the_packet_and_the_next_packet_starts_a_new_read_run() {
    // Очередь на 2 сообщения: MediaStarted + первый пакет, остальные теряются.
    let mut harness = harness_with(2, standard_tracks(), interleaved_packets(), None);
    run_ticks(&mut harness.session, 3);
    assert_ne!(
        harness
            .session
            .snapshot_with_frame_counters(FrameCounters::default())
            .subtitle_read_run,
        SubtitleReadRun::INITIAL,
        "потеря пакета двигает проход чтения"
    );
    // После освобождения очереди следующий принятый пакет уже несёт новый номер.
    drain(&harness.receiver);
    run_ticks(&mut harness.session, 3);
    let runs = runs_of(&drain(&harness.receiver));
    assert!(
        runs.iter().all(|run| *run != SubtitleReadRun::INITIAL),
        "{runs:?}"
    );
}
