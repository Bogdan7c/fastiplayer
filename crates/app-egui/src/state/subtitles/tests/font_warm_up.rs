//! Прогрев шрифтов от презентатора до настоящего потока: просьба уходит один раз на дорожку,
//! и только если в её тексте есть что прогревать.

use super::super::presentation::worker_link::SentCommandKind;
use super::render_thread::{owner_with_srt, shown_events};
use super::*;

const CJK_TRACK: &str = "1\n00:00:01,000 --> 00:00:03,000\n漢字の字幕\n";
const ASCII_TRACK: &str = "1\n00:00:01,000 --> 00:00:03,000\nHello there\n";
/// Латиница с диакритикой: ниже порога образца (встроенные шрифты покрывают её «по определению»).
const LATIN_EXTENDED_TRACK: &str = "1\n00:00:01,000 --> 00:00:03,000\nÀ la carte, œuvre\n";

fn warm_up_requests(sent: &[SentCommandKind]) -> usize {
    sent.iter()
        .filter(|kind| **kind == SentCommandKind::WarmUpFonts)
        .count()
}

#[test]
fn cjk_track_asks_the_worker_to_warm_up_once_and_the_worker_does_it() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, CJK_TRACK);
    let _setup = owner.presenter.take_sent_command_log();

    // Несколько кадров: просьба уходит при первой синхронизации дорожки, не на каждом кадре.
    for pts_ms in [500, 1_500, 2_000] {
        shown_events(&mut owner, media, pts_ms);
    }

    let sent = owner.presenter.take_sent_command_log();
    assert_eq!(warm_up_requests(&sent), 1, "sent: {sent:?}");
    let replace = sent
        .iter()
        .position(|kind| *kind == SentCommandKind::ChangeTrack)
        .expect("the track is mirrored");
    let warm_up = sent
        .iter()
        .position(|kind| *kind == SentCommandKind::WarmUpFonts)
        .expect("warm-up is requested");
    assert!(replace < warm_up, "the worker must know the track first");

    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert!(
        diagnostics.last_font_warm_up.is_some(),
        "the worker must have executed the warm-up"
    );
}

#[test]
fn latin_only_tracks_do_not_ask_for_a_warm_up() {
    for (name, srt) in [("ascii", ASCII_TRACK), ("latin", LATIN_EXTENDED_TRACK)] {
        let media = instance(1);
        let (mut owner, _dir) = owner_with_srt(media, srt);
        let _setup = owner.presenter.take_sent_command_log();

        shown_events(&mut owner, media, 1_500);

        let sent = owner.presenter.take_sent_command_log();
        assert_eq!(warm_up_requests(&sent), 0, "{name}: {sent:?}");
        let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
        assert_eq!(diagnostics.last_font_warm_up, None, "{name}");
    }
}
