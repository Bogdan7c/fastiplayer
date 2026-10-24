use super::*;

#[test]
fn reference_levels_accept_player_defaults_and_reject_invalid_values() {
    assert_eq!(
        HdrReferenceLevels::new(100.0, 1_000.0),
        Ok(HdrReferenceLevels::REFERENCE_DEFAULT)
    );
    for (white, peak) in [
        (0.0, 1_000.0),
        (-1.0, 1_000.0),
        (f32::NAN, 1_000.0),
        (100.0, f32::INFINITY),
    ] {
        assert!(
            matches!(
                HdrReferenceLevels::new(white, peak),
                Err(HdrLevelsError::NotPositive { .. })
            ),
            "white {white}, peak {peak}"
        );
    }
    assert_eq!(
        HdrReferenceLevels::new(200.0, 200.0),
        Err(HdrLevelsError::PeakNotAboveWhite {
            sdr_white_nits: 200.0,
            hdr_peak_nits: 200.0
        })
    );
}
