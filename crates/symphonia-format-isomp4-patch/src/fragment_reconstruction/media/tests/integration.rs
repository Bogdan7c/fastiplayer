//! Production `IsoMp4Reader` proof над accepted init + canonical media.

use super::super::FragmentMediaKind;
use super::support::{
    AUDIO_FIRST, AUDIO_SECOND, VIDEO_HIGH_FIRST, VIDEO_HIGH_SECOND, audio_initialization, inspect,
    inspection_limits, production_reader, read_packets, reconstruct, video_initialization,
};

#[test]
fn accepted_video_init_and_two_fragments_preserve_packets_timestamps_and_rap() {
    let limits = inspection_limits();
    let first_plan = inspect(
        VIDEO_HIGH_FIRST,
        0,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        &limits,
    );
    let second_plan = inspect(
        VIDEO_HIGH_SECOND,
        40_000_000,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        &limits,
    );
    let first = reconstruct(
        VIDEO_HIGH_FIRST,
        0,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
    )
    .expect("first video reconstruction");
    let second = reconstruct(
        VIDEO_HIGH_SECOND,
        40_000_000,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
    )
    .expect("second video reconstruction");
    let mut stream = video_initialization();
    stream.extend_from_slice(first.as_bytes());
    stream.extend_from_slice(second.as_bytes());

    let packets = read_packets(&mut production_reader(stream));
    assert_eq!(packets.len(), 192);
    assert_packets_match(&packets, &[&first_plan, &second_plan]);
    for plan in [&first_plan, &second_plan] {
        let first_flags = plan.samples()[0].flags().expect("video flags proven");
        assert_eq!(first_flags & 0x0001_0000, 0, "non-sync bit clear");
        assert_eq!(
            (first_flags >> 24) & 0b11,
            2,
            "sample_depends_on proves RAP"
        );
    }
}

#[test]
fn accepted_audio_init_preserves_188_187_packets_and_coverage_overhang() {
    let limits = inspection_limits();
    let first_plan = inspect(
        AUDIO_FIRST,
        0,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        &limits,
    );
    let second_plan = inspect(
        AUDIO_SECOND,
        39_680_000,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        &limits,
    );
    assert_eq!(first_plan.samples().len(), 188);
    assert_eq!(second_plan.samples().len(), 187);
    assert_eq!(first_plan.coded_coverage().end_exclusive(), 40_106_666);
    assert_eq!(second_plan.coded_coverage().end_exclusive(), 79_573_334);

    let first = reconstruct(
        AUDIO_FIRST,
        0,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
    )
    .expect("first audio reconstruction");
    let second = reconstruct(
        AUDIO_SECOND,
        39_680_000,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
    )
    .expect("second audio reconstruction");
    let mut stream = audio_initialization();
    stream.extend_from_slice(first.as_bytes());
    stream.extend_from_slice(second.as_bytes());

    let packets = read_packets(&mut production_reader(stream));
    assert_eq!(packets.len(), 188 + 187);
    assert_packets_match(&packets, &[&first_plan, &second_plan]);
}

fn assert_packets_match(
    packets: &[symphonia_core::packet::Packet],
    plans: &[&super::super::super::model::NormalizedFragmentPlan<'_>],
) {
    let expected_samples = plans.iter().flat_map(|plan| {
        plan.samples()
            .iter()
            .enumerate()
            .map(move |(sample_index, sample)| (plan, sample_index, sample))
    });
    for (packet, (plan, sample_index, sample)) in packets.iter().zip(expected_samples) {
        assert_eq!(packet.track_id, plan.track_id().get());
        assert_eq!(
            packet.dts.get(),
            i64::try_from(sample.dts()).expect("captured DTS fits i64")
        );
        assert_eq!(
            packet.pts.get(),
            i64::try_from(sample.pts()).expect("captured PTS fits i64")
        );
        assert_eq!(packet.dur.get(), u64::from(sample.duration()));
        assert_eq!(
            packet.data.as_ref(),
            plan.sample_payload(sample_index).expect("sample payload")
        );
    }
}
