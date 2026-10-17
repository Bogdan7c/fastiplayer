//! AC-3 / E-AC-3 в MPEG-TS на вручную собранных потоках: DVB-сигнализация,
//! разрезанные границами PES syncframe-ы, dependent substream-ы, ресинхронизация
//! на старте и fail-closed обрыв. Потеря синхронизации посреди потока и смена
//! формата — `ac3_stream_recovery.rs`.

use super::*;

/// Перед первым syncframe-ом в DVB-потоке после seek лежит хвост чужого кадра.
const FOREIGN_FRAME_TAIL: [u8; 5] = [0x13, 0x0b, 0x42, 0x99, 0x00];
/// Длительность AC-3 кадра 1536 сэмплов @ 48 кГц в тиках 90 кГц.
pub(super) const AC3_48K_FRAME_TICKS: u64 = 2_880;

/// MSB-first запись полей заголовка (независимо от кода разбора).
#[derive(Default)]
struct HeaderBits {
    bytes: Vec<u8>,
    bit_count: usize,
}

impl HeaderBits {
    fn put(&mut self, width: usize, value: u32) -> &mut Self {
        for shift in (0..width).rev() {
            if self.bit_count.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let bit = ((value >> shift) & 1) as u8;
            *self.bytes.last_mut().expect("byte pushed above") |= bit << (7 - self.bit_count % 8);
            self.bit_count += 1;
        }
        self
    }

    fn padded(&self, total_bytes: usize, fill: u8) -> Vec<u8> {
        let mut bytes = self.bytes.clone();
        bytes.resize(total_bytes, fill);
        bytes
    }
}

/// AC-3 stereo 48 кГц, frmsizecod 0 → 128 байт; `fill` различает кадры.
pub(super) fn ac3_stereo_frame(fill: u8) -> Vec<u8> {
    let mut bits = HeaderBits::default();
    bits.put(16, 0x0b77)
        .put(16, 0) // crc1
        .put(2, 0) // fscod 48 кГц
        .put(6, 0) // frmsizecod 32 кбит/с
        .put(5, 8) // bsid
        .put(3, 0) // bsmod
        .put(3, 2) // acmod 2/0
        .put(2, 0) // dsurmod
        .put(1, 0); // lfeon
    bits.padded(128, fill)
}

/// E-AC-3 syncframe 48 кГц, 6 блоков; `stream_type` 0/1, dependent несёт chanmap.
pub(super) fn eac3_frame(
    stream_type: u32,
    substream_id: u32,
    chanmap: Option<u16>,
    fill: u8,
) -> Vec<u8> {
    let frame_bytes = 64_usize;
    let mut bits = HeaderBits::default();
    bits.put(16, 0x0b77)
        .put(2, stream_type)
        .put(3, substream_id)
        .put(11, (frame_bytes / 2 - 1) as u32)
        .put(2, 0) // fscod 48 кГц
        .put(2, 3) // numblkscod: 6 блоков
        .put(3, if stream_type == 1 { 2 } else { 7 }) // acmod: 2/0 у dependent, 3/2 у основного
        .put(1, u32::from(stream_type != 1)) // lfeon у основного
        .put(5, 16) // bsid
        .put(5, 27) // dialnorm
        .put(1, 0); // compre
    if stream_type == 1 {
        match chanmap {
            Some(chanmap) => bits.put(1, 1).put(16, u32::from(chanmap)),
            None => bits.put(1, 0),
        };
    }
    bits.padded(frame_bytes, fill)
}

/// PAT + PMT с ES descriptor loop-ами (базовый builder пишет пустые loop-ы).
pub(super) fn program_with_descriptors(streams: &[(u8, u16, &[u8])]) -> TsFixtureBuilder {
    program_with_program_and_stream_descriptors(&[], streams)
}

