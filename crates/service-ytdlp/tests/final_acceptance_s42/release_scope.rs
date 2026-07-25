//! Explicit release exclusions и approved extended absences S42.

// Ordered collections сохраняют exact identity sets и стабильные diagnostics.
use std::collections::{BTreeMap, BTreeSet};

// JSON artifacts остаются immutable input-ом focused gates.
use serde_json::Value;

// Root владеет canonical artifact paths.
use super::{PROFILE_PATH, S42_ACCEPTANCE_PATH};
// Focused module использует только общий JSON/evidence facade.
use super::support::{
    assert_evidence_role, load_json_document, required_array, required_evidence_ids,
    required_nullable_string, required_object, required_row_by_string_field, required_string,
    required_string_array, required_usize, rows_by_id,
};

/// Immutable S00/S42 документы для release-scope assertions.
struct ReleaseScopeDocuments {
    /// Canonical S00 profile и exclusion inventory.
    profile: Value,
    /// Scoped S42 explicit exclusions и conditional absences.
    s42_acceptance: Value,
}

impl ReleaseScopeDocuments {
    /// Загружает exact canonical и scoped artifacts.
    fn load() -> Self {
        // S00 нужен для canonical exclusion cross-check.
        let profile = load_json_document(PROFILE_PATH);
        // S42 нужен для scoped profile disposition.
        let s42_acceptance = load_json_document(S42_ACCEPTANCE_PATH);
        // Документы остаются read-only у focused tests.
        Self {
            profile,
            s42_acceptance,
        }
    }

    /// Индексирует три обязательные release exclusion families.
    fn release_exclusions(&self) -> BTreeMap<&str, &Value> {
        // Stable IDs запрещают positional interpretation.
        rows_by_id(
            required_array(&self.s42_acceptance, "explicit_exclusions"),
            "S42 explicit exclusions",
        )
    }

    /// Индексирует conditional approved extended absences.
    fn extended_absences(&self) -> BTreeMap<&str, &Value> {
        // Stable synthetic identities отделены от canonical target rows.
        rows_by_id(
            required_array(&self.s42_acceptance, "approved_extended_absences"),
            "S42 approved extended absences",
        )
    }

    /// Индексирует canonical S00 target rows.
    fn target_rows(&self) -> BTreeMap<&str, &Value> {
        // Exact target inventory нужен для NoApprovedRow assertions.
        rows_by_id(
            required_array(&self.profile, "target_rows"),
            "S00 target rows",
        )
    }

    /// Индексирует canonical S00 exclusions.
    fn excluded_rows(&self) -> BTreeMap<&str, &Value> {
        // Release status сверяется с canonical disposition.
        rows_by_id(
            required_array(&self.profile, "excluded_rows"),
            "S00 exclusions",
        )
    }
}

/// Три обязательные exclusion families остаются canonical и имеют typed evidence.
#[test]
fn release_exclusion_families_match_canonical_profile() {
    // Загружаем canonical и scoped release документы.
    let documents = ReleaseScopeDocuments::load();
    // Evidence catalog разрешает stable references.
    let evidence_catalog = required_object(&documents.s42_acceptance, "evidence_catalog");
    // Canonical excluded set нужен для status cross-check.
    let excluded_rows = documents.excluded_rows();
    // Ровно три обязательных roadmap exclusion families перечислены отдельно.
    let release_exclusions = documents.release_exclusions();
    // Exact identity set запрещает потерять RTSP/RTP/MMS, private live либо DRM.
    assert_eq!(
        release_exclusions.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from(["rtsp-rtp-mms", "private-live-downloaders", "drm"])
    );

    // Каждая explicit exclusion сверяется с canonical profile.
    for (row_id, release_exclusion) in &release_exclusions {
        // Release status остаётся hard ProfileExcluded.
        assert_eq!(
            required_string(release_exclusion, "status"),
            "ProfileExcluded"
        );
        // Canonical row обязана существовать.
        let profile_exclusion = excluded_rows
            .get(*row_id)
            // Missing canonical exclusion сделал бы S42 self-invented.
            .unwrap_or_else(|| panic!("S00 exclusion `{row_id}` отсутствует"));
        // Canonical status также обязан оставаться hard exclusion.
        assert_eq!(
            required_string(profile_exclusion, "status"),
            "ProfileExcluded"
        );
        // Release exclusion обязана иметь non-empty focused evidence.
        let exclusion_evidence_ids =
            required_evidence_ids(release_exclusion, "evidence_ids", row_id);
        // Catalog roles не могут подменить exclusion production/test evidence.
        assert_evidence_role(
            evidence_catalog,
            &exclusion_evidence_ids,
            "exclusion",
            &format!("exclusion `{row_id}`"),
        );
    }
}

