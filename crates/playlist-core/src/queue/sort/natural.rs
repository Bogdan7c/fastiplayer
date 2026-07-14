//! Prepared total natural key с reversible native/foreign locator fallback.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use natural_sort_key::PreparedNaturalKey;

use crate::{
    ForeignPathEncoding, ForeignPathPlatform, LocalLocator, PlaylistItem, PlaylistItemId,
    PlaylistLocator,
};

/// Полный total fallback natural name -> exact name -> locator -> Item ID.
pub(super) struct NaturalSortKey {
    natural_name: PreparedNaturalKey,
    exact_name: ExactNameKey,
    exact_locator: ExactLocatorKey,
    item_id: PlaylistItemId,
}

impl Ord for NaturalSortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.natural_name
            .cmp(&other.natural_name)
            .then_with(|| self.exact_name.cmp(&other.exact_name))
            .then_with(|| self.exact_locator.cmp(&other.exact_locator))
            .then_with(|| self.item_id.cmp(&other.item_id))
    }
}

impl PartialOrd for NaturalSortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for NaturalSortKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NaturalSortKey {}

/// Exact filename units без lossy UTF-8 conversion.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ExactNameKey {
    Utf8(String),
    Native(std::ffi::OsString),
    Foreign(ExactForeignUnits),
}

/// Exact locator identity; secret-bearing value никогда не форматируется наружу.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ExactLocatorKey {
    Native(PathBuf),
    Foreign {
        platform: ExactForeignPlatform,
        units: ExactForeignUnits,
    },
    Url(String),
}

/// Ord-capable копия foreign platform vocabulary.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ExactForeignPlatform {
    Linux,
    MacOs,
    Windows,
    Other(String),
}

/// Ord-capable reversible foreign path units.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ExactForeignUnits {
    Utf8(String),
    Bytes(Vec<u8>),
    Wide(Vec<u16>),
    Opaque {
        encoding_name: String,
        raw_units: Vec<u32>,
    },
}

/// Готовит natural + exact keys, сохраняя native/foreign/URL identity.
pub(super) fn prepare_natural_sort_key(item: &PlaylistItem) -> NaturalSortKey {
    let fallback_name = item.cached_metadata().fallback_display_name();
    let (name_units, exact_locator) = match item.locator() {
        PlaylistLocator::Local(LocalLocator::Native(path)) => {
            let filename = native_filename(path);
            (
                exact_name_from_native(filename, fallback_name),
                ExactLocatorKey::Native(path.clone()),
            )
        }
        PlaylistLocator::Local(LocalLocator::Foreign(path)) => {
            let exact_units = exact_foreign_units(path.encoding_for_persistence());
            let filename_units = foreign_filename_units(&exact_units);
            (
                ExactNameKey::Foreign(filename_units),
                ExactLocatorKey::Foreign {
                    platform: exact_foreign_platform(path.platform_for_persistence()),
                    units: exact_units,
                },
            )
        }
        PlaylistLocator::Url(secret_url) => (
            ExactNameKey::Utf8(fallback_name.to_owned()),
            ExactLocatorKey::Url(secret_url.expose_secret_for_persistence().to_owned()),
        ),
    };
    let natural_name = prepare_neutral_natural_key(&name_units);

    NaturalSortKey {
        natural_name,
        exact_name: name_units,
        exact_locator,
        item_id: item.item_id(),
    }
}

/// Берёт basename, а для path без basename использует exact path units.
fn native_filename(path: &Path) -> &std::ffi::OsStr {
    path.file_name().unwrap_or(path.as_os_str())
}

/// Valid UTF-8 получает Unicode case-fold; invalid native units остаются exact.
fn exact_name_from_native(filename: &std::ffi::OsStr, fallback_name: &str) -> ExactNameKey {
    match filename.to_str() {
        Some(valid_utf8) => ExactNameKey::Utf8(valid_utf8.to_owned()),
        None if filename.is_empty() => ExactNameKey::Utf8(fallback_name.to_owned()),
        None => ExactNameKey::Native(filename.to_owned()),
    }
}

/// Делегирует единую natural semantics neutral std-only crate-у.
fn prepare_neutral_natural_key(exact_name: &ExactNameKey) -> PreparedNaturalKey {
    match exact_name {
        ExactNameKey::Utf8(name) => PreparedNaturalKey::from_utf8(name),
        ExactNameKey::Native(name) => PreparedNaturalKey::from_os_str(name),
        ExactNameKey::Foreign(ExactForeignUnits::Utf8(name)) => PreparedNaturalKey::from_utf8(name),
        ExactNameKey::Foreign(ExactForeignUnits::Bytes(units)) => {
            PreparedNaturalKey::from_bytes(units)
        }
        ExactNameKey::Foreign(ExactForeignUnits::Wide(units)) => {
            PreparedNaturalKey::from_wide_units(units)
        }
        ExactNameKey::Foreign(ExactForeignUnits::Opaque { raw_units, .. }) => {
            PreparedNaturalKey::from_opaque_units(raw_units)
        }
    }
}

/// Копирует foreign platform в локальный total-order key.
fn exact_foreign_platform(platform: &ForeignPathPlatform) -> ExactForeignPlatform {
    match platform {
        ForeignPathPlatform::Linux => ExactForeignPlatform::Linux,
        ForeignPathPlatform::MacOs => ExactForeignPlatform::MacOs,
        ForeignPathPlatform::Windows => ExactForeignPlatform::Windows,
        ForeignPathPlatform::Other(name) => ExactForeignPlatform::Other(name.clone()),
    }
}

/// Копирует exact foreign units без lossy conversion.
fn exact_foreign_units(encoding: &ForeignPathEncoding) -> ExactForeignUnits {
    match encoding {
        ForeignPathEncoding::Utf8(path) => ExactForeignUnits::Utf8(path.clone()),
        ForeignPathEncoding::Bytes(units) => ExactForeignUnits::Bytes(units.clone()),
        ForeignPathEncoding::Wide(units) => ExactForeignUnits::Wide(units.clone()),
        ForeignPathEncoding::Opaque {
            encoding_name,
            raw_units,
        } => ExactForeignUnits::Opaque {
            encoding_name: encoding_name.clone(),
            raw_units: raw_units.clone(),
        },
    }
}

/// Извлекает basename по reversible separator units известного encoding.
fn foreign_filename_units(units: &ExactForeignUnits) -> ExactForeignUnits {
    match units {
        ExactForeignUnits::Utf8(path) => {
            ExactForeignUnits::Utf8(path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned())
        }
        ExactForeignUnits::Bytes(path) => {
            ExactForeignUnits::Bytes(last_units_after_separator(path, b'/', b'\\'))
        }
        ExactForeignUnits::Wide(path) => ExactForeignUnits::Wide(last_units_after_separator(
            path,
            u16::from(b'/'),
            u16::from(b'\\'),
        )),
        ExactForeignUnits::Opaque {
            encoding_name,
            raw_units,
        } => ExactForeignUnits::Opaque {
            encoding_name: encoding_name.clone(),
            raw_units: raw_units.clone(),
        },
    }
}

/// Возвращает units после последнего Unix/Windows separator.
fn last_units_after_separator<T: Copy + PartialEq>(
    units: &[T],
    unix_separator: T,
    windows_separator: T,
) -> Vec<T> {
    let basename_start = units
        .iter()
        .rposition(|unit| *unit == unix_separator || *unit == windows_separator)
        .map_or(0, |separator_index| separator_index + 1);
    units[basename_start..].to_vec()
}
