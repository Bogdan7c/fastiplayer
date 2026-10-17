//! Поиск syncframe-а: мусор, ложные синхрослова, неполные буферы.

use ac3_syncframe::{SyncframeCodec, SyncframeSearch, locate_syncframe};

use crate::support::{Eac3Frame, ac3_frame_48k};

#[test]
fn garbage_before_first_frame_is_skipped() {
    let frame = ac3_frame_48k(10, 2, false);
    let mut stream = vec![0x00, 0x47, 0x0b, 0x12, 0xff];
    stream.extend_from_slice(&frame);
    match locate_syncframe(&stream) {
        SyncframeSearch::Found { offset, header } => {
            assert_eq!(offset, 5);
            assert_eq!(header.frame_length_bytes(), frame.len());
            assert_eq!(header.codec(), SyncframeCodec::Ac3);
        }
        other => panic!("frame expected, got {other:?}"),
    }
}

#[test]
fn false_sync_word_with_invalid_header_is_skipped() {
    // 0x0B77 с bsid 31 — не syncframe; следом настоящий E-AC-3 кадр.
    let mut stream = vec![0x0b, 0x77, 0x00, 0x00, 0x00, 0xf8, 0x00, 0x00];
    let frame = Eac3Frame::independent_5p1().encode();
    stream.extend_from_slice(&frame);
    match locate_syncframe(&stream) {
        SyncframeSearch::Found { offset, header } => {
            assert_eq!(offset, 8);
            assert_eq!(header.codec(), SyncframeCodec::EnhancedAc3);
        }
        other => panic!("frame expected, got {other:?}"),
    }
}

#[test]
fn incomplete_tail_is_kept_for_next_read() {
    assert_eq!(
        locate_syncframe(&[]),
        SyncframeSearch::NeedMoreData {
            discardable_prefix_bytes: 0
        }
    );
    // Последний байт может быть первой половиной синхрослова.
    assert_eq!(
        locate_syncframe(&[0x01, 0x02, 0x0b]),
        SyncframeSearch::NeedMoreData {
            discardable_prefix_bytes: 2
        }
    );
    assert_eq!(
        locate_syncframe(&[0x01, 0x02, 0x03]),
        SyncframeSearch::NeedMoreData {
            discardable_prefix_bytes: 3
        }
    );
    // Обрезанный заголовок: всё до синхрослова можно выбросить, заголовок — нет.
    let frame = Eac3Frame::independent_5p1().encode();
    let mut stream = vec![0xaa, 0xbb];
    stream.extend_from_slice(&frame[..4]);
    assert_eq!(
        locate_syncframe(&stream),
        SyncframeSearch::NeedMoreData {
            discardable_prefix_bytes: 2
        }
    );
}
