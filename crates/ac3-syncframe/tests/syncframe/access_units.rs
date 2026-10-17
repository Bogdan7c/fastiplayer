//! Сборка access unit-ов: границы пакетов, программы, каналы, потоковые буферы.

use ac3_syncframe::{
    AccessUnit, AccessUnitScan, MAX_SYNCFRAMES_PER_ACCESS_UNIT, StreamContinuation, SyncframeCodec,
    SyncframeHeaderError, scan_access_unit,
};

use crate::support::{Eac3Frame, ac3_frame_48k};

fn complete(scan: Result<AccessUnitScan, SyncframeHeaderError>) -> AccessUnit {
    match scan {
        Ok(AccessUnitScan::Complete(access_unit)) => access_unit,
        other => panic!("complete access unit expected, got {other:?}"),
    }
}

fn concat(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.concat()
}

#[test]
fn ac3_access_unit_is_one_syncframe_closed_by_next_frame() {
    let first = ac3_frame_48k(8, 7, true);
    let second = ac3_frame_48k(8, 2, false);
    let stream = concat(&[first.clone(), second.clone()]);

    let access_unit = complete(scan_access_unit(
        &stream,
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(access_unit.total_length_bytes(), first.len());
    assert_eq!(access_unit.primary_program_length_bytes(), first.len());
    assert_eq!(access_unit.syncframe_count(), 1);
    assert_eq!(access_unit.presentation_channel_count(), 6);
    assert_eq!(access_unit.samples_per_frame(), 1536);
    assert_eq!(access_unit.primary_header().codec(), SyncframeCodec::Ac3);

    // Последний кадр: без конца потока пакет не закрыт — вдруг следом dependent.
    assert_eq!(
        scan_access_unit(&second, StreamContinuation::MoreBytesMayFollow),
        Ok(AccessUnitScan::NeedMoreData)
    );
    let last = complete(scan_access_unit(&second, StreamContinuation::EndOfStream));
    assert_eq!(last.total_length_bytes(), second.len());
    assert_eq!(last.presentation_channel_count(), 2);
}

#[test]
fn eac3_dependent_substream_joins_primary_access_unit_and_extends_channels() {
    let independent = Eac3Frame::independent_5p1().encode();
    let dependent = Eac3Frame::dependent_back_pair().encode();
    let stream = concat(&[independent.clone(), dependent.clone(), independent.clone()]);

    let access_unit = complete(scan_access_unit(
        &stream,
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(
        access_unit.total_length_bytes(),
        independent.len() + dependent.len()
    );
    assert_eq!(
        access_unit.primary_program_length_bytes(),
        access_unit.total_length_bytes()
    );
    assert_eq!(access_unit.syncframe_count(), 2);
    // 5.1 независимого + пара Lrs/Rrs dependent-а = 7.1.
    assert_eq!(access_unit.presentation_channel_count(), 8);
    assert_eq!(access_unit.primary_header().channel_count(), 6);
    assert!(!access_unit.has_additional_programs());
}

#[test]
fn additional_programs_follow_primary_prefix_and_do_not_change_its_channels() {
    let independent = Eac3Frame::independent_5p1().encode();
    let dependent = Eac3Frame::dependent_back_pair().encode();
    let second_program = Eac3Frame {
        substream_id: 1,
        acmod: 2,
        lfeon: false,
        ..Eac3Frame::independent_5p1()
    }
    .encode();
    let second_program_dependent = Eac3Frame {
        substream_id: 1,
        // Пара Lw/Rw — если бы она попала в основную программу, каналов стало бы 10.
        chanmap: Some(1 << (15 - 10)),
        ..Eac3Frame::dependent_back_pair()
    }
    .encode();
    let stream = concat(&[
        independent.clone(),
        dependent.clone(),
        second_program.clone(),
        second_program_dependent.clone(),
        independent.clone(),
    ]);

    let access_unit = complete(scan_access_unit(
        &stream,
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(
        access_unit.total_length_bytes(),
        stream.len() - independent.len()
    );
    assert_eq!(
        access_unit.primary_program_length_bytes(),
        independent.len() + dependent.len()
    );
    assert_eq!(access_unit.syncframe_count(), 4);
    assert!(access_unit.has_additional_programs());
    assert_eq!(access_unit.presentation_channel_count(), 8);
}

#[test]
fn leading_dependent_after_resync_is_reported_as_orphan() {
    let dependent = Eac3Frame::dependent_back_pair().encode();
    let stream = concat(&[dependent.clone(), Eac3Frame::independent_5p1().encode()]);
    match scan_access_unit(&stream, StreamContinuation::MoreBytesMayFollow) {
        Ok(AccessUnitScan::OrphanSyncframe {
            header,
            syncframe_length_bytes,
        }) => {
            assert_eq!(syncframe_length_bytes, dependent.len());
            assert!(!header.starts_access_unit());
        }
        other => panic!("orphan expected, got {other:?}"),
    }
}

/// Пакет, разрезанный в любом месте (как границами PES), не публикуется
/// раньше времени; целиком — публикуется один раз с правильной длиной.
#[test]
fn every_split_point_waits_for_more_data_until_access_unit_is_closed() {
    let independent = Eac3Frame::independent_5p1().encode();
    let dependent = Eac3Frame::dependent_back_pair().encode();
    let access_unit_bytes = concat(&[independent.clone(), dependent.clone()]);
    for length in 0..access_unit_bytes.len() {
        assert_eq!(
            scan_access_unit(
                &access_unit_bytes[..length],
                StreamContinuation::MoreBytesMayFollow
            ),
            Ok(AccessUnitScan::NeedMoreData),
            "prefix {length}"
        );
    }
    // Ровно на границе пакета без конца потока — тоже ждать (dependent мог бы продолжиться).
    assert_eq!(
        scan_access_unit(&access_unit_bytes, StreamContinuation::MoreBytesMayFollow),
        Ok(AccessUnitScan::NeedMoreData)
    );
    let closed = complete(scan_access_unit(
        &access_unit_bytes,
        StreamContinuation::EndOfStream,
    ));
    assert_eq!(closed.total_length_bytes(), access_unit_bytes.len());
}

#[test]
fn corrupt_frame_after_complete_access_unit_is_reported_by_next_scan() {
    let independent = Eac3Frame::independent_5p1().encode();
    // Синхрослово с bsid 31 — повреждённый следующий кадр.
    let corrupt_next_frame = vec![0x0b, 0x77, 0, 0, 0, 0xff, 0, 0];
    let stream = concat(&[independent.clone(), corrupt_next_frame]);
    let access_unit = complete(scan_access_unit(
        &stream,
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(access_unit.total_length_bytes(), independent.len());
    assert_eq!(
        scan_access_unit(
            &stream[access_unit.total_length_bytes()..],
            StreamContinuation::MoreBytesMayFollow
        ),
        Err(SyncframeHeaderError::UnsupportedBitstreamId { bitstream_id: 31 })
    );
}

#[test]
fn truncated_frame_at_end_of_stream_is_typed_error() {
    let independent = Eac3Frame::independent_5p1().encode();
    assert_eq!(
        scan_access_unit(&independent[..40], StreamContinuation::EndOfStream),
        Err(SyncframeHeaderError::Truncated {
            available_bytes: 40,
            required_bytes: independent.len()
        })
    );
    // Оборванный dependent в конце потока не портит предыдущий пакет.
    let dependent = Eac3Frame::dependent_back_pair().encode();
    let stream = concat(&[independent.clone(), dependent[..20].to_vec()]);
    let access_unit = complete(scan_access_unit(&stream, StreamContinuation::EndOfStream));
    assert_eq!(access_unit.total_length_bytes(), independent.len());
    assert!(matches!(
        scan_access_unit(
            &stream[independent.len()..],
            StreamContinuation::EndOfStream
        ),
        Err(SyncframeHeaderError::Truncated { .. })
    ));
}

#[test]
fn endless_dependent_chain_is_bounded() {
    let mut frames = vec![Eac3Frame::independent_5p1().encode()];
    for _ in 0..MAX_SYNCFRAMES_PER_ACCESS_UNIT {
        frames.push(Eac3Frame::dependent_back_pair().encode());
    }
    assert_eq!(
        scan_access_unit(&concat(&frames), StreamContinuation::EndOfStream),
        Err(SyncframeHeaderError::AccessUnitTooLong {
            maximum_syncframes: MAX_SYNCFRAMES_PER_ACCESS_UNIT
        })
    );
    // Ровно предел — ещё корректный пакет.
    frames.pop();
    let access_unit = complete(scan_access_unit(
        &concat(&frames),
        StreamContinuation::EndOfStream,
    ));
    assert_eq!(
        access_unit.syncframe_count(),
        MAX_SYNCFRAMES_PER_ACCESS_UNIT
    );
}

#[test]
fn short_eac3_frames_give_short_access_unit_duration() {
    let one_block = Eac3Frame {
        numblkscod_or_fscod2: 0,
        ..Eac3Frame::independent_5p1()
    }
    .encode();
    let stream = concat(&[one_block.clone(), one_block]);
    let access_unit = complete(scan_access_unit(
        &stream,
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(access_unit.samples_per_frame(), 256);
}

#[test]
fn ac3_core_with_eac3_dependent_is_reported_as_enhanced_program() {
    let core = ac3_frame_48k(8, 7, true);
    let extension = Eac3Frame::dependent_back_pair().encode();
    let stream = concat(&[core.clone(), extension.clone(), core.clone()]);
    let access_unit = complete(scan_access_unit(
        &stream,
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(
        access_unit.total_length_bytes(),
        core.len() + extension.len()
    );
    assert_eq!(access_unit.primary_header().codec(), SyncframeCodec::Ac3);
    assert_eq!(access_unit.codec(), SyncframeCodec::EnhancedAc3);
    assert_eq!(access_unit.presentation_channel_count(), 8);

    // Чистый AC-3 пакет остаётся AC-3.
    let plain = complete(scan_access_unit(
        &concat(&[core.clone(), core]),
        StreamContinuation::MoreBytesMayFollow,
    ));
    assert_eq!(plain.codec(), SyncframeCodec::Ac3);
}
