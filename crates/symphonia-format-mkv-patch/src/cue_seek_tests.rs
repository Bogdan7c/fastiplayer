// Fastiplayer: функциональные тесты seek-а по Cues на синтетическом Matroska-потоке.
//
// Поток строится так же, как это делает ffmpeg: Cues с `CueTime` в единицах TimestampScale
// (здесь 1 мс) и `CueRelativePosition`, указывающим на блок внутри кластера (мимо элемента
// `Cluster/Timestamp`). Как в реальном A/V MKV, опорный блок cue — НЕ первый в кластере, поэтому
// CueTime больше timestamp-а кластера: подстановка CueTime вместо `Cluster/Timestamp` сдвинула бы
// pts. Payload каждого кадра — его настоящее время в мс текстом, поэтому тест проверяет не только
// pts, но и что под этим pts лежит правильный кадр.

use std::io::Cursor;

use symphonia_core::formats::prelude::*;
use symphonia_core::io::MediaSourceStream;

use crate::MkvReader;

/// Номер (и Symphonia track id) единственной дорожки.
const TRACK_NUMBER: u32 = 1;
/// Длительность кадра: 100 мс (DefaultDuration в наносекундах).
const FRAME_DURATION_MS: i64 = 100;
/// Кадров в кластере: кластер длится одну секунду.
const FRAMES_PER_CLUSTER: i64 = 10;

/// Кодирует EBML-элемент: id + 8-байтный size vint + payload.
fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = id.to_vec();
    bytes.push(0x01);
    bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
    bytes.extend_from_slice(payload);
    bytes
}

