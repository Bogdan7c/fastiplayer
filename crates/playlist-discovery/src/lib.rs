//! UI/player/config-neutral primitive для probe одного локального media-файла.
//!
//! Directory traversal, batch admission, progress и app commit policy принадлежат
//! следующим слоям. Этот crate делает ровно один cooperative container probe.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use media_core::{MediaDuration, MediaTagMetadata};
use source_core::CancellationToken;
use symphonia_demux::{
    ContainerProbeError, ContainerProbeSnapshot, ContainerTrackTopology,
    probe_open_local_media_file,
};

/// Media-категория, определённая только по container track topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMediaKind {
    /// Контейнер содержит хотя бы один video track; audio tracks необязательны.
    VideoContaining,
    /// Контейнер содержит audio track(s), но не содержит video tracks.
    AudioOnly,
}

/// Best-effort cache invalidation fingerprint из exact file size + mtime.
///
/// Это не content hash: same-size/same-mtime rewrite может остаться незамеченным.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMediaFingerprint {
    file_size_bytes: u64,
    modified_at: SystemTime,
}

impl LocalMediaFingerprint {
    /// Возвращает размер открытого файла в байтах.
    #[must_use]
    pub const fn file_size_bytes(self) -> u64 {
        self.file_size_bytes
    }

    /// Возвращает filesystem modification time открытого файла.
    #[must_use]
    pub const fn modified_at(self) -> SystemTime {
        self.modified_at
    }
}

/// Готовый static record для siblings, manual Add или metadata refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedLocalMedia {
    display_filename: String,
    media_kind: LocalMediaKind,
    duration: Option<MediaDuration>,
    metadata: MediaTagMetadata,
    fingerprint: LocalMediaFingerprint,
}

impl ProbedLocalMedia {
    /// Возвращает lossy-safe имя файла для display fallback.
    #[must_use]
    pub fn display_filename(&self) -> &str {
        &self.display_filename
    }

    /// Возвращает container topology category.
    #[must_use]
    pub const fn media_kind(&self) -> LocalMediaKind {
        self.media_kind
    }

    /// Возвращает duration, когда container сообщил надёжные time units.
    #[must_use]
    pub const fn duration(&self) -> Option<MediaDuration> {
        self.duration
    }

    /// Возвращает нормализованные metadata tags.
    #[must_use]
    pub const fn metadata(&self) -> &MediaTagMetadata {
        &self.metadata
    }

    /// Возвращает fingerprint того же открытого file handle.
    #[must_use]
    pub const fn fingerprint(&self) -> LocalMediaFingerprint {
        self.fingerprint
    }
}

/// Typed outcomes, по которым batch/discovery owner строит partial diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum ProbeOneLocalMediaError {
    /// Caller запросил cooperative cancellation.
    #[error("probe локального media отменён")]
    Cancelled,

    /// Для содержимого файла не найден поддерживаемый container reader.
    #[error("неподдерживаемый media container: {reason}")]
    UnsupportedContainer {
        /// Безопасная техническая причина от demux owner-а.
        reason: String,
    },

    /// Container прочитан, но audio/video tracks в нём нет.
    #[error("media container не содержит audio/video tracks")]
    NoAudioVideoTracks,

    /// Файл не удалось открыть, stat-ить или прочитать.
    #[error("ошибка I/O при probe локального media: {0}")]
    IoFailure(#[source] io::Error),

    /// Reader найден, но container оказался malformed или нарушил probe limit/invariant.
    #[error("ошибка разбора media container: {reason}")]
    ProbeFailure {
        /// Безопасная техническая причина от demux owner-а.
        reason: String,
    },
}

