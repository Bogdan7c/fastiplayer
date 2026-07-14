//! Deterministic natural grouping, D45 locator selection и bounded ownership.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use natural_sort_key::PreparedNaturalKey;

use super::accounting::{ManifestLimits, RawManifestAccounting};
use super::enumeration::EnumeratedEntry;
#[cfg(windows)]
use super::types::RawManifestLimit;
use super::types::{
    AliasPresentationChoice, DirectoryManifestBuildError, ManifestAliasDiagnostics, ManifestRecord,
    RawManifestLimitReached,
};

/// Полный builder output; facade превращает его в immutable manifest.
pub(super) struct BuiltManifest {
    pub(super) records: Box<[ManifestRecord]>,
    pub(super) validation_identities: Box<[ValidationIdentity]>,
    pub(super) target_position: usize,
    pub(super) accounting: RawManifestAccounting,
}

/// Private transient canonical identity для post-snapshot targeted validation.
pub(super) struct ValidationIdentity {
    pub(super) expected_canonical_path: PathBuf,
    pub(super) canonicalization_succeeded: bool,
}

/// Один original alias с compact prepared natural key.
struct PreparedAlias {
    original_path: PathBuf,
    exact_filename: OsString,
    natural_key: PreparedNaturalKey,
    is_symlink: bool,
    is_explicit_target: bool,
    canonicalization_failure: Option<io::ErrorKind>,
}

/// Aliases одной transient canonical identity.
struct AliasGroup {
    aliases: Vec<PreparedAlias>,
}

/// Streaming builder применяет D73 до удержания следующего directory entry.
pub(super) struct ManifestBuilder {
    accounting: RawManifestAccounting,
    groups: BTreeMap<PathBuf, AliasGroup>,
    limits: ManifestLimits,
}

impl ManifestBuilder {
    pub(super) fn new(limits: ManifestLimits) -> Self {
        Self {
            accounting: RawManifestAccounting::default(),
            groups: BTreeMap::new(),
            limits,
        }
    }

    /// Production push канонизирует ровно текущий bounded entry.
    pub(super) fn push(
        &mut self,
        entry: EnumeratedEntry,
    ) -> Result<(), DirectoryManifestBuildError> {
        self.push_with(entry, |path| {
            fs::canonicalize(path).map_err(|error| error.kind())
        })
    }

    /// Hidden/non-file/error entry расходует raw count, но не retained payload bytes.
    pub(super) fn observe_skipped_entry(&mut self) -> Result<(), DirectoryManifestBuildError> {
        self.accounting.add_entry(0, 0, self.limits)?;
        Ok(())
    }