/// Exact protocol, private-live и DRM payloads не ослабляют release exclusions.
#[test]
fn release_exclusion_payloads_are_exact() {
    // Загружаем scoped release decision.
    let documents = ReleaseScopeDocuments::load();
    // Explicit exclusions индексируются по stable identity.
    let release_exclusions = documents.release_exclusions();
    // Aggregate RTSP/RTP/MMS exclusion хранит exact scheme set.
    assert_eq!(
        required_string_array(
            release_exclusions
                .get("rtsp-rtp-mms")
                // Exact row set проверяет соседний focused test.
                .expect("RTSP/RTP/MMS exclusion обязана существовать"),
            "exact_schemes"
        )
        .into_iter()
        .collect::<BTreeSet<_>>(),
        BTreeSet::from(["rtsp", "rtp", "mms"])
    );
    // Private-live exclusion ссылается на production rejection boundary.
    let private_live = release_exclusions
        .get("private-live-downloaders")
        // Exact row set проверяет соседний focused test.
        .expect("private-live exclusion обязана существовать");
    // Typed production reason не сводится к fixture-only claim.
    assert_eq!(
        required_string(private_live, "production_rejection"),
        "KnownExcludedTransport::PrivateLiveState"
    );
    // DRM row нужна для exact trigger и evidence inventory.
    let drm = release_exclusions
        .get("drm")
        // Exact row set проверяет соседний focused test.
        .expect("DRM exclusion обязана существовать");
    // DRM trigger остаётся exact upstream field.
    assert_eq!(required_string(drm, "trigger_field"), "has_drm");
    // DRM evidence обязано закрывать yt-dlp поле и protocol parser boundaries.
    assert_eq!(
        required_evidence_ids(drm, "evidence_ids", "DRM exclusion")
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "exclusion-drm",
            "exclusion-drm-hls-sample-aes",
            "exclusion-drm-dash-content-protection",
            "exclusion-drm-smooth-protection",
            "exclusion-drm-hds-additional-header",
        ])
    );
}

/// Conditional rows не создают фиктивные provider cards и имеют exclusion evidence.
#[test]
fn approved_extended_absences_have_no_generated_provider_cards() {
    // Загружаем scoped extended decisions.
    let documents = ReleaseScopeDocuments::load();
    // Evidence catalog разрешает stable exclusion references.
    let evidence_catalog = required_object(&documents.s42_acceptance, "evidence_catalog");
    // Conditional rows индексируются отдельно от canonical target rows.
    let extended_absences = documents.extended_absences();
    // Exact set покрывает S36 live, S38 live и S40 special handoff.
    assert_eq!(
        extended_absences.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "ism-mss-live-dvr",
            "hds-live-dvr",
            "approved-special-providers",
        ])
    );

    // Каждая conditional absence имеет exact card namespace и evidence.
    for (row_id, absence) in &extended_absences {
        // Prefix задаёт утверждённую conditional-session family.
        let expected_card_prefix = match *row_id {
            // S36L зарезервирован только для будущего ISM live/DVR admission.
            "ism-mss-live-dvr" => "S36L-",
            // S38L зарезервирован только для будущего HDS live/DVR admission.
            "hds-live-dvr" => "S38L-",
            // S40P зарезервирован только для новых exact special provider rows.
            "approved-special-providers" => "S40P-",
            // Exact set assertion выше делает этот arm unreachable.
            unexpected_row_id => panic!("unknown conditional absence `{unexpected_row_id}`"),
        };
        // Manifest не может сменить card family без отдельного profile decision.
        assert_eq!(
            required_string(absence, "conditional_card_prefix"),
            expected_card_prefix
        );
        // Нулевая card count запрещает выдавать absence за implementation.
        assert_eq!(required_usize(absence, "generated_card_count"), 0);
        // Каждая absence обязана иметь non-empty evidence list.
        let absence_evidence_ids = required_evidence_ids(
            absence,
            "evidence_ids",
            &format!("conditional absence `{row_id}`"),
        );
        // Evidence catalog role обязана оставаться typed exclusion.
        assert_evidence_role(
            evidence_catalog,
            &absence_evidence_ids,
            "exclusion",
            &format!("conditional absence `{row_id}`"),
        );
    }
}