/// Открывает и статически исследует ровно один local media path.
///
/// Extension передаётся Symphonia только как `Hint`; allowlist отсутствует.
/// Функция не создаёт `PreparedMedia`, decoder/player command или UI progress и
/// не должна вызываться перед explicit playback target open (D64/D75).
pub fn probe_one_local_media(
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
    ensure_not_cancelled(cancellation)?;

    // Отсутствует отдельный exists-check: open остаётся единственной авторитетной
    // filesystem операцией и сохраняет точный `io::ErrorKind`.
    let mut file = File::open(path).map_err(ProbeOneLocalMediaError::IoFailure)?;
    let extension_hint = path.extension().and_then(|extension| extension.to_str());
    let container_snapshot = probe_open_local_media_file(&mut file, extension_hint, cancellation)
        .map_err(map_container_probe_error)?;

    ensure_not_cancelled(cancellation)?;
    let file_metadata = file
        .metadata()
        .map_err(ProbeOneLocalMediaError::IoFailure)?;
    let modified_at = file_metadata
        .modified()
        .map_err(ProbeOneLocalMediaError::IoFailure)?;
    let fingerprint = LocalMediaFingerprint {
        file_size_bytes: file_metadata.len(),
        modified_at,
    };
    ensure_not_cancelled(cancellation)?;

    build_success_record(path, container_snapshot, fingerprint)
}

fn build_success_record(
    path: &Path,
    container_snapshot: ContainerProbeSnapshot,
    fingerprint: LocalMediaFingerprint,
) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
    let media_kind = media_kind_from_topology(container_snapshot.topology())?;
    let duration = container_snapshot.duration();
    let metadata = container_snapshot.into_metadata().tags;

    Ok(ProbedLocalMedia {
        display_filename: display_filename(path),
        media_kind,
        duration,
        metadata,
        fingerprint,
    })
}

fn media_kind_from_topology(
    topology: ContainerTrackTopology,
) -> Result<LocalMediaKind, ProbeOneLocalMediaError> {
    if topology.has_video() {
        Ok(LocalMediaKind::VideoContaining)
    } else if topology.has_audio() {
        Ok(LocalMediaKind::AudioOnly)
    } else {
        Err(ProbeOneLocalMediaError::NoAudioVideoTracks)
    }
}