/// Беззнаковый элемент фиксированной ширины 8 байт: длина не зависит от значения.
fn uint(id: &[u8], value: u64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

fn text(id: &[u8], value: &str) -> Vec<u8> {
    element(id, value.as_bytes())
}

/// Payload кадра: его медиа-время в мс, чтобы проверять соответствие pts и содержимого.
fn frame_payload(media_ms: i64) -> Vec<u8> {
    format!("{media_ms:06}").into_bytes()
}

/// Как блоки кластера упаковывают кадры.
#[derive(Clone, Copy)]
enum BlockLayout {
    /// Один кадр на SimpleBlock (так ffmpeg пишет Opus).
    OneFramePerBlock,
    /// Пять кадров на SimpleBlock с fixed-size lacing (так mkvmerge пишет многие аудиокодеки).
    FiveLacedFrames,
}

/// Строит SimpleBlock: track vint, относительное время, флаги, (lacing) и кадры.
fn simple_block(relative_ms: i64, frames_ms: &[i64]) -> Vec<u8> {
    let mut block = vec![0x80 | TRACK_NUMBER as u8];
    block.extend_from_slice(&(relative_ms as i16).to_be_bytes());
    if frames_ms.len() == 1 {
        // Keyframe, без lacing.
        block.push(0x80);
    }
    else {
        // Keyframe + fixed-size lacing; следом число кадров минус один.
        block.push(0x80 | 0x04);
        block.push((frames_ms.len() - 1) as u8);
    }
    for frame_ms in frames_ms {
        block.extend(frame_payload(*frame_ms));
    }
    element(&[0xA3], &block)
}

/// Опорный блок cue-точки: его время и смещение внутри данных кластера.
struct CueAnchor {
    time_ms: i64,
    relative_position: u64,
}

/// Кластер на одну секунду; возвращает байты и cue-якорь на ВТОРОЙ блок кластера.
fn cluster(cluster_ms: i64, layout: BlockLayout) -> (Vec<u8>, CueAnchor) {
    let timestamp = uint(&[0xE7], cluster_ms as u64);
    let frames_per_block = match layout {
        BlockLayout::OneFramePerBlock => 1,
        BlockLayout::FiveLacedFrames => 5,
    };
    let frame_times: Vec<i64> =
        (0..FRAMES_PER_CLUSTER).map(|index| cluster_ms + index * FRAME_DURATION_MS).collect();
    let blocks: Vec<Vec<u8>> = frame_times
        .chunks(frames_per_block)
        .map(|block_frames| simple_block(block_frames[0] - cluster_ms, block_frames))
        .collect();
    let anchor = CueAnchor {
        time_ms: cluster_ms + frames_per_block as i64 * FRAME_DURATION_MS,
        relative_position: (timestamp.len() + blocks[0].len()) as u64,
    };
    (element(&[0x1F, 0x43, 0xB6, 0x75], &[timestamp, blocks.concat()].concat()), anchor)
}

/// Cues: по точке на кластер, позиции относительно начала данных Segment.
fn cues(points: &[(i64, u64, u64)]) -> Vec<u8> {
    let cue_points: Vec<u8> = points
        .iter()
        .flat_map(|(cue_ms, cluster_position, relative_position)| {
            let positions = element(
                &[0xB7],
                &[
                    uint(&[0xF7], u64::from(TRACK_NUMBER)),
                    uint(&[0xF1], *cluster_position),
                    uint(&[0xF0], *relative_position),
                ]
                .concat(),
            );
            element(&[0xBB], &[uint(&[0xB3], *cue_ms as u64), positions].concat())
        })
        .collect();
    element(&[0x1C, 0x53, 0xBB, 0x6B], &cue_points)
}

/// Matroska с кластерами, начинающимися в `cluster_starts_ms`, и cue-точкой на второй блок
/// каждого из них.
fn matroska_with_cues(cluster_starts_ms: &[i64], layout: BlockLayout) -> Vec<u8> {
    let ebml = element(&[0x1A, 0x45, 0xDF, 0xA3], &text(&[0x42, 0x82], "matroska"));
    // TimestampScale = 1 мс.
    let info = element(
        &[0x15, 0x49, 0xA9, 0x66],
        &[
            uint(&[0x2A, 0xD7, 0xB1], 1_000_000),
            text(&[0x4D, 0x80], "test"),
            text(&[0x57, 0x41], "test"),
        ]
        .concat(),
    );
    let track_entry = element(
        &[0xAE],
        &[
            uint(&[0xD7], u64::from(TRACK_NUMBER)),
            uint(&[0x73, 0xC5], 1),
            uint(&[0x83], 17),
            text(&[0x86], "S_TEXT/UTF8"),
            uint(&[0x23, 0xE3, 0x83], (FRAME_DURATION_MS * 1_000_000) as u64),
        ]
        .concat(),
    );
    let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &track_entry);
    let clusters: Vec<(Vec<u8>, CueAnchor)> =
        cluster_starts_ms.iter().map(|start_ms| cluster(*start_ms, layout)).collect();

    // Длина Cues не зависит от значений (uint фиксированной ширины), поэтому позиции кластеров
    // считаются по черновику Cues с нулевыми позициями.
    let draft_points: Vec<(i64, u64, u64)> = clusters.iter().map(|_| (0, 0, 0)).collect();
    let mut cluster_position = (info.len() + tracks.len() + cues(&draft_points).len()) as u64;
    let mut points = Vec::new();
    for (cluster_bytes, anchor) in &clusters {
        points.push((anchor.time_ms, cluster_position, anchor.relative_position));
        cluster_position += cluster_bytes.len() as u64;
    }

    let mut segment_payload = [info, tracks, cues(&points)].concat();
    for (cluster_bytes, _) in clusters {
        segment_payload.extend(cluster_bytes);
    }
    [ebml, element(&[0x18, 0x53, 0x80, 0x67], &segment_payload)].concat()
}

fn open(bytes: Vec<u8>) -> MkvReader<'static> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    MkvReader::try_new(source, FormatOptions::default()).expect("synthetic matroska must open")
}

/// Seek к цели в мс дорожки; возвращает фактическую позицию приземления.
fn seek_to_ms(reader: &mut MkvReader<'_>, target_ms: i64) -> symphonia_core::errors::Result<i64> {
    let seeked = reader.seek(
        SeekMode::Accurate,
        SeekTo::Timestamp { ts: Timestamp::new(target_ms), track_id: TRACK_NUMBER },
    )?;
    Ok(seeked.actual_ts.get())
}

