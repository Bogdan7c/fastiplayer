#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозные тесты `X-TIMESTAMP-MAP` (HLS WebVTT): текст сегмента -> `load_text_subtitle` ->
//! карта времени в документе; затем сдвиг событий на шкалу плеера и поиск реплики в шкале.

use subtitle_core::{
    SubtitleContent, SubtitleEvent, SubtitleTime, SubtitleTimeShift, SubtitleTimeline,
};
use subtitle_formats::{
    LegacyEncodingHint, LoadedTextSubtitle, MpegTsTimestamp90kHz, ParseIssueKind,
    SubtitleParseLimits, TextSubtitleFormat, WebVttTimestampMap, load_text_subtitle,
};

/// Щедрые лимиты: тесты лимитов живут в других файлах.
const ROOMY_LIMITS: SubtitleParseLimits = SubtitleParseLimits::new(1 << 20, 10_000, 100);

fn try_load_text(
    format: TextSubtitleFormat,
    bytes: &[u8],
    hint: LegacyEncodingHint,
    limits: &SubtitleParseLimits,
) -> Result<LoadedTextSubtitle, subtitle_formats::SubtitleLoadError> {
    load_text_subtitle(bytes, format, hint, limits)
}

fn load_vtt(text: &str) -> LoadedTextSubtitle {
    try_load_text(
        TextSubtitleFormat::WebVtt,
        text.as_bytes(),
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
    .unwrap()
}

fn map_of(loaded: &LoadedTextSubtitle) -> Option<WebVttTimestampMap> {
    loaded.document().webvtt_timestamp_map()
}

fn timestamp_map_issue_lines(loaded: &LoadedTextSubtitle) -> Vec<u32> {
    loaded
        .document()
        .report()
        .issues()
        .iter()
        .filter(|issue| issue.kind == ParseIssueKind::MalformedTimestampMap)
        .map(|issue| issue.line_number.get())
        .collect()
}

fn assert_cue_survived(loaded: &LoadedTextSubtitle) {
    let events = loaded.document().events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].start(), SubtitleTime::from_millis(1_000));
}

const HLS_SEGMENT: &str = "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:900000\n\n00:00:01.000 --> 00:00:02.000\nHello\n";

#[test]
fn hls_style_header_gives_map_and_keeps_cues() {
    let loaded = load_vtt(HLS_SEGMENT);
    let map = map_of(&loaded).expect("map present");
    assert_eq!(map.mpegts.ticks(), 900_000);
    assert_eq!(map.local, SubtitleTime::ZERO);
    assert_cue_survived(&loaded);
    assert!(timestamp_map_issue_lines(&loaded).is_empty());
}

#[test]
fn parameter_order_and_other_header_lines_do_not_matter() {
    let reversed = load_vtt(
        "\u{feff}WEBVTT\r\nKind: captions\r\nX-TIMESTAMP-MAP=MPEGTS:180000,LOCAL:00:00:10.500\r\n\r\n00:00:01.000 --> 00:00:02.000\r\nHello\r\n",
    );
    let map = map_of(&reversed).expect("map present");
    assert_eq!(map.mpegts, MpegTsTimestamp90kHz::new(180_000).unwrap());
    assert_eq!(map.local, SubtitleTime::from_millis(10_500));
    assert_cue_survived(&reversed);
}

#[test]
fn malformed_headers_are_reported_with_line_number_and_never_fatal() {
    let too_big = format!(
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:{},LOCAL:00:00.000\n\n00:01.000 --> 00:02.000\nHello\n",
        1u64 << 33
    );
    let headers = [
        // Нет LOCAL.
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000\n\n00:01.000 --> 00:02.000\nHello\n".to_owned(),
        // Нет MPEGTS.
        "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00.000\n\n00:01.000 --> 00:02.000\nHello\n".to_owned(),
        // Мусор.
        "WEBVTT\nX-TIMESTAMP-MAP=whatever\n\n00:01.000 --> 00:02.000\nHello\n".to_owned(),
        // MPEGTS > 2^33 - 1.
        too_big,
        // Не число.
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:abc,LOCAL:00:00.000\n\n00:01.000 --> 00:02.000\nHello\n"
            .to_owned(),
    ];
    for text in headers {
        let loaded = load_vtt(&text);
        assert_eq!(map_of(&loaded), None, "{text}");
        assert_eq!(timestamp_map_issue_lines(&loaded), [2], "{text}");
        assert_cue_survived(&loaded);
    }
}