/// ISM live сохраняет canonical provisional и scoped hard exclusion.
#[test]
fn ism_live_remains_canonical_provisional_exclusion() {
    // Загружаем canonical и scoped absence documents.
    let documents = ReleaseScopeDocuments::load();
    // Canonical exclusions нужны для disposition cross-check.
    let excluded_rows = documents.excluded_rows();
    // Extended absences индексируются по stable synthetic identity.
    let extended_absences = documents.extended_absences();
    // Exact ISM live row обязана существовать.
    let ism_live = extended_absences
        .get("ism-mss-live-dvr")
        // Missing row означала бы approved extended gap.
        .expect("ISM live absence обязана существовать");
    // Release не обещает ISM live provider.
    assert_eq!(required_string(ism_live, "status"), "ProfileExcluded");
    // Canonical profile identity хранится явно.
    assert_eq!(
        required_nullable_string(ism_live, "canonical_profile_row_id"),
        Some("ism-mss-live-dvr")
    );
    // S00 status не переписывается внутри S42.
    assert_eq!(
        required_string(ism_live, "canonical_profile_status"),
        "ProfileExcludedProvisional"
    );
    // Canonical exclusion действительно существует с declared status.
    assert_eq!(
        required_string(
            excluded_rows
                .get("ism-mss-live-dvr")
                // Missing row означает stale S42 assertion.
                .expect("canonical ISM live exclusion обязана существовать"),
            "status"
        ),
        "ProfileExcludedProvisional"
    );
}

/// HDS live и special expansion остаются checked-in NoApprovedRow evidence.
#[test]
fn hds_and_special_provider_expansions_have_no_approved_rows() {
    // Загружаем canonical target inventory и scoped absences.
    let documents = ReleaseScopeDocuments::load();
    // Canonical target set нужен для absence assertions.
    let target_rows = documents.target_rows();
    // Extended absences индексируются по stable synthetic identity.
    let extended_absences = documents.extended_absences();

    // Две synthetic rows не имеют approved S00 identities.
    for row_id in ["hds-live-dvr", "approved-special-providers"] {
        // Exact S42 decision row обязана существовать.
        let absence = extended_absences
            .get(row_id)
            // Failure называет отсутствующую decision row.
            .unwrap_or_else(|| panic!("S42 absence `{row_id}` отсутствует"));
        // NoApprovedRow запрещает fake Implemented provider.
        assert_eq!(required_string(absence, "status"), "NoApprovedRow");
        // Explicit null фиксирует отсутствие canonical identity.
        assert_eq!(
            required_nullable_string(absence, "canonical_profile_row_id"),
            None
        );
        // Human/machine-readable status остаётся exact.
        assert_eq!(
            required_string(absence, "canonical_profile_status"),
            "Absent"
        );
        // Synthetic S42 identity не должна случайно совпасть с target row.
        assert!(!target_rows.contains_key(row_id));
    }
}

/// HDS rejected intents и special aliases совпадают с exact profile vocabulary.
#[test]
fn extended_absence_payloads_match_profile_vocabulary() {
    // Загружаем canonical aliases и scoped absences.
    let documents = ReleaseScopeDocuments::load();
    // Extended absences индексируются по stable synthetic identity.
    let extended_absences = documents.extended_absences();
    // HDS live decision row обязана существовать.
    let hds_live = extended_absences
        .get("hds-live-dvr")
        // Missing row означала бы extended acceptance gap.
        .expect("HDS live absence обязана существовать");
    // Exact intent list не позволяет забыть post-live либо incompatible state.
    assert_eq!(
        required_string_array(hds_live, "rejected_live_intents"),
        ["Live", "Upcoming", "PostLive", "Incompatible"]
    );

    // Special absence сверяется с canonical S00 alias family.
    let special_absence = extended_absences
        .get("approved-special-providers")
        // Missing row означала бы special-provider evidence gap.
        .expect("special provider absence обязана существовать");
    // Canonical alias owner остаётся immutable profile.
    let special_alias_family = required_row_by_string_field(
        required_array(&documents.profile, "protocol_aliases"),
        "family",
        "special_private_state_excluded",
        "S00 protocol aliases",
    );
    // S42 не может расширить или сократить special exclusion inventory.
    assert_eq!(
        required_string_array(special_absence, "excluded_protocols"),
        required_string_array(special_alias_family, "aliases")
    );
}