/// Следующий packet как пара (pts в мс, время из payload в мс).
fn next_frame(reader: &mut MkvReader<'_>) -> (i64, i64) {
    let packet = reader.next_packet().expect("packet read").expect("packet before end");
    let payload_ms = std::str::from_utf8(&packet.data)
        .expect("utf8 payload")
        .parse::<i64>()
        .expect("payload holds media ms");
    (packet.pts.get(), payload_ms)
}

/// Seek приземляется на кадр, покрывающий цель, внутри кластера, выбранного по Cues, — для
/// прыжка вперёд, назад, почти в конец и ровно на границу кластера. Pts совпадает с содержимым.
#[test]
fn cue_seek_lands_on_frame_covering_target_with_matching_payload() {
    let cluster_starts_ms = [0, 1_000, 2_000];
    let media_end_ms = 3_000;
    let mut reader = open(matroska_with_cues(&cluster_starts_ms, BlockLayout::OneFramePerBlock));

    // (цель, ожидаемый кадр): внутри второго кластера, почти конец, назад и цель между началом
    // кластера и его cue-точкой (1100 мс) — опора на cue предыдущего кластера, переход через
    // границу кластеров. Цели не на границе кадров: upstream-правило «конец кадра ≥ цели» на
    // точной границе честно садится на кадр раньше (Accurate = не позже цели).
    for (target_ms, expected_frame_ms) in
        [(1_350, 1_300), (2_950, 2_900), (450, 400), (1_050, 1_000)]
    {
        let landed_ms = seek_to_ms(&mut reader, target_ms).expect("seek inside media succeeds");
        assert_eq!(landed_ms, expected_frame_ms, "seek to {target_ms} ms reported wrong landing");
        assert_eq!(
            next_frame(&mut reader),
            (expected_frame_ms, expected_frame_ms),
            "seek to {target_ms} ms delivered a wrong first packet"
        );
        // Следующий кадр идёт подряд: чтение после seek-а продолжает ту же шкалу времени.
        let following_ms = expected_frame_ms + FRAME_DURATION_MS;
        // После последнего кадра не читаем: seek после EOF — отдельный контракт (reprobe в
        // symphonia-demux), здесь проверяется только переход внутри медиа.
        if following_ms < media_end_ms {
            assert_eq!(next_frame(&mut reader), (following_ms, following_ms));
        }
    }
}

/// Дорожка начинается на 2-й секунде: цель раньше первой cue-точки приземляется на первый
/// кадр (фактическая позиция честно позже цели), а не отказывает `OutOfRange`.
#[test]
fn cue_seek_before_first_cue_lands_on_first_frame_of_late_track() {
    let mut reader =
        open(matroska_with_cues(&[2_000, 3_000, 4_000], BlockLayout::OneFramePerBlock));
    // Уходим вперёд, чтобы seek назад был настоящим переходом, а не чтением с начала.
    assert_eq!(seek_to_ms(&mut reader, 3_550).expect("forward seek"), 3_500);
    assert_eq!(next_frame(&mut reader), (3_500, 3_500));

    let landed_ms = seek_to_ms(&mut reader, 1_200).expect("seek before track start succeeds");

    assert_eq!(landed_ms, 2_000);
    assert_eq!(next_frame(&mut reader), (2_000, 2_000));
}

/// Остаток laced-блока, разобранного до seek-а, не должен стать точкой приземления и первым
/// packet-ом после seek-а назад.
#[test]
fn cue_seek_discards_frames_left_from_laced_block_before_seek() {
    let mut reader = open(matroska_with_cues(&[0, 1_000, 2_000], BlockLayout::FiveLacedFrames));
    assert_eq!(seek_to_ms(&mut reader, 2_050).expect("seek to last cluster"), 2_000);
    // Из блока 2000..2400 прочитан один кадр, четыре (2100..2400) остались в буфере reader-а.
    assert_eq!(next_frame(&mut reader), (2_000, 2_000));

    let landed_ms = seek_to_ms(&mut reader, 1_350).expect("backward seek succeeds");

    assert_eq!(landed_ms, 1_300);
    assert_eq!(next_frame(&mut reader), (1_300, 1_300));
    assert_eq!(next_frame(&mut reader), (1_400, 1_400));
}
