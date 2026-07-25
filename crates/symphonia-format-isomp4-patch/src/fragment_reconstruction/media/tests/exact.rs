//! Exact capture round-trip и deterministic canonical shape.

use super::super::FragmentMediaKind;
use super::support::{
    AUDIO_FIRST, AUDIO_SECOND, VIDEO_HIGH_FIRST, VIDEO_HIGH_SECOND, VIDEO_LOW_FIRST, inspect,
    inspection_limits, reconstruct,
};

#[test]
fn all_five_captured_fragments_round_trip_exact_samples() {
    let cases = [
        (
            VIDEO_LOW_FIRST,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        ),
        (
            VIDEO_HIGH_FIRST,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        ),
        (
            VIDEO_HIGH_SECOND,
            40_000_000,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        ),
        (
            AUDIO_FIRST,
            0,
            FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        ),
        (
            AUDIO_SECOND,
            39_680_000,
            FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        ),
    ];
    let limits = inspection_limits();

    for (input, base_decode_time, media_kind) in cases {
        let original = inspect(input, base_decode_time, media_kind, &limits);
        let reconstructed =
            reconstruct(input, base_decode_time, media_kind).expect("capture reconstructs");
        let canonical = inspect(
            reconstructed.as_bytes(),
            base_decode_time,
            media_kind,
            &limits,
        );

        assert_eq!(reconstructed.sequence_number(), original.sequence_number());
        assert_eq!(reconstructed.track_id(), original.track_id());
        assert_eq!(reconstructed.coded_coverage(), original.coded_coverage());
        assert_eq!(canonical.sequence_number(), original.sequence_number());
        assert_eq!(canonical.track_id(), original.track_id());
        assert_eq!(canonical.coded_coverage(), original.coded_coverage());
        assert_eq!(canonical.samples().len(), original.samples().len());
        for sample_index in 0..original.samples().len() {
            let expected = &original.samples()[sample_index];
            let actual = &canonical.samples()[sample_index];
            assert_eq!(actual.dts(), expected.dts());
            assert_eq!(actual.pts(), expected.pts());
            assert_eq!(actual.duration(), expected.duration());
            assert_eq!(actual.composition_offset(), expected.composition_offset());
            assert_eq!(actual.flags(), expected.flags());
            assert_eq!(
                canonical.sample_payload(sample_index),
                original.sample_payload(sample_index)
            );
        }
        assert_canonical_top_level(reconstructed.as_bytes());
    }
}

#[test]
fn identical_request_is_byte_deterministic_and_debug_is_payload_safe() {
    let first = reconstruct(
        VIDEO_HIGH_FIRST,
        0,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
    )
    .expect("first reconstruction");
    let second = reconstruct(
        VIDEO_HIGH_FIRST,
        0,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
    )
    .expect("second reconstruction");
    assert_eq!(first.as_bytes(), second.as_bytes());

    let debug = format!("{first:?}");
    assert!(debug.contains("byte_length"));
    assert!(!debug.contains(&format!("{:?}", &first.as_bytes()[0..16])));
}

fn assert_canonical_top_level(bytes: &[u8]) {
    assert_eq!(&bytes[4..8], b"moof");
    let moof_size = usize::try_from(u32::from_be_bytes(
        bytes[0..4].try_into().expect("moof size"),
    ))
    .expect("moof size fits usize");
    assert_eq!(&bytes[moof_size + 4..moof_size + 8], b"mdat");
    let mdat_size = usize::try_from(u32::from_be_bytes(
        bytes[moof_size..moof_size + 4]
            .try_into()
            .expect("mdat size"),
    ))
    .expect("mdat size fits usize");
    assert_eq!(moof_size + mdat_size, bytes.len());
}
