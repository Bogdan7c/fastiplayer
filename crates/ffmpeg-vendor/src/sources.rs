//! Закреплённые исходники FFmpeg и dav1d для vendored-сборки.
//!
//! Здесь живёт «замок» сборки: точные версии, адреса архивов и их SHA-256.
//! Обновление FFmpeg/dav1d (в том числе ради исправлений безопасности) —
//! это правка ровно этого файла: новая версия + новая контрольная сумма,
//! сверенная с подписью/контрольной суммой, которую публикует upstream.

/// Описание одного закреплённого архива с исходниками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedSource {
    /// Человекочитаемое имя библиотеки для сообщений об ошибках.
    pub library_name: &'static str,

    /// Короткое имя для логов и каталогов (`download-ffmpeg.log`).
    pub slug: &'static str,

    /// Точная версия upstream-релиза.
    pub version: &'static str,

    /// Имя файла архива; под этим именем архив кешируется в `OUT_DIR`.
    pub archive_file_name: &'static str,

    /// Официальный HTTPS-адрес архива.
    pub archive_url: &'static str,

    /// Ожидаемый SHA-256 архива в нижнем регистре (64 hex-символа).
    pub sha256: &'static str,

    /// Имя каталога верхнего уровня внутри архива.
    pub unpacked_directory_name: &'static str,

    /// Переменная окружения с путём к уже скачанному архиву (офлайн-сборка).
    /// Архив из неё проверяется тем же SHA-256, что и скачанный.
    pub local_archive_env: &'static str,
}

/// FFmpeg 8.1.3: подпись `.asc` проверена релизным ключом FFmpeg
/// (отпечаток FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8) при закреплении.
pub const FFMPEG_SOURCE: PinnedSource = PinnedSource {
    library_name: "FFmpeg",
    slug: "ffmpeg",
    version: "8.1.3",
    archive_file_name: "ffmpeg-8.1.3.tar.xz",
    archive_url: "https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz",
    sha256: "7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3",
    unpacked_directory_name: "ffmpeg-8.1.3",
    local_archive_env: "FASTIPLAYER_FFMPEG_ARCHIVE",
};

/// dav1d 1.5.4: SHA-256 совпадает с опубликованным VideoLAN файлом `.sha256`.
pub const DAV1D_SOURCE: PinnedSource = PinnedSource {
    library_name: "dav1d",
    slug: "dav1d",
    version: "1.5.4",
    archive_file_name: "dav1d-1.5.4.tar.xz",
    archive_url: "https://downloads.videolan.org/pub/videolan/dav1d/1.5.4/dav1d-1.5.4.tar.xz",
    sha256: "686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5",
    unpacked_directory_name: "dav1d-1.5.4",
    local_archive_env: "FASTIPLAYER_DAV1D_ARCHIVE",
};

/// Все закреплённые исходники в порядке сборки (dav1d нужен FFmpeg-у на configure).
pub const ALL_SOURCES: [PinnedSource; 2] = [DAV1D_SOURCE, FFMPEG_SOURCE];

#[cfg(test)]
mod tests {
    use super::ALL_SOURCES;

    #[test]
    fn pinned_sources_have_valid_sums_and_https_urls() {
        for source in ALL_SOURCES {
            assert_eq!(
                source.sha256.len(),
                64,
                "{} sha256 length",
                source.library_name
            );
            assert!(
                source
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{} sha256 must be lower-case hex",
                source.library_name
            );
            assert!(
                source.archive_url.starts_with("https://"),
                "{} url",
                source.library_name
            );
            assert!(
                source.archive_url.ends_with(source.archive_file_name),
                "{} url must point at the pinned archive",
                source.library_name
            );
            assert!(
                source.archive_file_name.contains(source.version)
                    && source.unpacked_directory_name.contains(source.version),
                "{} version must match archive and directory names",
                source.library_name
            );
        }
    }

    #[test]
    fn dav1d_is_built_before_ffmpeg() {
        // configure FFmpeg ищет уже установленную dav1d через pkg-config.
        let slugs: Vec<&str> = ALL_SOURCES.iter().map(|source| source.slug).collect();
        assert_eq!(slugs, ["dav1d", "ffmpeg"]);
    }
}
