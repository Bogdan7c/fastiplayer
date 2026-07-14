//! Компактный std-only natural key для имён файлов.
//!
//! Crate намеренно не добавляет exact/path/domain tie-breakers: каждый владелец
//! дополняет natural comparison своей точной identity в месте использования.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::mem::size_of;

/// Подготовленный case-folded key без allocation-heavy token tree.
///
/// Numeric и text runs выделяются непосредственно во время сравнения. Поэтому
/// одно имя удерживает только один compact buffer native/Unicode units.
#[derive(Clone, Debug)]
pub struct PreparedNaturalKey {
    units: CompactUnits,
}

/// Наиболее компактное обратимое представление folded units исходного имени.
#[derive(Clone, Debug)]
enum CompactUnits {
    Bytes(Box<[u8]>),
    Wide(Box<[u16]>),
    Unicode(Box<[u32]>),
}

impl PreparedNaturalKey {
    /// Готовит Unicode-aware key для корректной UTF-8 строки.
    #[must_use]
    pub fn from_utf8(name: &str) -> Self {
        let units = name
            .chars()
            .flat_map(char::to_lowercase)
            .map(u32::from)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            units: CompactUnits::Unicode(units),
        }
    }

    /// Готовит key для exact byte encoding с ASCII-only case folding.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let units = bytes
            .iter()
            .map(u8::to_ascii_lowercase)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            units: CompactUnits::Bytes(units),
        }
    }

    /// Готовит key для exact wide encoding с ASCII-only case folding.
    #[must_use]
    pub fn from_wide_units(units: &[u16]) -> Self {
        let units = units
            .iter()
            .copied()
            .map(ascii_lowercase_u16)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            units: CompactUnits::Wide(units),
        }
    }

    /// Готовит key для opaque 32-bit units с ASCII-only case folding.
    #[must_use]
    pub fn from_opaque_units(units: &[u32]) -> Self {
        let units = units
            .iter()
            .copied()
            .map(ascii_lowercase_u32)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            units: CompactUnits::Unicode(units),
        }
    }

    /// Готовит native filename key без lossy conversion на Unix/Windows.
    #[must_use]
    pub fn from_os_str(name: &OsStr) -> Self {
        if let Some(valid_utf8) = name.to_str() {
            return Self::from_utf8(valid_utf8);
        }
        Self::from_non_utf_os_str(name)
    }

    /// Возвращает exact payload bytes, которые учитывает bounded manifest.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        match &self.units {
            CompactUnits::Bytes(units) => units.len(),
            CompactUnits::Wide(units) => units.len().saturating_mul(size_of::<u16>()),
            CompactUnits::Unicode(units) => units.len().saturating_mul(size_of::<u32>()),
        }
    }

    /// Число logical folded units для comparator cursor.
    fn len(&self) -> usize {
        match &self.units {
            CompactUnits::Bytes(units) => units.len(),
            CompactUnits::Wide(units) => units.len(),
            CompactUnits::Unicode(units) => units.len(),
        }
    }

    /// Читает один unit в общей comparison domain без дополнительного buffer.
    fn unit(&self, index: usize) -> u32 {
        match &self.units {
            CompactUnits::Bytes(units) => u32::from(units[index]),
            CompactUnits::Wide(units) => u32::from(units[index]),
            CompactUnits::Unicode(units) => units[index],
        }
    }

    #[cfg(unix)]
    fn from_non_utf_os_str(name: &OsStr) -> Self {
        use std::os::unix::ffi::OsStrExt;

        Self::from_bytes(name.as_bytes())
    }

    #[cfg(windows)]
    fn from_non_utf_os_str(name: &OsStr) -> Self {
        use std::os::windows::ffi::OsStrExt;

        Self::from_wide_units(&name.encode_wide().collect::<Vec<_>>())
    }

    #[cfg(not(any(unix, windows)))]
    fn from_non_utf_os_str(name: &OsStr) -> Self {
        Self::from_utf8(&name.to_string_lossy())
    }
}

impl Ord for PreparedNaturalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_natural_units(self, other)
    }
}

impl PartialOrd for PreparedNaturalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for PreparedNaturalKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PreparedNaturalKey {}

