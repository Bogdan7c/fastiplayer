//! Prepared total natural key с reversible native/foreign locator fallback.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::{
    ForeignPathEncoding, ForeignPathPlatform, LocalLocator, PlaylistItem, PlaylistItemId,
    PlaylistLocator,
};

/// Полный total fallback natural name -> exact name -> locator -> Item ID.
pub(super) struct NaturalSortKey {
    tokens: Vec<NaturalToken>,
    exact_name: ExactNameKey,
    exact_locator: ExactLocatorKey,
    item_id: PlaylistItemId,
}

impl Ord for NaturalSortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tokens
            .cmp(&other.tokens)
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

/// Максимальные alternating text/numeric runs natural name.
#[derive(PartialEq, Eq)]
enum NaturalToken {
    /// ASCII numeric run без leading zeroes; all-zero run представлен одним zero.
    Number(Vec<u32>),
    /// Case-folded Unicode scalar или exact non-UTF platform units.
    Text(Vec<u32>),
}

impl Ord for NaturalToken {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => {
                left.len().cmp(&right.len()).then_with(|| left.cmp(right))
            }
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
            (Self::Number(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Number(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for NaturalToken {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

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
    let tokens = tokenize_exact_name(&name_units);

    NaturalSortKey {
        tokens,
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

/// Tokenization dispatch без lossy conversion.
fn tokenize_exact_name(exact_name: &ExactNameKey) -> Vec<NaturalToken> {
    match exact_name {
        ExactNameKey::Utf8(name) => tokenize_utf8(name),
        ExactNameKey::Native(name) => tokenize_native_os_string(name),
        ExactNameKey::Foreign(units) => tokenize_foreign_units(units),
    }
}

/// Unicode-valid filename: lowercase один раз, numeric policy остаётся ASCII-defined.
fn tokenize_utf8(name: &str) -> Vec<NaturalToken> {
    let folded_units = name
        .chars()
        .flat_map(char::to_lowercase)
        .map(u32::from)
        .collect::<Vec<_>>();
    tokenize_units(&folded_units)
}

#[cfg(unix)]
fn tokenize_native_os_string(name: &std::ffi::OsStr) -> Vec<NaturalToken> {
    use std::os::unix::ffi::OsStrExt;

    let folded_units = name
        .as_bytes()
        .iter()
        .map(|byte| u32::from(byte.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    tokenize_units(&folded_units)
}

#[cfg(windows)]
fn tokenize_native_os_string(name: &std::ffi::OsStr) -> Vec<NaturalToken> {
    use std::os::windows::ffi::OsStrExt;

    let folded_units = name
        .encode_wide()
        .map(|unit| ascii_lowercase_unit(u32::from(unit)))
        .collect::<Vec<_>>();
    tokenize_units(&folded_units)
}

#[cfg(not(any(unix, windows)))]
fn tokenize_native_os_string(name: &std::ffi::OsStr) -> Vec<NaturalToken> {
    tokenize_utf8(&name.to_string_lossy())
}

/// Foreign UTF-8 получает Unicode policy, остальные encodings — ASCII fold exact units.
fn tokenize_foreign_units(units: &ExactForeignUnits) -> Vec<NaturalToken> {
    match units {
        ExactForeignUnits::Utf8(name) => tokenize_utf8(name),
        ExactForeignUnits::Bytes(bytes) => {
            let folded_units = bytes
                .iter()
                .map(|byte| u32::from(byte.to_ascii_lowercase()))
                .collect::<Vec<_>>();
            tokenize_units(&folded_units)
        }
        ExactForeignUnits::Wide(units) => {
            let folded_units = units
                .iter()
                .map(|unit| ascii_lowercase_unit(u32::from(*unit)))
                .collect::<Vec<_>>();
            tokenize_units(&folded_units)
        }
        ExactForeignUnits::Opaque { raw_units, .. } => {
            let folded_units = raw_units
                .iter()
                .copied()
                .map(ascii_lowercase_unit)
                .collect::<Vec<_>>();
            tokenize_units(&folded_units)
        }
    }
}

/// Делит units на maximal ASCII digit и non-digit runs.
fn tokenize_units(units: &[u32]) -> Vec<NaturalToken> {
    let mut tokens = Vec::new();
    let mut run_start = 0;

    while run_start < units.len() {
        let run_is_numeric = is_ascii_digit(units[run_start]);
        let mut run_end = run_start + 1;
        while run_end < units.len() && is_ascii_digit(units[run_end]) == run_is_numeric {
            run_end += 1;
        }

        if run_is_numeric {
            tokens.push(NaturalToken::Number(normalize_numeric_run(
                &units[run_start..run_end],
            )));
        } else {
            tokens.push(NaturalToken::Text(units[run_start..run_end].to_vec()));
        }
        run_start = run_end;
    }

    tokens
}

/// Убирает leading zeroes без integer parsing и риска overflow.
fn normalize_numeric_run(run: &[u32]) -> Vec<u32> {
    let first_significant = run.iter().position(|unit| *unit != u32::from(b'0'));
    match first_significant {
        Some(index) => run[index..].to_vec(),
        None => vec![u32::from(b'0')],
    }
}

/// ASCII digit policy одинаков для UTF-8, bytes, wide и opaque units.
const fn is_ascii_digit(unit: u32) -> bool {
    unit >= b'0' as u32 && unit <= b'9' as u32
}

/// Lowercase только ASCII code points внутри non-UTF encodings.
const fn ascii_lowercase_unit(unit: u32) -> u32 {
    if unit >= b'A' as u32 && unit <= b'Z' as u32 {
        unit + (b'a' - b'A') as u32
    } else {
        unit
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