    fn push_with<F>(
        &mut self,
        entry: EnumeratedEntry,
        mut canonicalize: F,
    ) -> Result<(), DirectoryManifestBuildError>
    where
        F: FnMut(&Path) -> Result<PathBuf, io::ErrorKind>,
    {
        let exact_filename = candidate_filename(&entry.original_path).to_owned();
        let natural_key = PreparedNaturalKey::from_os_str(&exact_filename);
        self.accounting.add_entry(
            native_path_payload_bytes(&entry.original_path)?,
            natural_key.retained_bytes(),
            self.limits,
        )?;

        let (canonical_identity, canonicalization_failure) =
            match canonicalize(&entry.original_path) {
                Ok(canonical_path) => (canonical_path, None),
                Err(error_kind) => (entry.original_path.clone(), Some(error_kind)),
            };
        let prepared_alias = PreparedAlias {
            original_path: entry.original_path,
            exact_filename,
            natural_key,
            is_symlink: entry.is_symlink,
            is_explicit_target: entry.is_explicit_target,
            canonicalization_failure,
        };

        match self.groups.entry(canonical_identity) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                self.accounting
                    .add_canonical_identity(native_path_payload_bytes(slot.key())?, self.limits)?;
                slot.insert(AliasGroup {
                    aliases: vec![prepared_alias],
                });
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                slot.get_mut().aliases.push(prepared_alias);
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<BuiltManifest, DirectoryManifestBuildError> {
        finish_groups(self.groups, self.accounting)
    }
}

/// Группа после D45 locator selection, но до final natural sort.
struct SelectedGroup {
    canonical_identity: PathBuf,
    chosen: PreparedAlias,
    alias_count: usize,
    presentation_choice: AliasPresentationChoice,
    canonicalization_succeeded: bool,
}

/// Injectable canonicalizer делает ordering/limit tests hermetic.
#[cfg(test)]
pub(super) fn build_from_entries_with<F>(
    entries: Vec<EnumeratedEntry>,
    limits: ManifestLimits,
    mut canonicalize: F,
) -> Result<BuiltManifest, DirectoryManifestBuildError>
where
    F: FnMut(&Path) -> Result<PathBuf, io::ErrorKind>,
{
    let mut builder = ManifestBuilder::new(limits);
    for entry in entries {
        builder.push_with(entry, &mut canonicalize)?;
    }
    builder.finish()
}

/// Назначает stable keys только после полного bounded success.
fn finish_groups(
    groups: BTreeMap<PathBuf, AliasGroup>,
    accounting: RawManifestAccounting,
) -> Result<BuiltManifest, DirectoryManifestBuildError> {
    let mut selected_groups = groups
        .into_iter()
        .map(|(canonical_identity, group)| select_presentation_alias(canonical_identity, group))
        .collect::<Vec<_>>();
    selected_groups.sort_by(compare_selected_groups);

    let target_position = selected_groups
        .iter()
        .position(|group| group.chosen.is_explicit_target)
        .ok_or(DirectoryManifestBuildError::InvalidExplicitTarget)?;
    let mut records = Vec::with_capacity(selected_groups.len());
    let mut validation_identities = Vec::with_capacity(selected_groups.len());

    for (position, group) in selected_groups.into_iter().enumerate() {
        let diagnostics = ManifestAliasDiagnostics::new(
            group.alias_count,
            group.presentation_choice,
            group.chosen.canonicalization_failure,
        );
        records.push(ManifestRecord::new(
            position,
            group.chosen.original_path,
            diagnostics,
        ));
        validation_identities.push(ValidationIdentity {
            expected_canonical_path: group.canonical_identity,
            canonicalization_succeeded: group.canonicalization_succeeded,
        });
    }

    Ok(BuiltManifest {
        records: records.into_boxed_slice(),
        validation_identities: validation_identities.into_boxed_slice(),
        target_position,
        accounting,
    })
}

/// Explicit > direct > deterministic natural/exact alias.
fn select_presentation_alias(canonical_identity: PathBuf, mut group: AliasGroup) -> SelectedGroup {
    group.aliases.sort_by(compare_aliases);
    let alias_count = group.aliases.len();
    let (chosen_index, presentation_choice) = if alias_count == 1 {
        (0, AliasPresentationChoice::SoleEntry)
    } else if let Some(index) = group
        .aliases
        .iter()
        .position(|alias| alias.is_explicit_target)
    {
        (index, AliasPresentationChoice::ExplicitTarget)
    } else if let Some(index) = group.aliases.iter().position(|alias| !alias.is_symlink) {
        (index, AliasPresentationChoice::DirectEntry)
    } else {
        (0, AliasPresentationChoice::DeterministicAlias)
    };
    let chosen = group.aliases.remove(chosen_index);
    let canonicalization_succeeded = chosen.canonicalization_failure.is_none();

    SelectedGroup {
        canonical_identity,
        chosen,
        alias_count,
        presentation_choice,
        canonicalization_succeeded,
    }
}

/// Общий neutral natural comparator + owner-specific exact native tie-breakers.
fn compare_aliases(left: &PreparedAlias, right: &PreparedAlias) -> Ordering {
    left.natural_key
        .cmp(&right.natural_key)
        .then_with(|| left.exact_filename.cmp(&right.exact_filename))
        .then_with(|| left.original_path.cmp(&right.original_path))
}

/// Final manifest order определяется выбранным original locator группы.
fn compare_selected_groups(left: &SelectedGroup, right: &SelectedGroup) -> Ordering {
    compare_aliases(&left.chosen, &right.chosen)
}

/// Filename отсутствует только у unusual path; exact path остаётся fallback.
fn candidate_filename(path: &Path) -> &OsStr {
    path.file_name().unwrap_or(path.as_os_str())
}

/// Считает native payload без lossy conversion и checked wide multiplication.
pub(super) fn native_path_payload_bytes(path: &Path) -> Result<usize, RawManifestLimitReached> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        Ok(path.as_os_str().as_bytes().len())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        path.as_os_str()
            .encode_wide()
            .count()
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or_else(|| {
                RawManifestLimitReached::new(RawManifestLimit::CheckedArithmetic, usize::MAX)
            })
    }
    #[cfg(not(any(unix, windows)))]
    Ok(path.as_os_str().len())
}
