//! Тесты меню качества: подпись-ступень, тексты и снимок из настоящего `PlayerSnapshot`.

use std::time::Duration;

use media_core::TrackKind;
use player_core::PlayerSnapshot;
use pushed_media_source::{
    LaneId, MediaBitrate, PushedContainer, PushedSourcePolicy, pushed_stream,
};

use super::format::{bitrate_text, codec_text, frames_per_second};
use super::journal::{QualityJournalOutcome, QualityLabelJournal};
use super::test_support::snapshot_with_frame;
use super::tier::tier_label;
use super::{QualityMenuView, ShownPicture};
use crate::page_quality::{PageQualityFacts, PageViewOffer};

fn label(width: u32, height: u32) -> String {
    tier_label(ShownPicture { width, height })
}

/// Таблица из задания: ступень, в чей кадр 16:9 помещается картинка.
#[test]
fn the_tier_label_is_the_smallest_standard_box_that_holds_the_picture() {
    for (width, height, expected) in [
        (1920, 1080, "1080p"),
        (1920, 800, "1080p"),
        (1080, 1920, "1080p"),
        (640, 360, "360p"),
        (854, 480, "480p"),
        (1280, 720, "720p"),
        (3840, 1600, "2160p"),
        (1440, 1080, "1080p"),
        (480, 360, "360p"),
        (256, 144, "144p"),
        (64, 64, "64p"),
        (96, 96, "96p"),
        (7680, 4320, "4320p"),
    ] {
        assert_eq!(label(width, height), expected, "{width}x{height}");
    }
}

/// Края: чуть больше кадра ступени — следующая ступень; больше кадра 4320p — короткая сторона.
#[test]
fn the_tier_label_edges() {
    assert_eq!(label(1921, 1080), "1440p");
    assert_eq!(label(1920, 1081), "1440p");
    assert_eq!(label(257, 144), "240p");
    assert_eq!(
        label(7681, 4320),
        "4320p",
        "wider than the 4320p box: short side"
    );
    assert_eq!(label(8000, 4320), "4320p");
    assert_eq!(label(16000, 9000), "9000p");
    assert_eq!(label(143, 1000), "143p");
}

fn bitrate(bits_per_second: u64) -> Option<u64> {
    Some(bits_per_second)
}

/// Настоящий вес потока: `MediaBitrate` строит только источник, поэтому берём его у полки с
/// роликом фикстуры протокола (видео-полоса, позиция плеера 100 мс).
fn real_bitrate() -> MediaBitrate {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream starts");
    let lane = LaneId::new(0);
    writer
        .add_lane(lane, PushedContainer::FragmentedMp4)
        .expect("lane");
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../browser-bridge-protocol/fixtures/media/video.mp4");
    writer
        .append(lane, &std::fs::read(path).expect("fixture media"))
        .expect("append");
    let monitor = writer.monitor();
    monitor.note_playback_position(Duration::from_millis(100));
    monitor
        .video_bitrate_near_position()
        .expect("the shelf has a weight")
}

#[test]
fn the_bitrate_text_switches_to_megabits_at_one_megabit() {
    assert_eq!(bitrate_text(bitrate(850_000)), "850 kbps");
    assert_eq!(bitrate_text(bitrate(999_400)), "999 kbps");
    assert_eq!(bitrate_text(bitrate(999_700)), "1.0 Mbps");
    assert_eq!(bitrate_text(bitrate(1_000_000)), "1.0 Mbps");
    assert_eq!(bitrate_text(bitrate(4_290_000)), "4.3 Mbps");
    assert_eq!(bitrate_text(bitrate(400)), "1 kbps");
    assert_eq!(bitrate_text(None), "—");
}

#[test]
fn codec_names_are_human_and_unknown_ids_are_kept() {
    assert_eq!(codec_text(Some("V_VP9")), "VP9");
    assert_eq!(codec_text(Some("V_AV1")), "AV1");
    assert_eq!(codec_text(Some("avc1")), "H.264");
    assert_eq!(codec_text(Some("V_MYSTERY")), "V_MYSTERY");
    assert_eq!(codec_text(Some("  ")), "—");
    assert_eq!(codec_text(None), "—");
}

