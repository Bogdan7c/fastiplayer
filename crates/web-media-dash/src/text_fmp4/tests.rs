//! Тесты читателя текстовых fMP4: боксы собираются вручную, чтобы проверять границы.

use super::*;
use crate::text_fmp4::{TextSampleFormat, TextTrackInit, parse_text_mp4};

/// Обычный бокс.
fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(payload.len() + 8).unwrap().to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(payload);
    bytes
}

/// FullBox: версия, флаги и тело.
fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![version];
    payload.extend_from_slice(&flags.to_be_bytes()[1..]);
    payload.extend_from_slice(body);
    mp4_box(kind, &payload)
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.iter().flatten().copied().collect()
}

/// Инициализация с одной дорожкой `entry` (`stpp`/`wvtt`/`avc1`).
fn init_segment(entry: &[u8; 4], track_id: u32, timescale: u32, trex_duration: u32) -> Vec<u8> {
    init_segment_with_trex_size(entry, track_id, timescale, trex_duration, 0)
}

/// Как [`init_segment`], но с `default_sample_size` в `trex`.
fn init_segment_with_trex_size(
    entry: &[u8; 4],
    track_id: u32,
    timescale: u32,
    trex_duration: u32,
    trex_size: u32,
) -> Vec<u8> {
    let mut tkhd_body = vec![0; 8];
    tkhd_body.extend_from_slice(&track_id.to_be_bytes());
    let tkhd = full_box(b"tkhd", 0, 3, &tkhd_body);
    let mut mdhd_body = vec![0; 8];
    mdhd_body.extend_from_slice(&timescale.to_be_bytes());
    mdhd_body.extend_from_slice(&[0; 8]);
    let mdhd = full_box(b"mdhd", 0, 0, &mdhd_body);
    // Запись stsd: заголовок бокса + 8 байт SampleEntry (reserved + data_reference_index).
    let sample_entry = mp4_box(entry, &[0, 0, 0, 0, 0, 0, 0, 1]);
    let mut stsd_body = 1_u32.to_be_bytes().to_vec();
    stsd_body.extend_from_slice(&sample_entry);
    let stsd = full_box(b"stsd", 0, 0, &stsd_body);
    let stbl = mp4_box(b"stbl", &stsd);
    let minf = mp4_box(b"minf", &stbl);
    let mdia = mp4_box(b"mdia", &concat(&[mdhd, minf]));
    let trak = mp4_box(b"trak", &concat(&[tkhd, mdia]));
    let mut trex_body = track_id.to_be_bytes().to_vec();
    trex_body.extend_from_slice(&1_u32.to_be_bytes());
    trex_body.extend_from_slice(&trex_duration.to_be_bytes());
    trex_body.extend_from_slice(&trex_size.to_be_bytes());
    trex_body.extend_from_slice(&[0; 4]);
    let trex = full_box(b"trex", 0, 0, &trex_body);
    let mvex = mp4_box(b"mvex", &trex);
    let moov = mp4_box(b"moov", &concat(&[trak, mvex]));
    concat(&[mp4_box(b"ftyp", b"iso6\0\0\0\0"), moov])
}

/// Описание одного сэмпла для сборки `trun`.
struct SampleSpec<'a> {
    duration: Option<u32>,
    data: &'a [u8],
}

