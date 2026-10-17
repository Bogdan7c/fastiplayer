//! Меню аудиодорожек, подписи, выбор «следующей» и воронка пользовательского выбора.

use std::cell::RefCell;

use media_core::{TrackDisposition, TrackId, TrackKind, TrackLabels, TrackLanguage};
use player_core::{PlayerCommand, PlayerSnapshot, PlayerWorkerSendError, TrackSummarySnapshot};

use super::control_actions::{
    AudioTrackSelectionFeedback, NO_AUDIO_MESSAGE, NextTrackPlan, PlayerCommandPort,
    SINGLE_TRACK_MESSAGE, plan_next_track, send_audio_selection,
};
use super::labels::{AudioTrackNumber, audio_selected_toast, audio_track_label};
use super::{AudioTrackMenuTarget, AudioTrackMenuView};

/// Цель строки меню для встроенной дорожки с данным id.
fn embedded(raw_id: u32) -> AudioTrackMenuTarget {
    AudioTrackMenuTarget::EmbeddedTrack(TrackId::new(raw_id))
}

fn labels(language: Option<&str>, title: Option<&str>) -> TrackLabels {
    TrackLabels {
        language: language.map(TrackLanguage::new),
        title: title.map(str::to_owned),
        disposition: TrackDisposition::default(),
    }
}

fn track(
    raw_id: u32,
    kind: TrackKind,
    codec_id: &str,
    labels: TrackLabels,
) -> TrackSummarySnapshot {
    TrackSummarySnapshot {
        id: TrackId::new(raw_id),
        kind,
        codec_id: codec_id.to_owned(),
        sample_rate: None,
        channels: None,
        duration: None,
        video: None,
        video_color_summary: None,
        labels,
    }
}

/// Snapshot как у `two-audio-tracks.mkv`: видео, FLAC «Dub» (default, rus), Opus «Original» (eng).
fn two_audio_snapshot(selected: Option<u32>) -> PlayerSnapshot {
    let mut dub_labels = labels(Some("rus"), Some("Dub"));
    dub_labels.disposition.is_default = true;
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = vec![
        track(
            1,
            TrackKind::Video,
            "V_VP8",
            labels(Some("eng"), Some("Main video")),
        ),
        track(2, TrackKind::Audio, "A_FLAC", dub_labels),
        track(
            3,
            TrackKind::Audio,
            "A_OPUS",
            labels(Some("eng"), Some("Original")),
        ),
    ];
    snapshot.selected_tracks.audio_track = selected.map(TrackId::new);
    snapshot
}

fn view_labels(view: &AudioTrackMenuView) -> Vec<(&str, bool)> {
    view.tracks()
        .iter()
        .map(|item| (item.label.as_str(), item.is_selected))
        .collect()
}

#[test]
fn no_media_and_single_audio_track_have_no_choice() {
    assert!(!AudioTrackMenuView::from_snapshot(&PlayerSnapshot::empty()).has_choice());

    let mut single = PlayerSnapshot::empty();
    single.tracks = vec![
        track(1, TrackKind::Video, "V_VP8", TrackLabels::default()),
        track(2, TrackKind::Audio, "A_OPUS", TrackLabels::default()),
    ];
    let view = AudioTrackMenuView::from_snapshot(&single);
    assert_eq!(view.track_count(), 1, "the video track is not an audio row");
    assert!(!view.has_choice());
}

#[test]
fn two_audio_tracks_give_two_labeled_rows_and_selection_follows_the_snapshot() {
    let view = AudioTrackMenuView::from_snapshot(&two_audio_snapshot(Some(2)));
    assert!(view.has_choice());
    assert_eq!(
        view_labels(&view),
        [
            ("Russian — Dub (FLAC)", true),
            ("English — Original (Opus)", false)
        ]
    );

    let after_switch = AudioTrackMenuView::from_snapshot(&two_audio_snapshot(Some(3)));
    assert_eq!(
        view_labels(&after_switch),
        [
            ("Russian — Dub (FLAC)", false),
            ("English — Original (Opus)", true)
        ]
    );
}

#[test]
fn untagged_track_is_named_by_its_audio_number_not_its_container_id() {
    let mut snapshot = two_audio_snapshot(Some(2));
    snapshot
        .tracks
        .push(track(9, TrackKind::Audio, "A_AAC", TrackLabels::default()));
    let view = AudioTrackMenuView::from_snapshot(&snapshot);
    assert_eq!(view.tracks()[2].label, "Track 3 (AAC)");
}

#[test]
fn label_parts_are_skipped_when_the_container_has_no_data() {
    let number = AudioTrackNumber::from_menu_index(1);
    assert_eq!(
        audio_track_label(&labels(Some("rus"), None), "A_FLAC", number),
        "Russian (FLAC)"
    );
    assert_eq!(
        audio_track_label(&labels(None, Some("Dub")), "A_OPUS", number),
        "Dub (Opus)"
    );
    assert_eq!(
        audio_track_label(&labels(Some("und"), None), "", number),
        "Track 2"
    );
    assert_eq!(
        audio_track_label(&labels(Some("tlh"), None), "A_AC3", number),
        "tlh (AC-3)"
    );
    assert_eq!(
        audio_track_label(&labels(Some("ru"), None), "A_EAC3", number),
        "Russian (E-AC-3)"
    );
    assert_eq!(
        audio_track_label(&labels(None, None), "A_DTS/EXPRESS", number),
        "Track 2 (DTS)"
    );
}