#[test]
fn the_frame_rate_is_rounded_from_the_frame_duration() {
    assert_eq!(frames_per_second(Duration::from_micros(16_683)), Some(60));
    assert_eq!(frames_per_second(Duration::from_micros(41_708)), Some(24));
    assert_eq!(frames_per_second(Duration::from_micros(33_333)), Some(30));
    assert_eq!(frames_per_second(Duration::ZERO), None);
    assert_eq!(
        frames_per_second(Duration::from_micros(100)),
        None,
        "10 000 fps is not believable"
    );
    assert_eq!(
        frames_per_second(Duration::from_secs(5)),
        None,
        "0.2 fps is not a video"
    );
}

fn facts(bitrate: Option<MediaBitrate>, page_view: PageViewOffer) -> PageQualityFacts {
    PageQualityFacts { bitrate, page_view }
}

#[test]
fn without_a_frame_there_is_nothing_to_show() {
    let view = QualityMenuView::from_snapshot(
        &PlayerSnapshot::empty(),
        &facts(Some(real_bitrate()), PageViewOffer::Available),
    );
    assert_eq!(view, QualityMenuView::Hidden);
    assert!(!view.is_shown());
    assert_eq!(view.tier_label(), None);
    assert_eq!(view.text(), None);
    assert_eq!(view.journal_entry(), None);
}

#[test]
fn a_frame_with_an_empty_picture_is_hidden() {
    let snapshot = snapshot_with_frame((0, 0));
    assert_eq!(
        QualityMenuView::from_snapshot(&snapshot, &PageQualityFacts::default()),
        QualityMenuView::Hidden
    );
}

/// Главное: кадр 1280×720 + факты → «720p», тексты меню; размер — картинка, а не поверхность.
#[test]
fn a_720p_frame_with_facts_gives_the_label_and_the_menu_texts() {
    let snapshot = snapshot_with_frame((1280, 720));
    let view = QualityMenuView::from_snapshot(
        &snapshot,
        &facts(Some(real_bitrate()), PageViewOffer::Available),
    );
    assert_eq!(view.tier_label().as_deref(), Some("720p"));
    let text = view.text().expect("shown");
    assert_eq!(text.resolution, "1280 × 720");
    assert_eq!(
        text.bitrate,
        bitrate_text(Some(real_bitrate().bits_per_second()))
    );
    assert_ne!(text.bitrate, "—", "a known bitrate is written, not hidden");
    assert_eq!(text.codec, "VP9");
    assert_eq!(text.frame_rate.as_deref(), Some("60 fps"));
    assert_eq!(view.page_view_offer(), PageViewOffer::Available);
}

#[test]
fn unknown_bitrate_codec_and_frame_rate_are_told_honestly() {
    let mut snapshot = snapshot_with_frame((640, 360));
    snapshot.selected_tracks.video_track = None;
    snapshot.video_frame_duration_estimate = Duration::ZERO;
    let view = QualityMenuView::from_snapshot(&snapshot, &facts(None, PageViewOffer::PageAway));
    let text = view.text().expect("shown");
    assert_eq!(text.bitrate, "—");
    assert_eq!(text.codec, "—");
    assert_eq!(text.frame_rate, None);
    assert_eq!(view.page_view_offer(), PageViewOffer::PageAway);
}

/// Выбранная дорожка — не видео: кодека нет (аудио не выдаётся за видео).
#[test]
fn a_selected_track_that_is_not_video_gives_no_codec() {
    let mut snapshot = snapshot_with_frame((640, 360));
    snapshot.tracks[0].kind = TrackKind::Audio;
    let view = QualityMenuView::from_snapshot(&snapshot, &PageQualityFacts::default());
    assert_eq!(view.text().expect("shown").codec, "—");
}

