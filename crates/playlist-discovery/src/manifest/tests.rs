//! Focused D19/D20/D45/D63/D73 tests.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::accounting::{ManifestLimits, RawManifestAccounting};
use super::builder::{build_from_entries_with, native_path_payload_bytes};
use super::enumeration::EnumeratedEntry;
use super::types::{
    AliasPresentationChoice, CandidateSourceDiagnostic, DirectoryManifestBuildError,
    RAW_MANIFEST_MAX_ENTRIES, RAW_MANIFEST_MAX_PATH_KEY_BYTES, RawManifestLimit,
};
use super::{DirectoryManifest, build_directory_manifest};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

/// Unique tempdir без дополнительной production/dev dependency.
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rustiplayer-manifest-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory must be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, []).expect("empty file fixture must be written");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Возвращает exact filename strings только для UTF-8 table fixtures.
fn filenames(manifest: &DirectoryManifest) -> Vec<&str> {
    manifest
        .records()
        .iter()
        .map(|record| {
            record
                .original_locator()
                .file_name()
                .and_then(|name| name.to_str())
                .expect("table fixture filename must be UTF-8")
        })
        .collect()
}

fn fake_entry(path: impl Into<PathBuf>, is_explicit_target: bool) -> EnumeratedEntry {
    EnumeratedEntry {
        original_path: path.into(),
        is_symlink: false,
        is_explicit_target,
    }
}

fn fake_manifest(entries: Vec<EnumeratedEntry>) -> super::builder::BuiltManifest {
    build_from_entries_with(entries, ManifestLimits::PRODUCTION, |path| {
        Ok(path.to_path_buf())
    })
    .expect("fake manifest must build")
}

#[test]
fn numeric_case_order_is_natural_and_exactly_deterministic() {
    let directory = TestDirectory::new("natural");
    directory.file("episode 10.mkv");
    directory.file("episode 2.mkv");
    directory.file("case.mkv");
    let explicit = directory.file("Case.mkv");

    let manifest = build_directory_manifest(&explicit).expect("manifest must build");

    assert_eq!(
        filenames(&manifest),
        ["Case.mkv", "case.mkv", "episode 2.mkv", "episode 10.mkv"]
    );
    for (index, record) in manifest.records().iter().enumerate() {
        assert_eq!(record.candidate_key().get(), index as u32);
        assert_eq!(record.natural_position().get(), index as u32);
    }
}

#[test]
fn hidden_siblings_and_nested_files_are_skipped_but_explicit_hidden_is_retained() {
    let directory = TestDirectory::new("hidden");
    let explicit = directory.file(".explicit.mkv");
    directory.file(".automatic-hidden.mkv");
    directory.file("visible.mkv");
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).expect("nested directory must be created");
    fs::write(nested.join("inside.mkv"), []).expect("nested fixture must be written");

    let manifest = build_directory_manifest(&explicit).expect("manifest must build");

    assert_eq!(filenames(&manifest), [".explicit.mkv", "visible.mkv"]);
    assert_eq!(manifest.explicit_target().original_locator(), explicit);
    assert_eq!(manifest.raw_entry_count(), 4);
}

#[cfg(unix)]
#[test]
fn explicit_locator_preserves_symlink_parent_traversal_semantics() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("symlink-parent-dotdot");
    let resolved_parent = directory.path().join("resolved-parent");
    let symlink_child = resolved_parent.join("symlink-child");
    fs::create_dir_all(&symlink_child).expect("resolved directory fixture must be created");
    let resolved_target = resolved_parent.join("target.mkv");
    fs::write(&resolved_target, []).expect("resolved target fixture must be written");
    let symlink_path = directory.path().join("directory-alias");
    symlink(&symlink_child, &symlink_path).expect("directory symlink fixture must be created");
    let explicit_locator = symlink_path.join("..").join("target.mkv");

    let manifest = build_directory_manifest(&explicit_locator).expect("manifest must build");

    assert_eq!(
        manifest.explicit_target().original_locator(),
        explicit_locator
    );
    assert_eq!(
        fs::canonicalize(manifest.explicit_target().original_locator())
            .expect("preserved locator must resolve"),
        fs::canonicalize(resolved_target).expect("resolved target must canonicalize")
    );
}

