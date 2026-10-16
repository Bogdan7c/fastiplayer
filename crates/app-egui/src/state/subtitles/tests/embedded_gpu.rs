//! Встроенные дорожки до пикселей: настоящие пакеты из файлов -> сообщения трубы -> владелец ->
//! растеризатор -> настоящий GPU-проход -> readback. Тесты пропускаются (с сообщением), если на
//! машине нет GPU adapter-а, как остальные тесты цепочки субтитров.

use media_core::{MediaAttachment, MediaAttachments};
use player_core::{SubtitleFeedMediaStart, SubtitleFeedMessage};

use super::embedded_support::*;
use super::embedded_test_font::font_with_private_glyph;
use super::gpu_chain::Harness;
use super::*;

macro_rules! gpu_or_skip {
    () => {
        match Harness::new() {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the embedded subtitle test");
                return;
            }
        }
    };
}

fn picture() -> render_core::PictureRect {
    letterboxed_rect(256, 144)
}

/// Кадр «видео» + субтитры в момент `pts_ms`.
fn render_at(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    media: MediaInstanceId,
    pts_ms: u64,
) -> Vec<u8> {
    let rect = picture();
    let overlay = owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .cloned();
    harness.frame(rect, overlay.as_ref()).1
}

/// Сколько пикселей отличаются между двумя readback-ами.
fn differing_pixels(a: &[u8], b: &[u8]) -> usize {
    let (left_pixels, _) = a.as_chunks::<4>();
    let (right_pixels, _) = b.as_chunks::<4>();
    left_pixels
        .iter()
        .zip(right_pixels)
        .filter(|(left, right)| left != right)
        .count()
}

#[test]
fn embedded_srt_cue_appears_in_the_gpu_frame_only_while_it_is_active() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    owner.deliver_feed_messages(fixture.feed_messages(media));

    let inside_first = render_at(&mut owner, &mut harness, media, 1_000);
    let between = render_at(&mut owner, &mut harness, media, 1_800);
    let inside_second = render_at(&mut owner, &mut harness, media, 2_400);
    let after_all = render_at(&mut owner, &mut harness, media, 2_900);

    assert!(
        differing_pixels(&inside_first, &video_only) > 50,
        "the cue \"Hello\" paints subtitle pixels at 1.0 s"
    );
    assert_eq!(between, video_only, "no subtitle between the cues");
    assert!(differing_pixels(&inside_second, &video_only) > 50);
    assert_ne!(
        inside_first, inside_second,
        "\"Hello\" and \"Second line\" are different pictures"
    );
    assert_eq!(after_all, video_only, "cue [2.0, 2.8) is over at 2.9 s");
}

#[test]
fn switching_the_track_during_playback_changes_the_rendered_text_from_cache() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    let (_, video_only) = harness.frame(picture(), None);
    let english = render_at(&mut owner, &mut harness, media, 1_000);
    assert!(differing_pixels(&english, &video_only) > 50);
    let calls_before = owner.embedded_feed_counters();

    let russian_menu_id = owner.menu_view().tracks[1].id;
    assert_eq!(
        owner.select_track(russian_menu_id),
        SelectTrackOutcome::Selected
    );
    let russian = render_at(&mut owner, &mut harness, media, 1_000);

    assert!(
        differing_pixels(&russian, &video_only) > 50,
        "the second track shows its own cue"
    );
    assert_ne!(
        english, russian,
        "different text -> different pixels, without reopening"
    );
    assert_eq!(
        owner.embedded_feed_counters(),
        calls_before,
        "no packets were re-read: the cue came from the cache"
    );
}

#[test]
fn mp4_mov_text_cue_reaches_the_gpu_frame() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("mov-text-rus.mp4");
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    owner.deliver_feed_messages(fixture.feed_messages(media));
    // Берём первую реплику с текстом (пустые промежутки mov_text - два нулевых байта).
    let visible = fixture
        .subtitle_packets
        .iter()
        .find(|packet| packet.data.len() > 2)
        .expect("fixture has a text sample");
    let inside = u64::try_from(visible.pts.as_millis()).expect("fits") + 50;

    let pixels = render_at(&mut owner, &mut harness, media, inside);

    assert!(
        differing_pixels(&pixels, &video_only) > 50,
        "mov_text sample at {inside} ms is drawn"
    );
}

/// SRT-дорожка с приманкой: реплика с символом, который рисует только вложенный шрифт.
fn owner_with_private_glyph_cue(
    attachment: Option<Vec<u8>>,
    config: &SubtitlesConfig,
    private_char: char,
) -> (SubtitleOwner, MediaInstanceId) {
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let english = fixture.subtitle_row(0).id;
    let mut owner = embedded_owner(config, media, &fixture);
    let attachments = MediaAttachments {
        attachments: attachment
            .into_iter()
            .map(|bytes| MediaAttachment {
                file_name: "private.ttf".to_owned(),
                media_type: Some("font/ttf".to_owned()),
                bytes: bytes::Bytes::from(bytes),
            })
            .collect(),
        skipped: Vec::new(),
    };
    let has_font = !attachments.attachments.is_empty();
    owner.deliver_feed_messages(vec![
        SubtitleFeedMessage::MediaStarted(SubtitleFeedMediaStart {
            media_instance_id: media,
            attachments,
        }),
        raw_packet(
            media,
            english,
            1_000,
            Some(1_000),
            format!("x{private_char}y").as_bytes(),
        ),
    ]);
    if has_font {
        wait_until(&mut owner, "attached fonts installed", |owner| {
            !owner.attached_fonts.has_running_work()
        });
    }
    (owner, media)
}

