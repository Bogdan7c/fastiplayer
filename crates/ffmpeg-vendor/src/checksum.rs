//! Проверка SHA-256 архивов с исходниками.
//!
//! Модуль чистый (без cargo-директив и внешних процессов): его подключает
//! build script, а здесь же его закрепляют unit-тесты.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Размер буфера чтения: архив FFmpeg ~12 МБ, читать его целиком в память незачем.
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// Почему архив не прошёл проверку контрольной суммы.
#[derive(Debug)]
pub enum ChecksumError {
    /// Файл не удалось открыть или прочитать.
    Unreadable {
        /// Какой файл читали.
        archive_path: PathBuf,
        /// Исходная ошибка ввода-вывода.
        source: io::Error,
    },

    /// Файл прочитан, но его SHA-256 не совпал с закреплённым.
    Mismatch {
        /// Какой файл проверяли.
        archive_path: PathBuf,
        /// Закреплённая сумма из `sources.rs`.
        expected_sha256: String,
        /// Фактическая сумма файла.
        actual_sha256: String,
    },
}

impl fmt::Display for ChecksumError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable {
                archive_path,
                source,
            } => write!(
                formatter,
                "cannot read archive {}: {source}",
                archive_path.display()
            ),
            Self::Mismatch {
                archive_path,
                expected_sha256,
                actual_sha256,
            } => write!(
                formatter,
                "SHA-256 mismatch for {}: expected {expected_sha256}, got {actual_sha256}",
                archive_path.display()
            ),
        }
    }
}

/// Считает SHA-256 файла потоково и возвращает его в нижнем регистре hex.
pub fn sha256_of_file(archive_path: &Path) -> Result<String, ChecksumError> {
    // Ошибку открытия/чтения сохраняем вместе с путём, чтобы сообщение было понятным.
    let to_unreadable = |source: io::Error| ChecksumError::Unreadable {
        archive_path: archive_path.to_path_buf(),
        source,
    };

    let mut archive_file = File::open(archive_path).map_err(to_unreadable)?;
    let mut hasher = Sha256::new();
    let mut read_buffer = vec![0_u8; READ_BUFFER_BYTES];

    loop {
        let bytes_read = archive_file.read(&mut read_buffer).map_err(to_unreadable)?;
        // Ноль байт означает конец файла.
        if bytes_read == 0 {
            break;
        }
        hasher.update(&read_buffer[..bytes_read]);
    }

    Ok(to_lower_hex(&hasher.finalize()))
}

/// Проверяет, что SHA-256 файла совпадает с ожидаемым (регистр hex не важен).
pub fn verify_sha256(archive_path: &Path, expected_sha256: &str) -> Result<(), ChecksumError> {
    let actual_sha256 = sha256_of_file(archive_path)?;
    let expected_normalized = expected_sha256.to_ascii_lowercase();

    if actual_sha256 == expected_normalized {
        Ok(())
    } else {
        Err(ChecksumError::Mismatch {
            archive_path: archive_path.to_path_buf(),
            expected_sha256: expected_normalized,
            actual_sha256,
        })
    }
}

/// Переводит байты дайджеста в hex без внешних зависимостей.
fn to_lower_hex(digest_bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex_text = String::with_capacity(digest_bytes.len() * 2);
    for byte in digest_bytes {
        // Старшие 4 бита — первая цифра, младшие — вторая.
        hex_text.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        hex_text.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    hex_text
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{ChecksumError, sha256_of_file, verify_sha256};

    /// SHA-256 строки `abc` из FIPS 180-2 — эталон, не зависящий от нашего кода.
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// Создаёт пустой временный каталог для одного теста.
    fn temporary_directory(test_name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "ffmpeg-vendor-checksum-{test_name}-{}",
            std::process::id()
        ));
        // Остатки прошлого прогона мешают воспроизводимости, поэтому удаляем их.
        if directory.exists() {
            fs::remove_dir_all(&directory).expect("remove stale test directory");
        }
        fs::create_dir_all(&directory).expect("create test directory");
        directory
    }

    /// Пишет файл с заданным содержимым и возвращает путь.
    fn write_file(directory: &Path, file_name: &str, contents: &[u8]) -> PathBuf {
        let file_path = directory.join(file_name);
        fs::write(&file_path, contents).expect("write test file");
        file_path
    }

    #[test]
    fn sha256_matches_the_published_fips_vector() {
        let directory = temporary_directory("fips-vector");
        let archive_path = write_file(&directory, "abc.bin", b"abc");

        assert_eq!(
            sha256_of_file(&archive_path).expect("hash file"),
            ABC_SHA256
        );
    }

    #[test]
    fn sha256_of_multi_buffer_file_matches_reference() {
        let directory = temporary_directory("multi-buffer");
        // 1 000 000 символов `a` — второй эталон FIPS 180-2, больше буфера чтения.
        let archive_path = write_file(&directory, "million-a.bin", &vec![b'a'; 1_000_000]);

        assert_eq!(
            sha256_of_file(&archive_path).expect("hash file"),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn verification_accepts_matching_archive_in_any_hex_case() {
        let directory = temporary_directory("matching");
        let archive_path = write_file(&directory, "abc.bin", b"abc");

        verify_sha256(&archive_path, ABC_SHA256).expect("lower-case sum must match");
        verify_sha256(&archive_path, &ABC_SHA256.to_ascii_uppercase())
            .expect("upper-case sum must match");
    }

    #[test]
    fn verification_rejects_tampered_archive_and_reports_both_sums() {
        let directory = temporary_directory("tampered");
        // Один лишний байт — типичная подмена или обрыв загрузки.
        let archive_path = write_file(&directory, "abc.bin", b"abcd");

        let verification_error =
            verify_sha256(&archive_path, ABC_SHA256).expect_err("tampered archive must fail");

        let ChecksumError::Mismatch {
            expected_sha256,
            actual_sha256,
            ..
        } = verification_error
        else {
            panic!("expected checksum mismatch, got {verification_error}");
        };
        assert_eq!(expected_sha256, ABC_SHA256);
        assert_ne!(actual_sha256, ABC_SHA256);
        assert_eq!(actual_sha256.len(), 64);
    }

    #[test]
    fn verification_reports_missing_archive_as_unreadable() {
        let directory = temporary_directory("missing");
        let missing_archive = directory.join("does-not-exist.tar.xz");

        let verification_error =
            verify_sha256(&missing_archive, ABC_SHA256).expect_err("missing archive must fail");

        assert!(matches!(
            verification_error,
            ChecksumError::Unreadable { .. }
        ));
    }
}