/// PAT + PMT с program-level (`program_info`) и ES descriptor loop-ами.
pub(super) fn program_with_program_and_stream_descriptors(
    program_descriptors: &[u8],
    streams: &[(u8, u16, &[u8])],
) -> TsFixtureBuilder {
    let mut builder = TsFixtureBuilder::new().pat(&[(1, PMT_PID)], 0);
    let pcr_pid = streams.first().map_or(0x1fff, |stream| stream.1);
    let mut body = vec![
        0x02,
        0xb0,
        0x00,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0xe0 | ((pcr_pid >> 8) as u8 & 0x1f),
        pcr_pid as u8,
        0xf0 | ((program_descriptors.len() >> 8) as u8 & 0x0f),
        program_descriptors.len() as u8,
    ];
    body.extend_from_slice(program_descriptors);
    for &(stream_type, pid, descriptors) in streams {
        body.extend_from_slice(&[
            stream_type,
            0xe0 | ((pid >> 8) as u8 & 0x1f),
            pid as u8,
            0xf0 | ((descriptors.len() >> 8) as u8 & 0x0f),
            descriptors.len() as u8,
        ]);
        body.extend_from_slice(descriptors);
    }
    finalize_section(&mut body);
    let mut payload = vec![0];
    payload.extend(body);
    builder.push_payload(PMT_PID, true, false, &payload);
    builder
}

pub(super) fn open_bytes(bytes: Vec<u8>) -> Box<dyn Demuxer + Send> {
    open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("open AC-3 TS")
}

pub(super) fn audio_packets(events: &[DemuxReadEvent]) -> Vec<&media_core::Packet> {
    events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.track_id.get() == u32::from(AUDIO_PID) => {
                Some(packet)
            }
            _ => None,
        })
        .collect()
}

pub(super) fn pts_ticks(packet: &media_core::Packet) -> i64 {
    packet.track_pts.expect("track pts").units.get()
}

/// DVB: private PES 0x06 + только `AC-3_descriptor` (без registration), язык из 0x0A.
#[test]
fn dvb_private_pes_with_ac3_descriptor_becomes_labelled_ac3_track() {
    let descriptors = [0x6a, 0x01, 0x00, 0x0a, 0x04, b'd', b'e', b'u', 0x00];
    let frames = [ac3_stereo_frame(1), ac3_stereo_frame(2)].concat();
    let bytes = program_with_descriptors(&[(0x06, AUDIO_PID, &descriptors)])
        .pes(AUDIO_PID, 9_000, None, &frames)
        .finish();
    let mut demuxer = open_bytes(bytes);
    let track = &demuxer.tracks()[0];
    assert_eq!(track.codec_id, "A_AC3");
    assert_eq!(track.sample_rate, Some(48_000));
    assert_eq!(track.channels, Some(2));
    assert_eq!(
        track
            .labels
            .language
            .as_ref()
            .map(|language| language.as_str()),
        Some("deu")
    );

    let events = drain(&mut *demuxer).expect("drain AC-3");
    let packets = audio_packets(&events);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].data.as_ref(), ac3_stereo_frame(1).as_slice());
    assert_eq!(packets[1].data.as_ref(), ac3_stereo_frame(2).as_slice());
    assert_eq!(pts_ticks(packets[0]), 9_000);
    assert_eq!(pts_ticks(packets[1]), 9_000 + AC3_48K_FRAME_TICKS as i64);
}

/// Private PES без AC-3 сигнализации (teletext) остаётся неизвестным потоком,
/// а лишние/незнакомые дескрипторы рядом с AC-3 ничего не ломают.
#[test]
fn private_pes_without_ac3_signalling_is_ignored_and_unknown_descriptors_are_skipped() {
    let teletext = [0x56, 0x05, b'e', b'n', b'g', 0x09, 0x00];
    let ac3_with_noise = [
        0x52, 0x01, 0x07, 0xfe, 0x00, 0x05, 0x04, b'A', b'C', b'-', b'3',
    ];
    let bytes = program_with_descriptors(&[
        (0x06, VIDEO_PID, &teletext),
        (0x81, AUDIO_PID, &ac3_with_noise),
    ])
    .pes(VIDEO_PID, 0, None, &[0x10, 0x02, 0x2c])
    .pes(AUDIO_PID, 0, None, &ac3_stereo_frame(7))
    .finish();
    let mut demuxer = open_bytes(bytes);
    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].id.get(), u32::from(AUDIO_PID));
    let events = drain(&mut *demuxer).expect("drain");
    assert_eq!(audio_packets(&events).len(), 1);
}

