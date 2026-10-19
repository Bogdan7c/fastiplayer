//! Сквозные тесты `IsoMp4Reader` с синтезированными файлами: проверяют отдаваемые packet-ы и seek
//! для классического и фрагментированного MP4, а не только внутренние хелперы.

use symphonia_core::formats::{FormatReader, SeekMode, SeekTo};
use symphonia_core::packet::Packet;
use symphonia_core::units::Time;

use super::test_mp4::{FragmentSpec, TrackSpec, classic_mp4, fragmented_mp4, open};

const AUDIO_RATE: u32 = 48_000;
/// 226 мс в movie timescale 1000 -> 10848 тиков при 48 кГц.
const DWELL_MS: u64 = 226;
const DWELL_TICKS: i64 = 10_848;

fn read_all(reader: &mut impl FormatReader) -> Vec<Packet> {
    let mut packets = Vec::new();
    while let Some(packet) = reader.next_packet().expect("packet читается") {
        packets.push(packet);
    }
    packets
}

fn pts_of(packets: &[Packet], track_id: u32) -> Vec<i64> {
    packets
        .iter()
        .filter(|packet| packet.track_id == track_id)
        .map(|packet| packet.pts.get())
        .collect()
}

fn ffmpeg_dwell_edits() -> Vec<(u64, i64)> {
    vec![(DWELL_MS, -1), (0, 0)]
}

#[test]
fn classic_track_with_leading_empty_edit_starts_at_dwell() {
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 3]).with_edits(ffmpeg_dwell_edits());
    let mut reader = open(classic_mp4(&[track]));
    let packets = read_all(&mut reader);

    assert_eq!(
        pts_of(&packets, 1),
        vec![DWELL_TICKS, DWELL_TICKS + 1024, DWELL_TICKS + 2048]
    );
    // DTS сдвигается так же, а payload остаётся нетронутым (packet-ы не потеряны и не подменены).
    assert_eq!(packets[0].dts.get(), DWELL_TICKS);
    assert_eq!(&packets[2].data[..2], &[1, 2]);
}

#[test]
fn classic_track_with_version_1_edit_list_is_shifted_the_same_way() {
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 2]).with_edits_v1(ffmpeg_dwell_edits());
    let packets = read_all(&mut open(classic_mp4(&[track])));
    assert_eq!(pts_of(&packets, 1), vec![DWELL_TICKS, DWELL_TICKS + 1024]);
}

#[test]
fn track_duration_is_presentation_end() {
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 4]).with_edits(ffmpeg_dwell_edits());
    let reader = open(classic_mp4(&[track]));
    let duration = reader.tracks()[0]
        .duration
        .expect("duration известна")
        .get();
    assert_eq!(duration, 4 * 1024 + DWELL_TICKS as u64);
}

#[test]
fn track_without_edit_list_keeps_media_timeline() {
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 2]);
    let mut reader = open(classic_mp4(&[track]));
    assert_eq!(pts_of(&read_all(&mut reader), 1), vec![0, 1024]);
}

#[test]
fn unsupported_edit_shape_keeps_todays_unshifted_behaviour() {
    let track =
        TrackSpec::audio(1, AUDIO_RATE, vec![1024; 2]).with_edits(vec![(100, 1024), (100, 9000)]);
    let mut reader = open(classic_mp4(&[track]));
    assert_eq!(pts_of(&read_all(&mut reader), 1), vec![0, 1024]);
}