#[test]
fn snapshot_membership_ignores_later_create_and_reports_delete_and_rename() {
    let directory = TestDirectory::new("snapshot");
    let explicit = directory.file("target.mkv");
    let deleted = directory.file("delete-me.mkv");
    let renamed = directory.file("rename-me.mkv");
    let manifest = build_directory_manifest(&explicit).expect("manifest must build");
    let initial_locators = manifest
        .records()
        .iter()
        .map(|record| record.original_locator().to_path_buf())
        .collect::<Vec<_>>();
    let deleted_key = manifest
        .records()
        .iter()
        .find(|record| record.original_locator() == deleted)
        .expect("deleted fixture must be present")
        .candidate_key();
    let renamed_key = manifest
        .records()
        .iter()
        .find(|record| record.original_locator() == renamed)
        .expect("renamed fixture must be present")
        .candidate_key();

    directory.file("created-later.mkv");
    fs::remove_file(&deleted).expect("fixture must be deleted");
    fs::rename(&renamed, directory.path().join("renamed-later.mkv"))
        .expect("fixture must be renamed");

    assert_eq!(
        manifest
            .records()
            .iter()
            .map(|record| record.original_locator().to_path_buf())
            .collect::<Vec<_>>(),
        initial_locators
    );
    assert_eq!(
        manifest.validate_candidate_source(deleted_key),
        Err(CandidateSourceDiagnostic::MissingAfterSnapshot {
            candidate_key: deleted_key
        })
    );
    assert_eq!(
        manifest.validate_candidate_source(renamed_key),
        Err(CandidateSourceDiagnostic::MissingAfterSnapshot {
            candidate_key: renamed_key
        })
    );
}

#[cfg(unix)]
#[test]
fn symlink_aliases_merge_and_explicit_original_locator_wins() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("explicit-alias");
    let direct = directory.file("direct.mkv");
    let explicit_alias = directory.path().join("chosen-alias.mkv");
    symlink(&direct, &explicit_alias).expect("symlink fixture must be created");

    let manifest = build_directory_manifest(&explicit_alias).expect("manifest must build");

    assert_eq!(manifest.records().len(), 1);
    assert_eq!(manifest.records()[0].original_locator(), explicit_alias);
    assert_eq!(
        manifest.records()[0]
            .alias_diagnostics()
            .presentation_choice(),
        AliasPresentationChoice::ExplicitTarget
    );
    assert_eq!(
        manifest.records()[0]
            .alias_diagnostics()
            .original_entry_count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn automatic_group_prefers_direct_then_deterministic_original_alias() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("automatic-alias");
    let explicit = directory.file("explicit.mkv");
    let direct = directory.file("direct.mkv");
    symlink(&direct, directory.path().join("alias.mkv")).expect("direct alias must be created");
    let outside = TestDirectory::new("outside-alias");
    let outside_file = outside.file("outside.mkv");
    let alias_ten = directory.path().join("outside 10.mkv");
    let alias_two = directory.path().join("outside 2.mkv");
    symlink(&outside_file, &alias_ten).expect("first outside alias must be created");
    symlink(&outside_file, &alias_two).expect("second outside alias must be created");

    let manifest = build_directory_manifest(&explicit).expect("manifest must build");
    let direct_group = manifest
        .records()
        .iter()
        .find(|record| record.original_locator() == direct)
        .expect("automatic direct group must choose direct entry");
    let alias_group = manifest
        .records()
        .iter()
        .find(|record| record.original_locator() == alias_two)
        .expect("alias-only group must choose natural first alias");

    assert_eq!(
        direct_group.alias_diagnostics().presentation_choice(),
        AliasPresentationChoice::DirectEntry
    );
    assert_eq!(
        alias_group.alias_diagnostics().presentation_choice(),
        AliasPresentationChoice::DeterministicAlias
    );
    assert!(
        !manifest
            .records()
            .iter()
            .any(|record| record.original_locator() == alias_ten)
    );
}

#[cfg(unix)]
#[test]
fn hardlinks_remain_distinct_and_symlink_retarget_is_source_changed() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("hardlink-retarget");
    let first = directory.file("first.mkv");
    let second = directory.file("second.mkv");
    let hardlink = directory.path().join("hardlink.mkv");
    fs::hard_link(&first, &hardlink).expect("hardlink fixture must be created");
    let explicit_alias = directory.path().join("target-link.mkv");
    symlink(&first, &explicit_alias).expect("target symlink must be created");
    let manifest = build_directory_manifest(&explicit_alias).expect("manifest must build");
    let target_key = manifest.explicit_target().candidate_key();

    assert!(
        manifest
            .records()
            .iter()
            .any(|record| record.original_locator() == hardlink)
    );
    fs::remove_file(&explicit_alias).expect("old target symlink must be removed");
    symlink(&second, &explicit_alias).expect("target symlink must be retargeted");
    assert_eq!(
        manifest.validate_candidate_source(target_key),
        Err(CandidateSourceDiagnostic::SourceChangedAfterSnapshot {
            candidate_key: target_key
        })
    );
}

