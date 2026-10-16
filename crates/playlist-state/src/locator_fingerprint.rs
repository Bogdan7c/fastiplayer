//! Общий кодировщик SHA-256 fingerprint-а `PlaylistLocator`.
//!
//! Resume-sidecar и track memory хранят не сам путь/URL, а необратимый отпечаток. Кодирование
//! байтов локатора одно и то же, а различие артефактов задаёт domain separator, который
//! передаёт вызывающий: отпечаток одного файла нельзя подставить в другой. Байты, которые
//! получает resume, зафиксированы golden-тестом и не должны меняться.

use std::path::Path;

use playlist_core::{ForeignPathEncoding, ForeignPathPlatform, LocalLocator, PlaylistLocator};
use sha2::{Digest, Sha256};

/// Платформа не умеет отдавать точные байты native path, отпечаток построить нельзя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnsupportedNativePathEncoding;

/// Считает fingerprint точных байт локатора под заданным domain separator-ом.
pub(crate) fn locator_fingerprint_with_domain(
    domain_separator: &[u8],
    locator: &PlaylistLocator,
) -> Result<[u8; 32], UnsupportedNativePathEncoding> {
    let mut hasher = Sha256::new();
    // Домен идёт первым и с префиксом длины: границу домена нельзя «сдвинуть» содержимым.
    update_fingerprint_bytes(&mut hasher, domain_separator);
    match locator {
        PlaylistLocator::Url(url) => {
            hasher.update([0x01]);
            update_fingerprint_bytes(&mut hasher, url.expose_secret_for_persistence().as_bytes());
        }
        PlaylistLocator::Local(LocalLocator::Native(path)) => {
            hasher.update([0x02]);
            update_native_path_fingerprint(&mut hasher, path)?;
        }
        PlaylistLocator::Local(LocalLocator::Foreign(path)) => {
            hasher.update([0x03]);
            update_foreign_platform(&mut hasher, path.platform_for_persistence());
            update_foreign_encoding(&mut hasher, path.encoding_for_persistence());
        }
    }
    Ok(hasher.finalize().into())
}

fn update_fingerprint_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(unix)]
fn update_native_path_fingerprint(
    hasher: &mut Sha256,
    path: &Path,
) -> Result<(), UnsupportedNativePathEncoding> {
    use std::os::unix::ffi::OsStrExt;

    hasher.update([0x01]);
    update_fingerprint_bytes(hasher, path.as_os_str().as_bytes());
    Ok(())
}

#[cfg(windows)]
fn update_native_path_fingerprint(
    hasher: &mut Sha256,
    path: &Path,
) -> Result<(), UnsupportedNativePathEncoding> {
    use std::os::windows::ffi::OsStrExt;

    hasher.update([0x02]);
    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    hasher.update((units.len() as u64).to_le_bytes());
    for unit in units {
        hasher.update(unit.to_le_bytes());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn update_native_path_fingerprint(
    _hasher: &mut Sha256,
    _path: &Path,
) -> Result<(), UnsupportedNativePathEncoding> {
    Err(UnsupportedNativePathEncoding)
}

fn update_foreign_platform(hasher: &mut Sha256, platform: &ForeignPathPlatform) {
    match platform {
        ForeignPathPlatform::Linux => hasher.update([0x01]),
        ForeignPathPlatform::MacOs => hasher.update([0x02]),
        ForeignPathPlatform::Windows => hasher.update([0x03]),
        ForeignPathPlatform::Other(name) => {
            hasher.update([0x04]);
            update_fingerprint_bytes(hasher, name.as_bytes());
        }
    }
}

fn update_foreign_encoding(hasher: &mut Sha256, encoding: &ForeignPathEncoding) {
    match encoding {
        ForeignPathEncoding::Utf8(value) => {
            hasher.update([0x01]);
            update_fingerprint_bytes(hasher, value.as_bytes());
        }
        ForeignPathEncoding::Bytes(bytes) => {
            hasher.update([0x02]);
            update_fingerprint_bytes(hasher, bytes);
        }
        ForeignPathEncoding::Wide(units) => {
            hasher.update([0x03]);
            hasher.update((units.len() as u64).to_le_bytes());
            for unit in units {
                hasher.update(unit.to_le_bytes());
            }
        }
        ForeignPathEncoding::Opaque {
            encoding_name,
            raw_units,
        } => {
            hasher.update([0x04]);
            update_fingerprint_bytes(hasher, encoding_name.as_bytes());
            hasher.update((raw_units.len() as u64).to_le_bytes());
            for unit in raw_units {
                hasher.update(unit.to_le_bytes());
            }
        }
    }
}

pub(crate) fn encode_lower_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(crate) fn decode_lower_hex_32(encoded: &str) -> Option<[u8; 32]> {
    if encoded.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, chunk) in encoded.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let high = decode_lower_hex_nibble(chunk[0])?;
        let low = decode_lower_hex_nibble(chunk[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

fn decode_lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