fn display_filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), ProbeOneLocalMediaError> {
    if cancellation.is_cancelled() {
        Err(ProbeOneLocalMediaError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_container_probe_error(error: ContainerProbeError) -> ProbeOneLocalMediaError {
    match error {
        ContainerProbeError::Cancelled => ProbeOneLocalMediaError::Cancelled,
        ContainerProbeError::UnsupportedContainer { reason } => {
            ProbeOneLocalMediaError::UnsupportedContainer { reason }
        }
        ContainerProbeError::IoFailure(error) => ProbeOneLocalMediaError::IoFailure(error),
        ContainerProbeError::ProbeFailure { reason } => {
            ProbeOneLocalMediaError::ProbeFailure { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory {
        path: std::path::PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rustiplayer-playlist-discovery-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory must be created");
            Self { path }
        }

        fn file(&self, name: &str) -> std::path::PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn wav_is_audio_only_even_with_wrong_extension_and_has_fingerprint() {
        let directory = TestDirectory::new();
        let path = directory.file("display-name.video");
        let wav = pcm_wav_bytes(800, 8_000);
        fs::write(&path, &wav).expect("WAV fixture must be written");

        let record = probe_one_local_media(&path, &CancellationToken::new())
            .expect("content probe must ignore a misleading extension");
        let expected_modified_at = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("fixture mtime must be readable");

        assert_eq!(record.media_kind(), LocalMediaKind::AudioOnly);
        assert_eq!(record.display_filename(), "display-name.video");
        assert_eq!(record.fingerprint().file_size_bytes(), wav.len() as u64);
        assert_eq!(record.fingerprint().modified_at(), expected_modified_at);
        assert!(record.duration().is_some());
    }

    #[test]
    fn extension_is_optional_instead_of_an_allowlist() {
        let directory = TestDirectory::new();
        let path = directory.file("media-without-extension");
        fs::write(&path, pcm_wav_bytes(80, 8_000)).expect("WAV fixture must be written");

        let record = probe_one_local_media(&path, &CancellationToken::new())
            .expect("content probe must work without an extension");

        assert_eq!(record.media_kind(), LocalMediaKind::AudioOnly);
    }

    #[test]
    fn unsupported_bytes_are_not_reported_as_io_failure() {
        let directory = TestDirectory::new();
        let path = directory.file("notes.txt");
        fs::write(&path, b"this is not a media container").expect("fixture must be written");

        let error = probe_one_local_media(&path, &CancellationToken::new())
            .expect_err("plain text must be unsupported");

        assert!(matches!(
            error,
            ProbeOneLocalMediaError::UnsupportedContainer { .. }
        ));
    }

    #[test]
    fn malformed_known_container_is_distinct_from_unsupported_and_io() {
        let directory = TestDirectory::new();
        let path = directory.file("broken.wav");
        fs::write(&path, b"RIFF\x04\x00\x00\x00WAVE").expect("fixture must be written");

        let error = probe_one_local_media(&path, &CancellationToken::new())
            .expect_err("truncated WAV must fail");

        assert!(
            matches!(error, ProbeOneLocalMediaError::ProbeFailure { .. }),
            "unexpected malformed-container classification: {error:?}"
        );
    }

    #[test]
    fn missing_path_preserves_io_error_kind() {
        let directory = TestDirectory::new();
        let path = directory.file("disappeared.wav");

        let error = probe_one_local_media(&path, &CancellationToken::new())
            .expect_err("missing path must fail at open");

        match error {
            ProbeOneLocalMediaError::IoFailure(error) => {
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected I/O failure, got {other:?}"),
        }
    }

    #[test]
    fn cancellation_wins_before_any_file_io() {
        let token = CancellationToken::new();
        token.cancel();

        let error = probe_one_local_media(Path::new("missing-but-cancelled.wav"), &token)
            .expect_err("cancelled request must stop before open");

        assert!(matches!(error, ProbeOneLocalMediaError::Cancelled));
    }

    #[test]
    fn topology_classification_prefers_video_and_rejects_non_media_tracks() {
        let video_and_audio = ContainerTrackTopology::new(2, 1);
        let audio_only = ContainerTrackTopology::new(1, 0);
        let no_media = ContainerTrackTopology::new(0, 0);

        assert_eq!(
            media_kind_from_topology(video_and_audio).expect("video topology must classify"),
            LocalMediaKind::VideoContaining
        );
        assert_eq!(
            media_kind_from_topology(audio_only).expect("audio topology must classify"),
            LocalMediaKind::AudioOnly
        );
        assert!(matches!(
            media_kind_from_topology(no_media),
            Err(ProbeOneLocalMediaError::NoAudioVideoTracks)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_an_io_failure() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TestDirectory::new();
        let path = directory.file("unreadable.wav");
        fs::write(&path, pcm_wav_bytes(80, 8_000)).expect("fixture must be written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
            .expect("permissions must change");

        let result = probe_one_local_media(&path, &CancellationToken::new());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("permissions must restore for cleanup");
        match result {
            Err(ProbeOneLocalMediaError::IoFailure(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            }
            // Privileged CI users can bypass mode bits; in that environment this
            // platform-specific scenario cannot prove permission denial.
            Ok(_) if is_privileged_unix_user() => {}
            other => panic!("expected permission I/O failure, got {other:?}"),
        }
    }

    #[cfg(unix)]
    fn is_privileged_unix_user() -> bool {
        std::env::var_os("USER").is_some_and(|user| user == "root")
    }

    fn pcm_wav_bytes(sample_count: u32, sample_rate: u32) -> Vec<u8> {
        let bytes_per_sample = 2_u32;
        let audio_byte_count = sample_count * bytes_per_sample;
        let mut wav = Vec::with_capacity((44 + audio_byte_count) as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + audio_byte_count).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * bytes_per_sample).to_le_bytes());
        wav.extend_from_slice(&(bytes_per_sample as u16).to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&audio_byte_count.to_le_bytes());
        wav.resize((44 + audio_byte_count) as usize, 0);
        wav
    }
}