#[test]
fn attached_font_changes_the_rendered_cue_and_media_change_removes_it() {
    let mut harness = gpu_or_skip!();
    let (font, private_char) = font_with_private_glyph();
    let config = enabled_config();
    let (mut bare, media) = owner_with_private_glyph_cue(None, &config, private_char);
    let (mut with_font, _) = owner_with_private_glyph_cue(Some(font), &config, private_char);

    let without_attachment = render_at(&mut bare, &mut harness, media, 1_500);
    let with_attachment = render_at(&mut with_font, &mut harness, media, 1_500);

    assert_ne!(
        with_attachment, without_attachment,
        "the attached glyph must change the picture"
    );

    // Смена media снимает вложенные шрифты: шрифт больше не участвует.
    assert!(matches!(
        with_font.presenter.clear_attached_fonts(),
        subtitle_raster::AttachedFontsClearOutcome::Cleared { .. }
    ));
    with_font.begin_media(instance(3), SubtitleMediaSource::NotLocal);
    assert_eq!(
        with_font.presenter.clear_attached_fonts(),
        subtitle_raster::AttachedFontsClearOutcome::NothingAttached,
        "nothing is attached after the previous media was cleared"
    );
}

#[test]
fn media_change_clears_the_attached_fonts_of_the_previous_media() {
    let (font, private_char) = font_with_private_glyph();
    let (mut owner, _) = owner_with_private_glyph_cue(Some(font), &enabled_config(), private_char);

    owner.begin_media(instance(3), SubtitleMediaSource::NotLocal);

    assert_eq!(
        owner.presenter.clear_attached_fonts(),
        subtitle_raster::AttachedFontsClearOutcome::NothingAttached,
        "begin_media already removed them"
    );
}

#[test]
fn the_real_fixture_attachment_is_prepared_and_installed() {
    let media = instance(2);
    let fixture = demux_fixture("ass-with-font.mkv");
    assert_eq!(fixture.attachments.attachments.len(), 1);
    let mut owner = embedded_owner(&enabled_config(), media, &fixture);

    owner.deliver_feed_messages(vec![fixture.media_started(media)]);
    wait_until(&mut owner, "attached fonts", |owner| {
        !owner.attached_fonts.has_running_work()
    });

    // Установленное вложение видно по тому, что его можно снять.
    let outcome = owner.presenter.clear_attached_fonts();
    assert!(
        matches!(
            outcome,
            subtitle_raster::AttachedFontsClearOutcome::Cleared { faces: 1, .. }
        ),
        "got {outcome:?}"
    );
}

#[test]
fn attachment_over_the_limit_is_skipped_and_cues_still_render() {
    let mut harness = gpu_or_skip!();
    let media = instance(2);
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let mut config = enabled_config();
    config.max_attached_font_mb = 1;
    let mut owner = embedded_owner(&config, media, &fixture);
    let (_, video_only) = harness.frame(picture(), None);
    // 2 МиБ при лимите 1 МиБ: шрифт отвергается ещё при подготовке.
    let oversized = MediaAttachments {
        attachments: vec![MediaAttachment {
            file_name: "huge.ttf".to_owned(),
            media_type: Some("font/ttf".to_owned()),
            bytes: bytes::Bytes::from(vec![0_u8; 2 * 1024 * 1024]),
        }],
        skipped: Vec::new(),
    };

    owner.deliver_feed_messages(vec![SubtitleFeedMessage::MediaStarted(
        SubtitleFeedMediaStart {
            media_instance_id: media,
            attachments: oversized,
        },
    )]);
    wait_until(&mut owner, "attached fonts job", |owner| {
        !owner.attached_fonts.has_running_work()
    });
    owner.deliver_feed_messages(fixture.feed_messages(media));

    assert_eq!(
        owner.presenter.clear_attached_fonts(),
        subtitle_raster::AttachedFontsClearOutcome::NothingAttached,
        "the oversized font was not installed"
    );
    let cue = render_at(&mut owner, &mut harness, media, 1_000);
    assert!(
        differing_pixels(&cue, &video_only) > 50,
        "cues still render with the built-in fonts"
    );
}

#[test]
fn attachments_of_a_previous_media_are_not_installed() {
    let (font, _) = font_with_private_glyph();
    let fixture = demux_fixture("two-srt-tracks.mkv");
    let mut owner = embedded_owner(&enabled_config(), instance(5), &fixture);
    let stale = MediaAttachments {
        attachments: vec![MediaAttachment {
            file_name: "old.ttf".to_owned(),
            media_type: Some("font/ttf".to_owned()),
            bytes: bytes::Bytes::from(font),
        }],
        skipped: Vec::new(),
    };

    owner.deliver_feed_messages(vec![SubtitleFeedMessage::MediaStarted(
        SubtitleFeedMediaStart {
            media_instance_id: instance(4),
            attachments: stale,
        },
    )]);

    assert!(
        !owner.attached_fonts.has_running_work(),
        "no job for a stale media"
    );
    assert_eq!(
        owner.presenter.clear_attached_fonts(),
        subtitle_raster::AttachedFontsClearOutcome::NothingAttached
    );
}