#[test]
fn commentary_flag_is_shown_next_to_the_codec() {
    let mut commentary = labels(Some("eng"), Some("Director"));
    commentary.disposition.is_commentary = true;
    assert_eq!(
        audio_track_label(&commentary, "A_AAC", AudioTrackNumber::from_menu_index(0)),
        "English — Director (AAC, commentary)"
    );
}

#[test]
fn next_track_cycles_and_wraps_around() {
    let first_selected = AudioTrackMenuView::from_snapshot(&two_audio_snapshot(Some(2)));
    assert_eq!(
        first_selected.next_track().map(|item| item.target),
        Some(embedded(3))
    );
    let last_selected = AudioTrackMenuView::from_snapshot(&two_audio_snapshot(Some(3)));
    assert_eq!(
        last_selected.next_track().map(|item| item.target),
        Some(embedded(2))
    );

    let mut three = two_audio_snapshot(Some(3));
    three
        .tracks
        .push(track(4, TrackKind::Audio, "A_AAC", TrackLabels::default()));
    let view = AudioTrackMenuView::from_snapshot(&three);
    assert_eq!(view.next_track().map(|item| item.target), Some(embedded(4)));
}

#[test]
fn without_a_selection_the_default_track_counts_as_current() {
    // Default — «Dub» (id 2): следующая после неё — «Original» (id 3).
    let view = AudioTrackMenuView::from_snapshot(&two_audio_snapshot(None));
    assert_eq!(view.next_track().map(|item| item.target), Some(embedded(3)));

    // Ни выбора, ни default: текущей считается первая, следующая — вторая.
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.tracks = vec![
        track(5, TrackKind::Audio, "A_AAC", TrackLabels::default()),
        track(6, TrackKind::Audio, "A_AAC", TrackLabels::default()),
    ];
    let view = AudioTrackMenuView::from_snapshot(&snapshot);
    assert_eq!(view.next_track().map(|item| item.target), Some(embedded(6)));
}

#[test]
fn b_plan_distinguishes_no_audio_single_track_and_switch() {
    let none = plan_next_track(&AudioTrackMenuView::default());
    assert_eq!(none, NextTrackPlan::NoAudio);
    assert_eq!(none.notice(), Some(NO_AUDIO_MESSAGE));

    let mut single = PlayerSnapshot::empty();
    single.tracks = vec![track(2, TrackKind::Audio, "A_OPUS", TrackLabels::default())];
    let single_plan = plan_next_track(&AudioTrackMenuView::from_snapshot(&single));
    assert_eq!(single_plan, NextTrackPlan::SingleTrack);
    assert_eq!(
        single_plan.notice(),
        Some("This video has only one audio track")
    );
    assert_eq!(SINGLE_TRACK_MESSAGE, "This video has only one audio track");

    let switch = plan_next_track(&AudioTrackMenuView::from_snapshot(&two_audio_snapshot(
        Some(2),
    )));
    assert_eq!(
        switch,
        NextTrackPlan::Switch {
            target: embedded(3),
            label: "English — Original (Opus)".to_owned(),
        }
    );
    assert_eq!(switch.notice(), None);
}

#[test]
fn selection_toast_has_the_audio_prefix() {
    assert_eq!(
        audio_selected_toast("Russian — Dub (FLAC)"),
        "Audio: Russian — Dub (FLAC)"
    );
}

/// Заглушка очереди команд плеера: записывает отправленное, отвечает заданным результатом.
struct FakePort {
    sent: RefCell<Vec<PlayerCommand>>,
    result: Result<(), PlayerWorkerSendError>,
}

impl FakePort {
    fn answering(result: Result<(), PlayerWorkerSendError>) -> Self {
        Self {
            sent: RefCell::new(Vec::new()),
            result,
        }
    }
}

impl PlayerCommandPort for FakePort {
    fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.sent.borrow_mut().push(command);
        self.result
    }
}

#[test]
fn funnel_sends_exactly_select_audio_track_for_a_different_track() {
    let port = FakePort::answering(Ok(()));
    let feedback = send_audio_selection(&port, Some(TrackId::new(2)), TrackId::new(3));
    assert_eq!(feedback, AudioTrackSelectionFeedback::Requested);
    assert_eq!(
        *port.sent.borrow(),
        [PlayerCommand::SelectAudioTrack(TrackId::new(3))]
    );
}

#[test]
fn funnel_sends_nothing_when_the_track_is_already_selected() {
    let port = FakePort::answering(Ok(()));
    let feedback = send_audio_selection(&port, Some(TrackId::new(3)), TrackId::new(3));
    assert_eq!(feedback, AudioTrackSelectionFeedback::AlreadySelected);
    assert!(port.sent.borrow().is_empty());
}

#[test]
fn funnel_with_no_selection_yet_still_sends() {
    let port = FakePort::answering(Ok(()));
    let feedback = send_audio_selection(&port, None, TrackId::new(2));
    assert_eq!(feedback, AudioTrackSelectionFeedback::Requested);
    assert_eq!(port.sent.borrow().len(), 1);
}

#[test]
fn funnel_reports_a_full_queue_and_a_dead_worker_separately() {
    for error in [
        PlayerWorkerSendError::Full,
        PlayerWorkerSendError::Disconnected,
    ] {
        let port = FakePort::answering(Err(error));
        let feedback = send_audio_selection(&port, None, TrackId::new(3));
        assert_eq!(feedback, AudioTrackSelectionFeedback::SendFailed(error));
    }
}