/// Журнал пишет строку только при смене подписи и несёт числа картинки и битрейта.
#[test]
fn the_journal_writes_only_when_the_label_changes() {
    let mut journal = QualityLabelJournal::default();
    let small = QualityMenuView::from_snapshot(
        &snapshot_with_frame((64, 64)),
        &facts(None, PageViewOffer::Available),
    );
    let large = QualityMenuView::from_snapshot(
        &snapshot_with_frame((96, 96)),
        &facts(Some(real_bitrate()), PageViewOffer::Available),
    );
    assert_eq!(
        journal.note(&QualityMenuView::Hidden),
        QualityJournalOutcome::Unchanged
    );
    assert_eq!(journal.note(&small), QualityJournalOutcome::Logged);
    assert_eq!(journal.note(&small), QualityJournalOutcome::Unchanged);
    assert_eq!(
        journal.note(&QualityMenuView::Hidden),
        QualityJournalOutcome::Unchanged
    );
    assert_eq!(journal.note(&large), QualityJournalOutcome::Logged);
    let entry = large.journal_entry().expect("shown");
    assert_eq!(entry.label, "96p");
    assert_eq!(
        entry.picture,
        ShownPicture {
            width: 96,
            height: 96
        }
    );
    assert_eq!(entry.bitrate, Some(real_bitrate()));
}

fn bitrate_of(bits_per_second: u64) -> MediaBitrate {
    MediaBitrate::from_bits_per_second(
        std::num::NonZeroU64::new(bits_per_second).expect("non-zero bitrate"),
    )
}

/// Вторая строка подписи — при первом обновлении числа (неизвестное стало известным).
#[test]
fn the_journal_repeats_a_label_once_when_its_bitrate_becomes_known() {
    let mut journal = QualityLabelJournal::default();
    let unknown = QualityMenuView::from_snapshot(
        &snapshot_with_frame((96, 96)),
        &facts(None, PageViewOffer::Available),
    );
    let known = QualityMenuView::from_snapshot(
        &snapshot_with_frame((96, 96)),
        &facts(Some(real_bitrate()), PageViewOffer::Available),
    );
    assert_eq!(journal.note(&unknown), QualityJournalOutcome::Logged);
    assert_eq!(journal.note(&unknown), QualityJournalOutcome::Unchanged);
    assert_eq!(journal.note(&known), QualityJournalOutcome::Logged);
    assert_eq!(journal.note(&known), QualityJournalOutcome::Unchanged);
    assert_eq!(
        journal.note(&unknown),
        QualityJournalOutcome::Unchanged,
        "a known value that disappears is not news"
    );
}

/// В миг смены 360p → 720p число ещё старое (позиция у источника отстаёт на проход): журнал
/// пишет вторую строку с первым новым числом и дальше о смене сцены молчит.
#[test]
fn the_journal_records_the_first_number_after_a_stale_one_and_then_stays_quiet() {
    let mut journal = QualityLabelJournal::default();
    let view = |size: (u32, u32), bits: u64| {
        QualityMenuView::from_snapshot(
            &snapshot_with_frame(size),
            &facts(Some(bitrate_of(bits)), PageViewOffer::Available),
        )
    };
    assert_eq!(
        journal.note(&view((640, 360), 535_320)),
        QualityJournalOutcome::Logged
    );
    // Кадр уже 720p, а вес ещё у куска 360p.
    assert_eq!(
        journal.note(&view((1280, 720), 509_005)),
        QualityJournalOutcome::Logged
    );
    assert_eq!(
        journal.note(&view((1280, 720), 509_005)),
        QualityJournalOutcome::Unchanged
    );
    assert_eq!(
        journal.note(&view((1280, 720), 1_520_000)),
        QualityJournalOutcome::Logged
    );
    assert_eq!(
        journal.note(&view((1280, 720), 1_480_000)),
        QualityJournalOutcome::Unchanged,
        "scene-to-scene changes of the same quality are not logged"
    );
    // Новая подпись снова получает свои две строки.
    assert_eq!(
        journal.note(&view((640, 360), 1_480_000)),
        QualityJournalOutcome::Logged
    );
    assert_eq!(
        journal.note(&view((640, 360), 530_000)),
        QualityJournalOutcome::Logged
    );
}
