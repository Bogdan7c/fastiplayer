//! Seekable вход играет с самого начала: topology probe и bounded initial index
//! читают первые PES, но после них чтение обязано вернуться к началу данных.
//! Раньше playback продолжался с конца probe-а, и первые video RAP / аудиокадры
//! молча терялись (короткий файл не отдавал ни одного пакета).

use std::collections::BTreeMap;

use super::*;

/// Muxed H.264 + AAC: `count` секундных AU и по ADTS-кадру на каждый.
fn muxed_fixture(count: u64) -> Vec<u8> {
    let access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
        0x65, 0x88,
    ];
    let mut builder = TsFixtureBuilder::new().pat(&[(1, PMT_PID)], 0).pmt(
        PMT_PID,
        1,
        &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)],
        0,
    );
    for index in 0..count {
        let pts = index * 90_000;
        builder = builder.pes(VIDEO_PID, pts, None, &access_unit).pes(
            AUDIO_PID,
            pts,
            None,
            &adts_frame(&[0x11, 0x22]),
        );
    }
    builder.finish()
}

/// (pts, длина) пакетов каждой дорожки по порядку, до EOS.
///
/// Сравниваем по дорожкам: порядок между дорожками при финальном flush-е на EOF
/// не определён (pending PES разных PID), а для player-а важен только порядок
/// внутри дорожки.
fn packet_trace(demuxer: &mut MpegTsDemuxer) -> BTreeMap<u32, Vec<(Duration, usize)>> {
    let mut trace: BTreeMap<u32, Vec<(Duration, usize)>> = BTreeMap::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) => {
                trace
                    .entry(packet.track_id.get())
                    .or_default()
                    .push((packet.pts, packet.data.len()));
            }
            DemuxReadEvent::EndOfStream => return trace,
            _ => {}
        }
    }
}

fn open_seekable(bytes: &[u8], options: MpegTsDemuxOptions) -> MpegTsDemuxer {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("playback-start.ts");
    std::fs::write(&path, bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("open seekable TS")
}

fn open_forward_only(bytes: &[u8]) -> MpegTsDemuxer {
    MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(std::io::Cursor::new(bytes.to_vec()))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open forward-only TS")
}

/// Весь файл внутри окна probe/index: seekable вход отдаёт ровно то же, что
/// forward-only, начиная с первого keyframe-а и первого аудиокадра.
#[test]
fn seekable_input_replays_bytes_consumed_by_probe_and_index() {
    let bytes = muxed_fixture(3);
    let reference = packet_trace(&mut open_forward_only(&bytes));
    assert_eq!(reference.len(), 2, "video and audio tracks");
    assert!(
        reference.values().all(|packets| packets.len() == 3),
        "3 AU + 3 ADTS frames"
    );

    let seekable = packet_trace(&mut open_seekable(&bytes, MpegTsDemuxOptions::default()));

    assert_eq!(seekable, reference);
    for packets in seekable.values() {
        assert_eq!(
            packets[0].0,
            Duration::ZERO,
            "each track starts at its 0 s frame"
        );
    }
}

/// Initial index окно меньше файла: probe + частичный scan тоже не съедают начало.
#[test]
fn seekable_input_with_partial_initial_index_still_starts_at_zero() {
    let bytes = muxed_fixture(8);
    let reference = packet_trace(&mut open_forward_only(&bytes));
    let options = MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(4, "seek_scan_packets").expect("limit"),
        ..MpegTsDemuxOptions::default()
    };

    let seekable = packet_trace(&mut open_seekable(&bytes, options));

    assert_eq!(seekable, reference);
}