/// Сравнивает maximal numeric/text runs без materialized token allocations.
fn compare_natural_units(left: &PreparedNaturalKey, right: &PreparedNaturalKey) -> Ordering {
    let mut left_start = 0;
    let mut right_start = 0;

    while left_start < left.len() && right_start < right.len() {
        let left_is_number = is_ascii_digit(left.unit(left_start));
        let right_is_number = is_ascii_digit(right.unit(right_start));
        if left_is_number != right_is_number {
            return if left_is_number {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }

        let left_end = run_end(left, left_start, left_is_number);
        let right_end = run_end(right, right_start, right_is_number);
        let ordering = if left_is_number {
            compare_numeric_run(left, left_start, left_end, right, right_start, right_end)
        } else {
            compare_unit_range(left, left_start, left_end, right, right_start, right_end)
        };
        if ordering != Ordering::Equal {
            return ordering;
        }

        left_start = left_end;
        right_start = right_end;
    }

    if left_start == left.len() {
        if right_start == right.len() {
            Ordering::Equal
        } else {
            Ordering::Less
        }
    } else {
        Ordering::Greater
    }
}

/// Находит конец maximal run того же numeric/text класса.
fn run_end(key: &PreparedNaturalKey, start: usize, is_number: bool) -> usize {
    let mut end = start + 1;
    while end < key.len() && is_ascii_digit(key.unit(end)) == is_number {
        end += 1;
    }
    end
}

/// Сравнивает числа по significant length и digits без integer parsing/overflow.
fn compare_numeric_run(
    left: &PreparedNaturalKey,
    left_start: usize,
    left_end: usize,
    right: &PreparedNaturalKey,
    right_start: usize,
    right_end: usize,
) -> Ordering {
    let left_significant = first_significant_digit(left, left_start, left_end);
    let right_significant = first_significant_digit(right, right_start, right_end);
    let left_length = normalized_numeric_length(left_significant, left_end);
    let right_length = normalized_numeric_length(right_significant, right_end);

    left_length.cmp(&right_length).then_with(|| {
        compare_unit_range(
            left,
            left_significant,
            left_end,
            right,
            right_significant,
            right_end,
        )
    })
}

/// All-zero run нормализуется к одному logical zero.
fn normalized_numeric_length(significant_start: usize, run_end: usize) -> usize {
    run_end.saturating_sub(significant_start).max(1)
}

/// Пропускает leading zeroes, не выходя за границу run.
fn first_significant_digit(key: &PreparedNaturalKey, start: usize, end: usize) -> usize {
    let mut significant = start;
    while significant < end && key.unit(significant) == u32::from(b'0') {
        significant += 1;
    }
    significant
}

/// Лексикографически сравнивает два диапазона logical units.
fn compare_unit_range(
    left: &PreparedNaturalKey,
    mut left_index: usize,
    left_end: usize,
    right: &PreparedNaturalKey,
    mut right_index: usize,
    right_end: usize,
) -> Ordering {
    while left_index < left_end && right_index < right_end {
        let ordering = left.unit(left_index).cmp(&right.unit(right_index));
        if ordering != Ordering::Equal {
            return ordering;
        }
        left_index += 1;
        right_index += 1;
    }
    (left_end - left_index).cmp(&(right_end - right_index))
}

/// Numeric policy намеренно ограничен ASCII digits.
const fn is_ascii_digit(unit: u32) -> bool {
    unit >= b'0' as u32 && unit <= b'9' as u32
}

/// Выполняет ASCII-only fold для wide units.
const fn ascii_lowercase_u16(unit: u16) -> u16 {
    if unit >= b'A' as u16 && unit <= b'Z' as u16 {
        unit + (b'a' - b'A') as u16
    } else {
        unit
    }
}

/// Выполняет ASCII-only fold для opaque units.
const fn ascii_lowercase_u32(unit: u32) -> u32 {
    if unit >= b'A' as u32 && unit <= b'Z' as u32 {
        unit + (b'a' - b'A') as u32
    } else {
        unit
    }
}

#[cfg(test)]
mod tests {
    use super::PreparedNaturalKey;
    use std::cmp::Ordering;

    #[test]
    fn numbers_leading_zeroes_case_and_unicode_match_session_05_semantics() {
        let mut names = [
            "Épisode 10.mkv",
            "épisode 2.mkv",
            "ÉPISODE 02.mkv",
            "Case.mkv",
            "case.mkv",
        ]
        .map(|name| (PreparedNaturalKey::from_utf8(name), name));

        names.sort_by(|(left_key, left_name), (right_key, right_name)| {
            left_key
                .cmp(right_key)
                .then_with(|| left_name.cmp(right_name))
        });

        assert_eq!(
            names.map(|(_, name)| name),
            [
                "Case.mkv",
                "case.mkv",
                "ÉPISODE 02.mkv",
                "épisode 2.mkv",
                "Épisode 10.mkv",
            ]
        );
    }

    #[test]
    fn comparator_is_total_antisymmetric_and_transitive() {
        let keys = [
            "item 0",
            "Item 00",
            "item 2",
            "ITEM 02",
            "item 10",
            "épisode 1",
            "Épisode 01",
            "zeta",
        ]
        .map(PreparedNaturalKey::from_utf8);

        for left in &keys {
            assert_eq!(left.cmp(left), Ordering::Equal);
            for right in &keys {
                assert_eq!(left.cmp(right), right.cmp(left).reverse());
                for third in &keys {
                    if left <= right && right <= third {
                        assert!(left <= third);
                    }
                }
            }
        }
    }

    #[test]
    fn compact_payload_uses_native_width() {
        assert_eq!(
            PreparedNaturalKey::from_bytes(b"File 10").retained_bytes(),
            7
        );
        assert_eq!(
            PreparedNaturalKey::from_wide_units(&[b'F' as u16, b'2' as u16]).retained_bytes(),
            4
        );
    }
}