/// Syncframe-ы разрезаны границами PES в произвольных местах. PTS PES-а
/// достаётся первому кадру, который *начинается* в этом PES; кадр, начатый в
/// предыдущем PES, продолжает цепочку длительностей.
///
/// Раскладка (кадры по 128 байт): PES1 [0..200) — F1 целиком и начало F2;
/// PES2 [200..300) — конец F2 и начало F3 (F3 стартует на 256); PES3 — только
/// хвост F3, его PTS не относится ни к одному кадру; PES4 — ровно F4.
#[test]
fn syncframes_split_across_pes_are_reassembled_with_carried_timestamps() {
    let frames: Vec<Vec<u8>> = (1..=4).map(ac3_stereo_frame).collect();
    let first_three = frames[..3].concat();
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 1_000, None, &first_three[..200])
        .pes(AUDIO_PID, 7_000, None, &first_three[200..300])
        .pes(AUDIO_PID, 9_999, None, &first_three[300..])
        .pes(AUDIO_PID, 50_000, None, &frames[3])
        .finish();
    let mut demuxer = open_bytes(bytes);
    let events = drain(&mut *demuxer).expect("drain split AC-3");
    let packets = audio_packets(&events);
    assert_eq!(packets.len(), 4, "every frame published exactly once");
    for (packet, frame) in packets.iter().zip(&frames) {
        assert_eq!(packet.data.as_ref(), frame.as_slice());
    }
    let pts: Vec<i64> = packets.iter().map(|packet| pts_ticks(packet)).collect();
    assert_eq!(pts, vec![1_000, 3_880, 7_000, 50_000]);
}