#[test]
fn aac_priming_edit_gives_monotonic_timestamps_without_dropping_packets() {
    // Типичный AAC: media_time = 1024 (priming), первый packet получает ts -1024.
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 4]).with_edits(vec![(100_000, 1024)]);
    let packets = read_all(&mut open(classic_mp4(&[track])));
    let pts = pts_of(&packets, 1);
    assert_eq!(pts, vec![-1024, 0, 1024, 2048]);
    assert_eq!(packets.len(), 4, "ни один packet не должен быть отброшен");
    assert!(pts.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn video_media_time_shift_puts_first_presented_frame_at_zero_and_keeps_bframe_order() {
    // Порядок декодирования I P B B; media CTS = dts + offset: 1328, 4328, 2328, 3328.
    let video = TrackSpec::audio(1, 16_000, vec![1000; 4])
        .with_composition_offsets(vec![1328, 3328, 328, 328])
        .with_edits(vec![(1_000_000, 1328)]);
    let packets = read_all(&mut open(classic_mp4(&[video])));

    // Порядок декодирования сохранён, PTS сдвинуты на -M.
    assert_eq!(pts_of(&packets, 1), vec![0, 3000, 1000, 2000]);
    let dts: Vec<i64> = packets.iter().map(|packet| packet.dts.get()).collect();
    assert_eq!(dts, vec![-1328, -328, 672, 1672]);
    let mut presentation_order = pts_of(&packets, 1);
    presentation_order.sort_unstable();
    assert_eq!(presentation_order, vec![0, 1000, 2000, 3000]);
}

#[test]
fn ffmpeg_combination_video_media_time_and_audio_dwell_gives_expected_av_offset() {
    let video = TrackSpec::audio(1, 16_000, vec![1000; 2])
        .with_composition_offsets(vec![1328, 1328])
        .with_edits(vec![(1_000_000, 1328)]);
    let audio = TrackSpec::audio(2, AUDIO_RATE, vec![1024; 2]).with_edits(ffmpeg_dwell_edits());
    let mut reader = open(classic_mp4(&[video, audio]));
    let packets = read_all(&mut reader);

    let seconds = |track_id: u32| {
        let track = reader
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .expect("track");
        let first = pts_of(&packets, track_id)[0];
        track
            .time_base
            .expect("time base")
            .calc_time(first.into())
            .expect("time")
            .as_secs_f64()
    };
    assert_eq!(
        seconds(1),
        0.0,
        "видео стартует с 0 после сдвига media_time"
    );
    assert!(
        (seconds(2) - 0.226).abs() < 1e-9,
        "аудио стартует с dwell 0.226 с"
    );
}

#[test]
fn fragmented_track_with_empty_edit_starts_at_dwell_and_tfdt_adds_on_top() {
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![]).with_edits(ffmpeg_dwell_edits());
    let fragments = [
        FragmentSpec {
            track_id: 1,
            base_decode_time: 0,
            durations: vec![1024, 1024],
        },
        FragmentSpec {
            track_id: 1,
            base_decode_time: 2048,
            durations: vec![1024, 1024],
        },
    ];
    let packets = read_all(&mut open(fragmented_mp4(&[track], &fragments)));
    assert_eq!(
        pts_of(&packets, 1),
        vec![
            DWELL_TICKS,
            DWELL_TICKS + 1024,
            DWELL_TICKS + 2048,
            DWELL_TICKS + 3072
        ]
    );
}

#[test]
fn seek_inside_dwell_lands_on_first_sample() {
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 5]).with_edits(ffmpeg_dwell_edits());
    let mut reader = open(classic_mp4(&[track]));
    // Читаем немного вперёд, чтобы seek действительно вернул позицию назад.
    reader.next_packet().expect("packet");
    reader.next_packet().expect("packet");

    let seeked = reader
        .seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::ZERO,
                track_id: Some(1),
            },
        )
        .expect("seek в dwell");
    assert_eq!(seeked.actual_ts.get(), DWELL_TICKS);
    let packet = reader.next_packet().expect("packet").expect("есть packet");
    assert_eq!((packet.pts.get(), packet.data[1]), (DWELL_TICKS, 0));
}

#[test]
fn seek_to_presentation_time_maps_to_media_time_in_classic_and_fragmented_files() {
    let target = DWELL_TICKS + 3 * 1024;
    let track = TrackSpec::audio(1, AUDIO_RATE, vec![1024; 6]).with_edits(ffmpeg_dwell_edits());
    let mut classic = open(classic_mp4(std::slice::from_ref(&track)));
    let seeked = classic
        .seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: target.into(),
                track_id: 1,
            },
        )
        .expect("classic seek");
    assert_eq!(seeked.actual_ts.get(), target);
    assert_eq!(
        classic.next_packet().expect("packet").expect("есть").data[1],
        3
    );

    let fragments = [
        FragmentSpec {
            track_id: 1,
            base_decode_time: 0,
            durations: vec![1024; 2],
        },
        FragmentSpec {
            track_id: 1,
            base_decode_time: 2048,
            durations: vec![1024; 2],
        },
        FragmentSpec {
            track_id: 1,
            base_decode_time: 4096,
            durations: vec![1024; 2],
        },
    ];
    let mut fragmented = open(fragmented_mp4(&[track], &fragments));
    let seeked = fragmented
        .seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: target.into(),
                track_id: 1,
            },
        )
        .expect("fragmented seek");
    assert_eq!(seeked.actual_ts.get(), target);
    let packet = fragmented.next_packet().expect("packet").expect("есть");
    // Media 3072 = второй sample второго фрагмента.
    assert_eq!((packet.pts.get(), packet.data[1]), (target, 1));
}

#[test]
fn audio_priming_preroll_does_not_overtake_the_first_video_packet() {
    // Priming-пакет аудио (ts -1024) и первый кадр видео (ts 0) — одно «время ноль» для порядка
    // чтения: видео идёт первым, как и до поддержки `elst`. Иначе DASH decode-anchor scan
    // отбрасывал бы priming-пакет, а аудио теряло бы первый кадр.
    let video = TrackSpec::audio(1, AUDIO_RATE, vec![2048; 2]);
    let audio = TrackSpec::audio(2, AUDIO_RATE, vec![1024; 4]).with_edits(vec![(100_000, 1024)]);
    let packets = read_all(&mut open(classic_mp4(&[video, audio])));

    assert_eq!(packets[0].track_id, 1, "video keyframe is read first");
    assert_eq!(pts_of(&packets, 2), vec![-1024, 0, 1024, 2048]);
}