/// Фрагмент `moof`+`mdat`; `tfdt` версии 1; размеры сэмплов всегда явные.
fn media_segment(track_id: u32, tfdt: u64, samples: &[SampleSpec<'_>]) -> Vec<u8> {
    build_media_segment(track_id, Some(tfdt), None, samples)
}

fn build_media_segment(
    track_id: u32,
    tfdt: Option<u64>,
    tfhd_default_duration: Option<u32>,
    samples: &[SampleSpec<'_>],
) -> Vec<u8> {
    let with_offset = |data_offset: i32| -> Vec<u8> {
        let mut tfhd_flags = 0x2_0000_u32; // default-base-is-moof
        let mut tfhd_body = track_id.to_be_bytes().to_vec();
        if let Some(duration) = tfhd_default_duration {
            tfhd_flags |= 0x08;
            tfhd_body.extend_from_slice(&duration.to_be_bytes());
        }
        let tfhd = full_box(b"tfhd", 0, tfhd_flags, &tfhd_body);
        let mut parts = vec![tfhd];
        if let Some(tfdt) = tfdt {
            parts.push(full_box(b"tfdt", 1, 0, &tfdt.to_be_bytes()));
        }
        let has_durations = samples.iter().all(|sample| sample.duration.is_some());
        let mut trun_flags = 0x1 | 0x200;
        if has_durations {
            trun_flags |= 0x100;
        }
        let mut trun_body = u32::try_from(samples.len()).unwrap().to_be_bytes().to_vec();
        trun_body.extend_from_slice(&data_offset.to_be_bytes());
        for sample in samples {
            if has_durations {
                trun_body.extend_from_slice(&sample.duration.unwrap().to_be_bytes());
            }
            trun_body.extend_from_slice(&u32::try_from(sample.data.len()).unwrap().to_be_bytes());
        }
        parts.push(full_box(b"trun", 0, trun_flags, &trun_body));
        let traf = mp4_box(b"traf", &concat(&parts));
        let mfhd = full_box(b"mfhd", 0, 0, &1_u32.to_be_bytes());
        mp4_box(b"moof", &concat(&[mfhd, traf]))
    };
    let moof_size = with_offset(0).len();
    let moof = with_offset(i32::try_from(moof_size + 8).unwrap());
    let mdat_payload: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.data.to_vec())
        .collect();
    concat(&[moof, mp4_box(b"mdat", &mdat_payload)])
}

fn stpp_init() -> TextTrackInit {
    let parsed = parse_text_mp4(&init_segment(b"stpp", 7, 1000, 0), None).unwrap();
    parsed.init.unwrap()
}

#[test]
fn init_segment_yields_timescale_track_id_and_format() {
    let content = parse_text_mp4(&init_segment(b"wvtt", 3, 90_000, 4_000), None).unwrap();
    let init = content.init.expect("moov present");
    assert_eq!(init.track_id, 3);
    assert_eq!(init.timescale.get(), 90_000);
    assert_eq!(init.format, TextSampleFormat::Wvtt);
    assert_eq!(init.default_sample_duration, Some(4_000));
    assert!(content.samples.is_empty());

    let stpp = parse_text_mp4(&init_segment(b"stpp", 1, 1000, 0), None).unwrap();
    let init = stpp.init.unwrap();
    assert_eq!(init.format, TextSampleFormat::Stpp);
    assert_eq!(
        init.default_sample_duration, None,
        "trex duration 0 means not set"
    );
}

#[test]
fn media_segment_with_nonzero_tfdt_yields_samples_with_running_decode_time() {
    let init = stpp_init();
    let segment = media_segment(
        7,
        90_000,
        &[
            SampleSpec {
                duration: Some(2000),
                data: b"<tt>one</tt>",
            },
            SampleSpec {
                duration: Some(3000),
                data: b"<tt>two</tt>",
            },
        ],
    );
    let content = parse_text_mp4(&segment, Some(&init)).unwrap();
    assert!(content.init.is_none());
    let times: Vec<(u64, u64)> = content
        .samples
        .iter()
        .map(|sample| (sample.decode_time, sample.duration))
        .collect();
    assert_eq!(times, vec![(90_000, 2000), (92_000, 3000)]);
    assert_eq!(content.samples[0].data, b"<tt>one</tt>");
    assert_eq!(content.samples[1].data, b"<tt>two</tt>");
}

#[test]
fn init_and_fragments_in_one_file_are_read_together() {
    let file = concat(&[
        init_segment(b"stpp", 7, 1000, 0),
        media_segment(
            7,
            0,
            &[SampleSpec {
                duration: Some(1000),
                data: b"a",
            }],
        ),
        media_segment(
            7,
            1000,
            &[SampleSpec {
                duration: Some(1000),
                data: b"b",
            }],
        ),
    ]);
    let content = parse_text_mp4(&file, None).unwrap();
    assert!(content.init.is_some());
    let decode_times: Vec<u64> = content
        .samples
        .iter()
        .map(|sample| sample.decode_time)
        .collect();
    assert_eq!(decode_times, vec![0, 1000]);
    assert_eq!(content.samples[1].data, b"b");
}

#[test]
fn durations_fall_back_to_tfhd_then_trex_and_missing_everywhere_is_an_error() {
    let segment = build_media_segment(
        7,
        Some(500),
        Some(1500),
        &[SampleSpec {
            duration: None,
            data: b"x",
        }],
    );
    let content = parse_text_mp4(&segment, Some(&stpp_init())).unwrap();
    assert_eq!(
        content.samples[0].duration, 1500,
        "tfhd default wins over absence"
    );

    let init_with_trex = parse_text_mp4(&init_segment(b"stpp", 7, 1000, 2500), None)
        .unwrap()
        .init
        .unwrap();
    let from_trex = build_media_segment(
        7,
        Some(0),
        None,
        &[SampleSpec {
            duration: None,
            data: b"x",
        }],
    );
    assert_eq!(
        parse_text_mp4(&from_trex, Some(&init_with_trex))
            .unwrap()
            .samples[0]
            .duration,
        2500
    );

    assert_eq!(
        parse_text_mp4(&from_trex, Some(&stpp_init())),
        Err(TextFmp4Error::MissingSampleDuration)
    );
}

#[test]
fn missing_tfdt_starts_at_zero() {
    let segment = build_media_segment(
        7,
        None,
        None,
        &[SampleSpec {
            duration: Some(10),
            data: b"x",
        }],
    );
    let content = parse_text_mp4(&segment, Some(&stpp_init())).unwrap();
    assert_eq!(content.samples[0].decode_time, 0);
}

#[test]
fn traf_of_another_track_is_ignored() {
    let segment = media_segment(
        99,
        0,
        &[SampleSpec {
            duration: Some(10),
            data: b"x",
        }],
    );
    let content = parse_text_mp4(&segment, Some(&stpp_init())).unwrap();
    assert!(content.samples.is_empty());
}

#[test]
fn malformed_input_is_a_typed_error_not_a_panic() {
    let init = stpp_init();
    let good = media_segment(
        7,
        0,
        &[SampleSpec {
            duration: Some(10),
            data: b"payload",
        }],
    );
    // Обрезанный mdat: сэмпл выходит за пределы буфера.
    let cut = &good[..good.len() - 3];
    assert!(matches!(
        parse_text_mp4(cut, Some(&init)),
        Err(TextFmp4Error::Truncated | TextFmp4Error::SampleOutOfBounds)
    ));
    // Размер бокса меньше заголовка.
    let mut bad_size = good.clone();
    bad_size[..4].copy_from_slice(&4_u32.to_be_bytes());
    assert_eq!(
        parse_text_mp4(&bad_size, Some(&init)),
        Err(TextFmp4Error::InvalidBox)
    );
    // Фрагмент без инициализации.
    assert_eq!(parse_text_mp4(&good, None), Err(TextFmp4Error::MissingInit));
    // Нет moof вообще.
    assert_eq!(
        parse_text_mp4(&mp4_box(b"free", b"zz"), Some(&init)),
        Err(TextFmp4Error::UnsupportedLayout)
    );
    // moov без текстовой дорожки.
    assert_eq!(
        parse_text_mp4(&init_segment(b"avc1", 1, 1000, 0), None),
        Err(TextFmp4Error::NoTextTrack)
    );
    // Нулевой таймскейл.
    assert_eq!(
        parse_text_mp4(&init_segment(b"stpp", 1, 0, 0), None),
        Err(TextFmp4Error::ZeroTimescale)
    );
    // Пустой ввод и мусор.
    assert_eq!(
        parse_text_mp4(&[], Some(&init)),
        Err(TextFmp4Error::UnsupportedLayout)
    );
    assert!(parse_text_mp4(&[0xFF; 5], Some(&init)).is_err());
}

#[test]
fn absurd_sample_count_is_rejected_before_allocation() {
    let init = stpp_init();
    let mut segment = media_segment(
        7,
        0,
        &[SampleSpec {
            duration: Some(1),
            data: b"x",
        }],
    );
    // sample_count лежит в начале тела trun; найдём бокс trun и испортим счётчик.
    let position = segment
        .windows(4)
        .position(|window| window == b"trun")
        .unwrap();
    let count_at = position + 4 + 4; // тип, версия/флаги
    segment[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        parse_text_mp4(&segment, Some(&init)),
        Err(TextFmp4Error::TooManySamples)
    );
}

#[test]
fn wvtt_samples_become_webvtt_with_identifier_settings_and_media_times() {
    let vttc = |parts: &[(&[u8; 4], &str)]| -> Vec<u8> {
        let children: Vec<u8> = parts
            .iter()
            .flat_map(|(kind, text)| mp4_box(kind, text.as_bytes()))
            .collect();
        mp4_box(b"vttc", &children)
    };
    let first = vttc(&[
        (b"iden", "cue-1"),
        (b"sttg", "line:90% align:start"),
        (b"payl", "Hello <b>world</b>"),
    ]);
    let empty = mp4_box(b"vtte", &[]);
    let second = vttc(&[(b"payl", "multi\n\nline --> arrow")]);
    let init = parse_text_mp4(&init_segment(b"wvtt", 7, 1000, 0), None)
        .unwrap()
        .init
        .unwrap();
    let segment = media_segment(
        7,
        5_000,
        &[
            SampleSpec {
                duration: Some(2000),
                data: &first,
            },
            SampleSpec {
                duration: Some(1000),
                data: &empty,
            },
            SampleSpec {
                duration: Some(1500),
                data: &second,
            },
        ],
    );
    let content = parse_text_mp4(&segment, Some(&init)).unwrap();
    let document = webvtt_document_from_samples(&content.samples, init.timescale.get()).unwrap();
    assert_eq!(
        document,
        "WEBVTT\n\ncue-1\n00:00:05.000 --> 00:00:07.000 line:90% align:start\nHello <b>world</b>\n\n\
         00:00:08.000 --> 00:00:09.500\nmulti\nline --&gt; arrow\n\n"
    );
}

#[test]
fn timestamps_use_more_than_two_hour_digits_and_floor_to_milliseconds() {
    let samples = [TextSample {
        decode_time: 100 * 3600 * 90_000 + 45_000,
        duration: 90_000,
        data: mp4_box(b"vttc", &mp4_box(b"payl", b"late")),
    }];
    let document = webvtt_document_from_samples(&samples, 90_000).unwrap();
    assert!(
        document.contains("100:00:00.500 --> 100:00:01.500\nlate"),
        "{document}"
    );
}

/// `moof` из `trun_count` одинаковых `trun`, каждый указывает на ОДИН И ТОТ ЖЕ диапазон `mdat`.
fn overlapping_runs_segment(trun_count: usize, samples_per_run: u32, sample_size: u32) -> Vec<u8> {
    let build = |data_offset: i32| -> Vec<u8> {
        let tfhd = full_box(b"tfhd", 0, 0x2_0000, &7_u32.to_be_bytes());
        let tfdt = full_box(b"tfdt", 1, 0, &0_u64.to_be_bytes());
        let mut parts = vec![tfhd, tfdt];
        for _ in 0..trun_count {
            let mut body = samples_per_run.to_be_bytes().to_vec();
            body.extend_from_slice(&data_offset.to_be_bytes());
            for _ in 0..samples_per_run {
                body.extend_from_slice(&1000_u32.to_be_bytes());
                body.extend_from_slice(&sample_size.to_be_bytes());
            }
            // Явный data_offset у каждого trun: все они смотрят в начало mdat.
            parts.push(full_box(b"trun", 0, 0x1 | 0x100 | 0x200, &body));
        }
        let traf = mp4_box(b"traf", &concat(&parts));
        mp4_box(b"moof", &traf)
    };
    let moof_len = build(0).len();
    let moof = build(i32::try_from(moof_len + 8).unwrap());
    // mdat вмещает ровно ОДИН trun: остальные могут лишь повторно ссылаться на те же байты.
    let run_bytes = usize::try_from(sample_size * samples_per_run).unwrap();
    concat(&[moof, mp4_box(b"mdat", &vec![b'x'; run_bytes])])
}

#[test]
fn many_truns_over_the_same_mdat_range_are_rejected_not_amplified() {
    let init = stpp_init();
    // 1000 trun по 100 сэмплов по 1 КиБ на один и тот же килобайт mdat: без защиты
    // получилось бы ~100 МиБ копий из сегмента в ~1 КиБ.
    let segment = overlapping_runs_segment(1000, 100, 1024);
    assert_eq!(
        parse_text_mp4(&segment, Some(&init)),
        Err(TextFmp4Error::OverlappingSamples)
    );
}

#[test]
fn contiguous_samples_still_parse_and_total_bytes_stay_within_segment() {
    let init = stpp_init();
    let segment = overlapping_runs_segment(1, 1, 64);
    let content = parse_text_mp4(&segment, Some(&init)).unwrap();
    let total: usize = content.samples.iter().map(|sample| sample.data.len()).sum();
    assert_eq!(content.samples.len(), 1);
    assert!(total <= segment.len());
}

#[test]
fn trex_default_sample_size_is_used_when_tfhd_and_trun_give_none() {
    let init_bytes = init_segment_with_trex_size(b"stpp", 7, 1000, 0, 5);
    let init = parse_text_mp4(&init_bytes, None).unwrap().init.unwrap();
    assert_eq!(init.default_sample_size, Some(5));
    // trun без размеров (флаг только data_offset + duration), размер берётся из trex.
    let build = |data_offset: i32| -> Vec<u8> {
        let tfhd = full_box(b"tfhd", 0, 0x2_0000, &7_u32.to_be_bytes());
        let mut body = 2_u32.to_be_bytes().to_vec();
        body.extend_from_slice(&data_offset.to_be_bytes());
        for _ in 0..2 {
            body.extend_from_slice(&1000_u32.to_be_bytes());
        }
        let trun = full_box(b"trun", 0, 0x1 | 0x100, &body);
        mp4_box(b"moof", &mp4_box(b"traf", &concat(&[tfhd, trun])))
    };
    let moof_len = build(0).len();
    let moof = build(i32::try_from(moof_len + 8).unwrap());
    let segment = concat(&[moof, mp4_box(b"mdat", b"AAAAABBBBB")]);
    let content = parse_text_mp4(&segment, Some(&init)).unwrap();
    let payloads: Vec<&[u8]> = content.samples.iter().map(|s| s.data.as_slice()).collect();
    assert_eq!(payloads, vec![b"AAAAA".as_slice(), b"BBBBB".as_slice()]);
}

#[test]
fn missing_sample_size_everywhere_is_a_typed_error() {
    let init = stpp_init();
    let build = |data_offset: i32| -> Vec<u8> {
        let tfhd = full_box(b"tfhd", 0, 0x2_0000, &7_u32.to_be_bytes());
        let mut body = 1_u32.to_be_bytes().to_vec();
        body.extend_from_slice(&data_offset.to_be_bytes());
        body.extend_from_slice(&1000_u32.to_be_bytes());
        let trun = full_box(b"trun", 0, 0x1 | 0x100, &body);
        mp4_box(b"moof", &mp4_box(b"traf", &concat(&[tfhd, trun])))
    };
    let moof_len = build(0).len();
    let segment = concat(&[
        build(i32::try_from(moof_len + 8).unwrap()),
        mp4_box(b"mdat", b"x"),
    ]);
    assert_eq!(
        parse_text_mp4(&segment, Some(&init)),
        Err(TextFmp4Error::MissingSampleSize)
    );
}
