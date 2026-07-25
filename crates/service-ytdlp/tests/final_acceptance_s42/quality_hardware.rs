//! Exact quality polarity matrix и owner-approved VA-API exception.

use std::collections::BTreeSet;

use super::S42_ACCEPTANCE_PATH;
use super::support::{
    assert_evidence_role, assert_vaapi_profile_arm_sets, executable_polarities, load_json_document,
    required_array, required_bool, required_evidence_ids, required_object, required_string,
    rows_by_id,
};

/// Quality axes разделены, а VA-API exception не подделывает manual PASS.
#[test]
fn quality_evidence_and_hardware_exception_preserve_exact_acceptance_state() {
    // Загружаем scoped profile-traceability artifact.
    let s42_acceptance = load_json_document(S42_ACCEPTANCE_PATH);
    // Catalog нужен для reference validation.
    let evidence_catalog = required_object(&s42_acceptance, "evidence_catalog");

    // Quality concerns индексируются отдельно, чтобы один слабый тест не закрыл всё.
    let quality_evidence = rows_by_id(
        required_array(&s42_acceptance, "quality_selection_evidence"),
        "S42 quality evidence",
    );
    // BestPlayable, Exact, height и runtime override являются разными axes.
    assert_eq!(
        quality_evidence.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "best-playable",
            "exact",
            "preferred-height",
            "runtime-override",
        ])
    );
    // Каждый axis обязан иметь typed evidence с exact executable polarity.
    for (quality_axis, evidence) in quality_evidence {
        // Evidence IDs читаются из единого schema field.
        let evidence_ids = required_evidence_ids(
            evidence,
            "evidence_ids",
            &format!("quality axis `{quality_axis}`"),
        );
        // Catalog role не позволяет подменить switch тест unrelated fixture-ом.
        assert_evidence_role(
            evidence_catalog,
            &evidence_ids,
            "quality",
            &format!("quality axis `{quality_axis}`"),
        );
        // Exact и runtime override требуют success path и rollback/rejection path.
        let expected_polarities = match quality_axis {
            // BestPlayable обязан иметь реальный successful selection.
            "best-playable" | "preferred-height" => BTreeSet::from(["Positive"]),
            // Exact и switch semantics обязаны иметь обе стороны contract-а.
            "exact" | "runtime-override" => BTreeSet::from(["Positive", "Negative"]),
            // Exact axis set выше делает этот arm unreachable.
            unexpected_axis => panic!("unknown quality axis `{unexpected_axis}`"),
        };
        // Negative-only fixture больше не может закрыть positive quality claim.
        assert_eq!(
            executable_polarities(evidence_catalog, &evidence_ids),
            expected_polarities,
            "quality axis `{quality_axis}` имеет неверную polarity matrix"
        );
    }

    // Hardware exception хранится отдельно от generic Implemented rows.
    let hardware_exception = s42_acceptance
        .get("hardware_capability_exception")
        // Missing decision нельзя трактовать как unchanged hardware.
        .unwrap_or_else(|| panic!("S42 hardware capability exception отсутствует"));
    // Stable ID связывает решение с S27 root-cause fix.
    assert_eq!(
        required_string(hardware_exception, "id"),
        "s27-vaapi-h264-baseline"
    );
    // Только owner-approved exception допускает hardware matrix delta.
    assert_eq!(
        required_string(hardware_exception, "decision"),
        "OwnerApprovedException"
    );
    // Scope запрещает alias на Constrained Baseline либо другое pixel layout.
    assert_eq!(
        required_string(hardware_exception, "scope"),
        "exact VAProfileH264Baseline, H.264 Baseline 8-bit YUV420/NV12, capability intersection only"
    );
    // Software fallback не удаляется ради hardware admission.
    assert!(required_bool(
        hardware_exception,
        "software_fallback_retained"
    ));
    // Planning, software, probe, no-alias, adapter и production dispatch обязательны.
    let hardware_evidence_ids = required_evidence_ids(
        hardware_exception,
        "evidence_ids",
        "S27 VA-API Baseline exception",
    );
    // Exact set не позволяет свести exception к одному enum test.
    assert_eq!(
        hardware_evidence_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "hardware-baseline-planning",
            "hardware-baseline-software",
            "hardware-baseline-vaapi-probe",
            "hardware-baseline-vaapi-no-alias",
            "hardware-baseline-vaapi-adapter",
            "hardware-vaapi-profile-dispatch",
        ])
    );
    // Runtime/code evidence обязано иметь отдельную hardware_exception role.
    assert_evidence_role(
        evidence_catalog,
        &hardware_evidence_ids,
        "hardware_exception",
        "S27 VA-API Baseline exception",
    );

    // Owner approval хранится отдельно от executable/code evidence.
    let approval_evidence_ids = required_evidence_ids(
        hardware_exception,
        "approval_evidence_ids",
        "S27 VA-API Baseline owner approval",
    );
    // Ровно checked-in user plan является authority для exception.
    assert_eq!(approval_evidence_ids, ["hardware-baseline-owner-approval"]);
    // Approval нельзя подменить self-authored production boundary.
    assert_evidence_role(
        evidence_catalog,
        &approval_evidence_ids,
        "owner_approval",
        "S27 VA-API Baseline owner approval",
    );

    // Shared strict helper связывает manifest baseline/current с production match arms.
    assert_vaapi_profile_arm_sets(hardware_exception);

    // Manual status является nested object, а не свободной строкой.
    let manual_rerun = required_object(hardware_exception, "hardware_manual_rerun");
    // Value нужен strict scalar-field helpers.
    let manual_rerun_value = hardware_exception
        .get("hardware_manual_rerun")
        // Object existence уже проверена выше.
        .expect("manual rerun value обязан существовать");
    // Текущий владелец честно не имеет второго VA-API устройства для rerun.
    assert_eq!(required_string(manual_rerun_value, "status"), "NotRun");
    // Exact reason фиксирует отсутствие compatible device без fake manual PASS.
    assert_eq!(
        required_string(manual_rerun_value, "reason"),
        "project owner currently has no compatible VA-API device available for an opt-in rerun"
    );
    // Object type используется явно, чтобы schema не принимала scalar.
    assert_eq!(manual_rerun.len(), 2);
}
