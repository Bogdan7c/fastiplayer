//! Conformance-набор на честном fake и на fake с дефектами.
//!
//! Вторая половина доказывает, что проверки не «вакуумные»: каждый дефект ломает ровно
//! свою проверку, а остальные проверки на нём проходят.

use crate::conformance::{ConformanceCheck, run_render_shell_conformance};

use super::FakeHarness;
use super::fake_shell::FakeDefect;

/// Честный fake проходит весь набор.
#[test]
fn honest_fake_renderer_passes_every_conformance_check() {
    let violations = run_render_shell_conformance(&mut FakeHarness {
        defect: FakeDefect::None,
    });

    assert!(
        violations.is_empty(),
        "unexpected violations: {violations:#?}"
    );
}

/// Каждый дефект ловится своей проверкой и не задевает чужие.
///
/// Удержание ключей рендером ломает обе проверки освобождения: кадр не возвращается декодеру
/// ни обычным путём, ни при потере устройства (ключ держит аренду). Молчаливое принятие правки
/// неизвестной текстуры ломает и проверку 11: её последний шаг (правка после `free`) тоже
/// обязан закончиться `UiTexturesOutOfSync`.
#[test]
fn each_contract_defect_is_caught_by_its_own_check() {
    let cases = [
        (
            FakeDefect::AcceptsForeignSurface,
            vec![ConformanceCheck::ForeignSurfaceFrameRejected],
        ),
        (
            FakeDefect::ReusesWithoutRetainedUi,
            vec![ConformanceCheck::ReusePreviousRequiresRetainedUi],
        ),
        (
            FakeDefect::ReportsUnsupportedAsError,
            vec![ConformanceCheck::UnsupportedContractIsNotError],
        ),
        (
            FakeDefect::ReportsMissingBackendAsMissing,
            vec![ConformanceCheck::MissingVideoBackendIsDistinct],
        ),
        (
            FakeDefect::RetainsKeys,
            vec![
                ConformanceCheck::FrameReleasedExactlyOnce,
                ConformanceCheck::DeviceLostReleasesExactlyOnce,
            ],
        ),
        (
            FakeDefect::KeepsPendingAfterDeviceLost,
            vec![ConformanceCheck::DeviceLostReleasesExactlyOnce],
        ),
        (
            FakeDefect::PreparerStampsForeignOwner,
            vec![ConformanceCheck::PreparerPreparesOnAnotherThread],
        ),
        (
            FakeDefect::AcquiresWhileDetached,
            vec![ConformanceCheck::DetachedPresentationGivesNoFrames],
        ),
        (
            FakeDefect::ReconfigureNeverReportsNoOp,
            vec![ConformanceCheck::PresentationReconfigureDistinguishesNoOp],
        ),
        (
            FakeDefect::AcceptsPartialEditOfUnknownUiTexture,
            vec![
                ConformanceCheck::PartialEditOfUnknownUiTextureIsReported,
                ConformanceCheck::UiTexturesApplyOnRejectedFrames,
            ],
        ),
        (
            FakeDefect::AppliesUiTexturesOnlyWhenDrawing,
            vec![ConformanceCheck::UiTexturesApplyOnRejectedFrames],
        ),
        (
            FakeDefect::SilentPresentPassSignal,
            vec![ConformanceCheck::PresentPassSignalFollowsPresentedFrames],
        ),
        (
            FakeDefect::SignalsPresentPassForRejectedFrames,
            vec![ConformanceCheck::PresentPassSignalFollowsPresentedFrames],
        ),
    ];

    for (defect, expected_checks) in cases {
        let violations = run_render_shell_conformance(&mut FakeHarness { defect });
        let failed_checks: Vec<ConformanceCheck> =
            violations.iter().map(|violation| violation.check).collect();

        assert_eq!(
            failed_checks, expected_checks,
            "defect {defect:?} must fail exactly {expected_checks:?}; violations: {violations:#?}"
        );
    }
}