#[test]
fn canonicalization_fallback_preserves_original_and_typed_diagnostic() {
    let directory = TestDirectory::new("canonical-fallback");
    let missing_explicit = directory.path().join("missing-explicit.mkv");

    let manifest = build_directory_manifest(&missing_explicit)
        .expect("missing explicit locator must survive as fallback record");
    let target = manifest.explicit_target();

    assert_eq!(target.original_locator(), missing_explicit);
    assert_eq!(
        target.alias_diagnostics().canonicalization_failure(),
        Some(io::ErrorKind::NotFound)
    );
}

#[test]
fn mixed_alias_group_validation_uses_chosen_locator_canonicalization_outcome() {
    let target = fake_entry("/music/target.mkv", true);
    let fallback_alias = EnumeratedEntry {
        original_path: PathBuf::from("/music/a-alias.mkv"),
        is_symlink: true,
        is_explicit_target: false,
    };
    let direct = fake_entry("/music/direct.mkv", false);
    let shared_identity = PathBuf::from("/music/a-alias.mkv");

    let built = build_from_entries_with(
        vec![target, fallback_alias, direct],
        ManifestLimits::PRODUCTION,
        |path| match path.file_name().and_then(|name| name.to_str()) {
            Some("a-alias.mkv") => Err(io::ErrorKind::PermissionDenied),
            Some("direct.mkv") => Ok(shared_identity.clone()),
            _ => Ok(path.to_path_buf()),
        },
    )
    .expect("mixed alias group must build");
    let direct_position = built
        .records
        .iter()
        .position(|record| record.original_locator() == Path::new("/music/direct.mkv"))
        .expect("successful direct locator must be selected");

    assert!(built.validation_identities[direct_position].canonicalization_succeeded);
}

#[cfg(unix)]
#[test]
fn non_utf8_filename_keys_are_exact_and_natural() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let directory = TestDirectory::new("non-utf");
    let invalid_ten = directory
        .path()
        .join(OsString::from_vec(b"episode 10-\xff.mkv".to_vec()));
    let invalid_two = directory
        .path()
        .join(OsString::from_vec(b"episode 2-\xfe.mkv".to_vec()));
    fs::write(&invalid_ten, []).expect("first non-UTF fixture must be written");
    fs::write(&invalid_two, []).expect("second non-UTF fixture must be written");

    let manifest = build_directory_manifest(&invalid_ten).expect("manifest must build");

    assert_eq!(manifest.records()[0].original_locator(), invalid_two);
    assert_eq!(manifest.records()[1].original_locator(), invalid_ten);
}