#[test]
fn second_map_line_is_reported_and_first_valid_wins() {
    let loaded = load_vtt(
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:90000,LOCAL:00:00.000\nX-TIMESTAMP-MAP=MPEGTS:1,LOCAL:00:00.000\n\n00:01.000 --> 00:02.000\nHello\n",
    );
    assert_eq!(map_of(&loaded).unwrap().mpegts.ticks(), 90_000);
    assert_eq!(timestamp_map_issue_lines(&loaded), [3]);
}

#[test]
fn map_line_after_first_blank_line_is_not_a_header() {
    let loaded = load_vtt(
        "WEBVTT\n\nNOTE x\n\nX-TIMESTAMP-MAP=MPEGTS:90000,LOCAL:00:00.000\n\n00:01.000 --> 00:02.000\nHello\n",
    );
    assert_eq!(map_of(&loaded), None);
    assert!(timestamp_map_issue_lines(&loaded).is_empty());
}

#[test]
fn file_without_header_and_srt_have_no_map() {
    assert_eq!(
        map_of(&load_vtt("WEBVTT\n\n00:01.000 --> 00:02.000\nHello\n")),
        None
    );
    let srt = try_load_text(
        TextSubtitleFormat::Srt,
        b"1\n00:00:01,000 --> 00:00:02,000\nHello\n",
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS,
    )
    .unwrap();
    assert_eq!(map_of(&srt), None);
}

/// Как будет использоваться вызывающим кодом: сдвиг событий сегмента и поиск в шкале.
#[test]
fn shifted_events_of_loaded_segment_are_found_at_player_time() {
    let loaded = load_vtt(
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00.000\n\n00:00:01.000 --> 00:00:02.000\n<00:00:01.500>Hi\n",
    );
    let map = map_of(&loaded).unwrap();
    // Сегмент на 10 с шкалы (900000 / 90000), реплика на локальной 1 с -> игрок видит её на 11 с.
    let shift = SubtitleTimeShift::from_micros(map.mpegts.as_micros() - map.local.as_micros());
    let shifted: Vec<SubtitleEvent> = loaded
        .document()
        .events()
        .iter()
        .map(|event| event.shifted_by(shift).unwrap())
        .collect();
    let (timeline, _) = SubtitleTimeline::from_events(shifted);
    let mut active = Vec::new();
    timeline.collect_active_at(SubtitleTime::from_millis(1_500), &mut active);
    assert!(active.is_empty());
    timeline.collect_active_at(SubtitleTime::from_millis(11_500), &mut active);
    assert_eq!(active.len(), 1);
    let SubtitleContent::Text(cue) = timeline.event(active[0]).unwrap().content() else {
        panic!("expected text");
    };
    assert_eq!(cue.text.plain_text(), "Hi");
    assert_eq!(
        cue.text.spans()[0].reveal_at,
        Some(SubtitleTime::from_millis(11_500))
    );
}

#[test]
fn shift_leaves_ass_content_unchanged_because_its_times_are_event_relative() {
    let script = "[Script Info]\nScriptType: v4.00+\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\move(0,0,10,10,0,500)\\k50}x\n";
    let loaded = try_load_text(
        TextSubtitleFormat::Ass,
        script.as_bytes(),
        LegacyEncodingHint::Generic,
        &ROOMY_LIMITS.with_ass_limits(subtitle_formats::AssParseLimits {
            max_header_bytes: 1 << 16,
            max_styles: 32,
            max_override_tags_per_event: 64,
            max_tracked_unknown_tag_names: 8,
        }),
    )
    .unwrap();
    let original = &loaded.document().events()[0];
    let shifted = original
        .shifted_by(SubtitleTimeShift::from_millis(5_000))
        .unwrap();
    assert_eq!(shifted.start(), SubtitleTime::from_millis(6_000));
    assert_eq!(shifted.end(), SubtitleTime::from_millis(8_000));
    assert_eq!(shifted.content(), original.content());
}