/// E-AC-3: dependent substream уходит в пакет основного, дополнительная
/// программа (substream 1) — нет; последний пакет закрывается концом потока.
#[test]
fn eac3_dependent_substream_is_packed_with_its_independent_frame() {
    let independent = eac3_frame(0, 0, None, 1);
    let back_pair = eac3_frame(1, 0, Some(1 << (15 - 6)), 2);
    let other_program = eac3_frame(0, 1, None, 3);
    let last = eac3_frame(0, 0, None, 4);
    let last_back_pair = eac3_frame(1, 0, Some(1 << (15 - 6)), 5);
    let stream = [
        independent.clone(),
        back_pair.clone(),
        other_program,
        last.clone(),
        last_back_pair.clone(),
    ]
    .concat();
    let bytes = program_with_descriptors(&[(0x87, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &stream)
        .finish();
    let mut demuxer = open_bytes(bytes);
    let track = &demuxer.tracks()[0];
    assert_eq!(track.codec_id, "A_EAC3");
    assert_eq!(track.channels, Some(8));
    let events = drain(&mut *demuxer).expect("drain E-AC-3");
    let packets = audio_packets(&events);
    assert_eq!(packets.len(), 2);
    assert_eq!(
        packets[0].data.as_ref(),
        [independent, back_pair].concat().as_slice()
    );
    assert_eq!(
        packets[1].data.as_ref(),
        [last, last_back_pair].concat().as_slice()
    );
    assert_eq!(pts_ticks(packets[1]), AC3_48K_FRAME_TICKS as i64);
}

/// Кодек доказывает bsid, а не stream_type: E-AC-3 под ATSC AC-3 (0x81).
#[test]
fn codec_id_comes_from_bitstream_not_from_stream_type() {
    let stream = [eac3_frame(0, 0, None, 1), eac3_frame(0, 0, None, 2)].concat();
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &stream)
        .finish();
    let demuxer = open_bytes(bytes);
    assert_eq!(demuxer.tracks()[0].codec_id, "A_EAC3");
    assert_eq!(demuxer.tracks()[0].channels, Some(6));
}

/// Поток начинается посреди syncframe-а (так бывает после seek и в live):
/// хвост чужого кадра и висящий dependent пропускаются, PTS PES-а достаётся
/// первому целому кадру.
#[test]
fn stream_starting_mid_syncframe_resynchronizes_on_first_frame() {
    let orphan_dependent = eac3_frame(1, 0, None, 9);
    let first = eac3_frame(0, 0, None, 1);
    let second = eac3_frame(0, 0, None, 2);
    let payload = [
        FOREIGN_FRAME_TAIL.to_vec(),
        orphan_dependent,
        first.clone(),
        second,
    ]
    .concat();
    let bytes = program_with_descriptors(&[(0x87, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 4_500, None, &payload)
        .finish();
    let mut demuxer = open_bytes(bytes);
    let events = drain(&mut *demuxer).expect("resync drain");
    let packets = audio_packets(&events);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].data.as_ref(), first.as_slice());
    assert_eq!(pts_ticks(packets[0]), 4_500);
}

/// Оборванный последний кадр на EOF — fatal, а не тихий конец потока.
#[test]
fn truncated_ac3_tail_at_end_of_input_is_fatal_after_complete_packets() {
    let complete_frames = [ac3_stereo_frame(1), ac3_stereo_frame(2)].concat();
    let truncated_frame = ac3_stereo_frame(3)[..50].to_vec();
    // Null-пакеты не дают initial probe дочитать до EOF: topology доказывается
    // первым пакетом, а обрыв обнаруживается уже при чтении.
    let bytes = program_with_descriptors(&[(0x81, AUDIO_PID, &[])])
        .pes(AUDIO_PID, 0, None, &complete_frames)
        .pes(AUDIO_PID, 2 * AC3_48K_FRAME_TICKS, None, &truncated_frame)
        .null_packets(5_000)
        .finish();
    let mut demuxer = open_bytes(bytes);
    let mut published = 0;
    let error = loop {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(_)) => published += 1,
            Ok(DemuxReadEvent::EndOfStream) => panic!("truncated AC-3 must not end cleanly"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    // F1 закрыт заголовком F2 ещё при чтении. F2 закрывается только концом
    // потока, и тот же шаг находит оборванный F3: fatal ошибка demuxer-а
    // возвращается раньше очереди событий (общий контракт `next_event`), так
    // что F2 не публикуется. Главное — обрыв не превращается в чистый EOF.
    assert_eq!(published, 1);
    assert!(matches!(
        error.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::Malformed { .. })
    ));
}

/// Битый ES descriptor loop не роняет PMT: ATSC AC-3 (самоописывающий
/// stream_type) играет без языка, а private PES, чей кодек известен только из
/// дескрипторов, просто не распознаётся.
#[test]
fn malformed_descriptor_loops_keep_pmt_and_self_describing_streams() {
    // Язык объявлен длиной 9 при 3 байтах тела — дескриптор вылезает за loop.
    let overrunning = [0x0a, 0x09, b'e', b'n', b'g'];
    let bytes = program_with_descriptors(&[
        (0x06, VIDEO_PID, &overrunning),
        (0x81, AUDIO_PID, &overrunning),
    ])
    .pes(VIDEO_PID, 0, None, &ac3_stereo_frame(9))
    .pes(AUDIO_PID, 0, None, &ac3_stereo_frame(1))
    .finish();
    let mut demuxer = open_bytes(bytes);
    let tracks = demuxer.tracks().to_vec();
    assert_eq!(
        tracks.len(),
        1,
        "private PES without readable signalling is skipped"
    );
    assert_eq!(tracks[0].id.get(), u32::from(AUDIO_PID));
    assert_eq!(tracks[0].codec_id, "A_AC3");
    assert_eq!(tracks[0].labels.language, None);
    let events = drain(&mut *demuxer).expect("drain");
    assert_eq!(audio_packets(&events).len(), 1);
}

/// Blu-ray: program registration `HDMV` делает stream_type 0x84 основным
/// Dolby Digital Plus; тот же 0x84 в обычном TS без сигнализации — чужой поток.
#[test]
fn blu_ray_hdmv_program_maps_stream_type_0x84_to_ac3_family() {
    let hdmv_registration = *b"\x05\x04HDMV";
    let blu_ray =
        program_with_program_and_stream_descriptors(&hdmv_registration, &[(0x84, AUDIO_PID, &[])])
            .pes(AUDIO_PID, 0, None, &ac3_stereo_frame(1))
            .finish();
    let demuxer = open_bytes(blu_ray);
    assert_eq!(demuxer.tracks()[0].codec_id, "A_AC3");

    let plain = program_with_descriptors(&[(0x84, VIDEO_PID, &[]), (0x81, AUDIO_PID, &[])])
        .pes(VIDEO_PID, 0, None, &ac3_stereo_frame(2))
        .pes(AUDIO_PID, 0, None, &ac3_stereo_frame(1))
        .finish();
    let demuxer = open_bytes(plain);
    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].id.get(), u32::from(AUDIO_PID));
}
