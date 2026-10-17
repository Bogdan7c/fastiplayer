//! Учёт бит E-AC-3 (Annex E §2.2.5): после всех блоков позиция чтения +
//! `nauxbits` + хвост ровно равны длине кадра, а `auxbits` ffmpeg — нули.

use super::support::{EAC3_FIXTURES, EnhancedHarness, bits_are_zero, split_frames};
use crate::config::CrcPolicy;
use crate::spectral::dither::DitherMode;

/// `auxdatae` (1) + `encinfo` (1) + `crc2` (16).
const TRAILER_BITS_WITHOUT_USER_DATA: usize = 18;

#[test]
fn every_fixture_frame_is_consumed_exactly() {
    for fixture in &EAC3_FIXTURES {
        let mut harness = EnhancedHarness::new(DitherMode::Disabled);
        let frames = split_frames(fixture.bytes);
        assert!(frames.len() >= 6, "{}", fixture.name);
        for (index, frame) in frames.into_iter().enumerate() {
            let decoded = harness
                .decode(frame, CrcPolicy::Verify)
                .unwrap_or_else(|error| panic!("{} #{index}: {error}", fixture.name));
            let trailer = decoded.trailer;
            assert_eq!(trailer.user_data_bits, None, "{} #{index}", fixture.name);
            assert_eq!(
                trailer.audio_end_bits + trailer.auxiliary_bits + TRAILER_BITS_WITHOUT_USER_DATA,
                frame.len() * 8,
                "{} #{index}",
                fixture.name
            );
            assert!(
                bits_are_zero(frame, trailer.audio_end_bits, trailer.auxiliary_bits),
                "{} #{index}",
                fixture.name
            );
            // Разбор, остановившийся рано, оставил бы большой «хвост» (у ffmpeg — 1–2 %).
            assert!(
                trailer.auxiliary_bits * 20 < frame.len() * 8,
                "{} #{index}: {}",
                fixture.name,
                trailer.auxiliary_bits
            );
        }
        assert_eq!(
            harness.usage.enhanced_coupling_blocks + harness.usage.spectral_extension_blocks,
            0
        );
    }
}

/// ffmpeg пишет coupling только на низких битрейтах; фиксируем, какие
/// фикстуры его реально используют (иначе тест coupling ничего не проверял бы).
#[test]
fn coupling_usage_matches_bitrate_choice_of_encoder() {
    for fixture in &EAC3_FIXTURES {
        let mut harness = EnhancedHarness::new(DitherMode::Disabled);
        for frame in split_frames(fixture.bytes) {
            harness.decode(frame, CrcPolicy::Verify).unwrap();
        }
        let usage = harness.usage;
        let expect_coupling = !matches!(
            fixture.name,
            "mono-48k-64k.eac3" | "stereo-32k-640k.eac3" | "5p1-32k-1000k.eac3"
        );
        assert_eq!(
            usage.coupling_blocks == usage.audio_blocks,
            expect_coupling,
            "{}: {usage:?}",
            fixture.name
        );
        assert!(
            expect_coupling || usage.coupling_blocks == 0,
            "{}",
            fixture.name
        );
    }
}