#[test]
fn shuffled_input_has_identical_records_keys_and_order() {
    let original = vec![
        fake_entry("/music/item 10.mkv", false),
        fake_entry("/music/Item 02.mkv", true),
        fake_entry("/music/item 2.mkv", false),
        fake_entry("/music/Case.mkv", false),
    ];
    let mut reversed = original.clone();
    reversed.reverse();
    let first = fake_manifest(original);
    let second = fake_manifest(reversed);

    let first_records = first
        .records
        .iter()
        .map(|record| {
            (
                record.candidate_key(),
                record.natural_position(),
                record.original_locator().to_path_buf(),
            )
        })
        .collect::<Vec<_>>();
    let second_records = second
        .records
        .iter()
        .map(|record| {
            (
                record.candidate_key(),
                record.natural_position(),
                record.original_locator().to_path_buf(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(first_records, second_records);
}

#[test]
fn exact_entry_limit_accepts_100000_and_rejects_100001() {
    let limits = ManifestLimits {
        max_entries: RAW_MANIFEST_MAX_ENTRIES,
        max_path_key_bytes: usize::MAX,
    };
    let mut accounting = RawManifestAccounting::default();
    for _ in 0..RAW_MANIFEST_MAX_ENTRIES {
        accounting
            .add_entry(0, 0, limits)
            .expect("exact entry boundary must fit");
    }

    let error = accounting
        .add_entry(0, 0, limits)
        .expect_err("entry boundary + 1 must fail");
    assert_eq!(accounting.entry_count(), RAW_MANIFEST_MAX_ENTRIES);
    assert_eq!(error.limit(), RawManifestLimit::EntryCount);
    assert_eq!(error.observed_at_least(), RAW_MANIFEST_MAX_ENTRIES + 1);
}

#[test]
fn exact_byte_boundary_plus_one_and_oversized_single_path_are_typed() {
    let limits = ManifestLimits {
        max_entries: 2,
        max_path_key_bytes: RAW_MANIFEST_MAX_PATH_KEY_BYTES,
    };
    let mut exact = RawManifestAccounting::default();
    exact
        .add_entry(RAW_MANIFEST_MAX_PATH_KEY_BYTES, 0, limits)
        .expect("exact byte boundary must fit");
    let plus_one = exact
        .add_entry(1, 0, limits)
        .expect_err("byte boundary + 1 must fail");
    assert_eq!(plus_one.limit(), RawManifestLimit::PathKeyBytes);
    assert_eq!(exact.path_key_bytes(), RAW_MANIFEST_MAX_PATH_KEY_BYTES);

    let mut oversized = RawManifestAccounting::default();
    let oversized_error = oversized
        .add_entry(RAW_MANIFEST_MAX_PATH_KEY_BYTES + 1, 0, limits)
        .expect_err("one oversized path must fail without retention");
    assert_eq!(oversized_error.limit(), RawManifestLimit::PathKeyBytes);
    assert_eq!(oversized.path_key_bytes(), 0);
    assert_eq!(oversized.entry_count(), 0);
}

#[test]
fn checked_arithmetic_overflow_is_typed_and_atomic() {
    let limits = ManifestLimits {
        max_entries: usize::MAX,
        max_path_key_bytes: usize::MAX,
    };
    let mut entry_overflow = RawManifestAccounting::with_state(usize::MAX, 0);
    let error = entry_overflow
        .add_entry(0, 0, limits)
        .expect_err("entry checked_add overflow must fail");
    assert_eq!(error.limit(), RawManifestLimit::CheckedArithmetic);
    assert_eq!(entry_overflow.entry_count(), usize::MAX);

    let mut byte_overflow = RawManifestAccounting::with_state(0, usize::MAX);
    let error = byte_overflow
        .add_entry(1, 0, limits)
        .expect_err("byte checked_add overflow must fail");
    assert_eq!(error.limit(), RawManifestLimit::CheckedArithmetic);
    assert_eq!(byte_overflow.path_key_bytes(), usize::MAX);
    assert_eq!(byte_overflow.entry_count(), 0);
}

#[test]
fn overflow_returns_no_arbitrary_prefix_for_any_input_order() {
    let limits = ManifestLimits {
        max_entries: 2,
        max_path_key_bytes: usize::MAX,
    };
    let entries = vec![
        fake_entry("/music/target.mkv", true),
        fake_entry("/music/two.mkv", false),
        fake_entry("/music/three.mkv", false),
    ];
    let mut shuffled = entries.clone();
    shuffled.swap(1, 2);

    for order in [entries, shuffled] {
        let error = build_from_entries_with(order, limits, |path| Ok(path.to_path_buf()))
            .err()
            .expect("overflow must not return a partial manifest");
        assert!(matches!(
            error,
            DirectoryManifestBuildError::RawManifestLimitReached(reached)
                if reached.limit() == RawManifestLimit::EntryCount
                    && reached.observed_at_least() == 3
        ));
    }
}

#[test]
fn retained_accounting_includes_original_key_and_unique_canonical_path() {
    let target = PathBuf::from("/music/target.mkv");
    let built = fake_manifest(vec![fake_entry(target.clone(), true)]);
    let path_bytes = native_path_payload_bytes(&target).expect("path size must fit");
    let filename_key_bytes = natural_sort_key::PreparedNaturalKey::from_os_str(
        target.file_name().expect("target filename must exist"),
    )
    .retained_bytes();

    assert_eq!(built.accounting.entry_count(), 1);
    assert_eq!(
        built.accounting.path_key_bytes(),
        path_bytes + filename_key_bytes + path_bytes
    );
    assert!(built.accounting.path_key_bytes() <= RAW_MANIFEST_MAX_PATH_KEY_BYTES);
}
