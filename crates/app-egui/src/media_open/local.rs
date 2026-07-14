//! Single-open local preparation для D64/D75.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{Demuxer, MediaTagMetadata, TrackInfo};
use player_core::PreparedMedia;
use playlist_discovery::{LocalMediaFingerprint, LocalMediaKind, classify_local_media_tracks};
use rustiplayer_config::PlayerDemuxConfig;
use source_core::{LocalFileMetadataSnapshot, LocalFileSource};

use super::{ActiveMediaSource, PreparedMediaOpen, SafeMediaLabel};

/// Результат сравнения cached D64 fingerprint с реально открытым source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalFingerprintValidation {
    /// Caller не передал cache assumption; opened source сразу становится truth.
    NotRequested,
    /// Cached fingerprint совпал с тем же handle, который передан demuxer-у.
    Matched,
    /// Cache устарел; этот же prepared open является единственной revalidation.
    CacheMismatch,
}

/// Полный local envelope, снятый до transfer `PreparedMedia` player owner-у.
pub(crate) struct PreparedLocalOpenResult {
    /// Единственный prepared demuxer, построенный из одного opened file handle.
    pub(crate) prepared_media: PreparedMedia,
    /// Container category по immutable track snapshot-у.
    pub(crate) media_kind: LocalMediaKind,
    /// Полный список tracks из того же demux open.
    pub(crate) tracks: Vec<TrackInfo>,
    /// Duration из того же demux open.
    pub(crate) duration: Option<Duration>,
    /// Полный D12 tag cache из metadata snapshot-а demuxer-а.
    pub(crate) metadata: MediaTagMetadata,
    /// Actual size/mtime opened handle-а.
    pub(crate) fingerprint: LocalMediaFingerprint,
    /// Reconstructible source без lossy path conversion.
    pub(crate) source_path: PathBuf,
    /// Bounded display label без полного path-а.
    pub(crate) safe_label: SafeMediaLabel,
    /// Результат проверки caller cache assumption.
    pub(crate) fingerprint_validation: LocalFingerprintValidation,
}

/// Typed ошибки local preparation без раскрытия полного filesystem path-а.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PrepareLocalOpenError {
    /// Cooperative cancellation замечена между blocking stages.
    #[error("подготовка локального media отменена")]
    Cancelled,
    /// Открытие либо чтение metadata source-а не удалось.
    #[error("не удалось открыть локальный media source: {0}")]
    Source(#[source] source_core::SourceError),
    /// Container demuxer не смог открыть уже созданный source.
    #[error("не удалось открыть media container")]
    Demux(#[source] symphonia_demux::DemuxError),
    /// Container не содержит audio/video tracks.
    #[error("media container не содержит audio/video tracks")]
    NoAudioVideoTracks,
    /// Locator изменился после открытия handle-а и до ownership transfer.
    #[error("локальный media source изменился во время подготовки")]
    SourceChangedDuringPreparation,
    /// Финальная stat-проверка locator-а не удалась.
    #[error("не удалось повторно проверить локальный media source: {0}")]
    RevalidationIo(#[source] std::io::Error),
}

/// Открывает explicit local target ровно один раз и строит playback+cache envelope.
pub(crate) fn prepare_local_open(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
    expected_fingerprint: Option<LocalMediaFingerprint>,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedLocalOpenResult, PrepareLocalOpenError> {
    prepare_local_open_with_hook(
        path,
        demux_config,
        expected_fingerprint,
        is_cancelled,
        || {},
    )
}

fn prepare_local_open_with_hook(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
    expected_fingerprint: Option<LocalMediaFingerprint>,
    is_cancelled: impl Fn() -> bool,
    before_final_revalidation: impl FnOnce(),
) -> Result<PreparedLocalOpenResult, PrepareLocalOpenError> {
    ensure_not_cancelled(&is_cancelled)?;
    let local_source = LocalFileSource::open(path).map_err(PrepareLocalOpenError::Source)?;
    let opened_snapshot = local_source.metadata_snapshot();
    let actual_fingerprint = fingerprint_from_snapshot(opened_snapshot);
    let fingerprint_validation = match expected_fingerprint {
        None => LocalFingerprintValidation::NotRequested,
        Some(expected) if expected == actual_fingerprint => LocalFingerprintValidation::Matched,
        Some(_) => LocalFingerprintValidation::CacheMismatch,
    };

    ensure_not_cancelled(&is_cancelled)?;
    let extension_hint = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let safe_label = SafeMediaLabel::from_local_path(path);
    let demuxer = symphonia_demux::SymphoniaDemuxer::from_byte_source_with_options(
        local_source,
        extension_hint,
        safe_label.as_str(),
        crate::local_media::demuxer_options_from_config(demux_config),
    )
    .map_err(PrepareLocalOpenError::Demux)?;

    ensure_not_cancelled(&is_cancelled)?;
    let tracks = demuxer.tracks().to_vec();
    let media_kind = classify_local_media_tracks(&tracks)
        .map_err(|_| PrepareLocalOpenError::NoAudioVideoTracks)?;
    let duration = demuxer.duration();
    let metadata = demuxer.media_metadata().unwrap_or_default().tags;

    before_final_revalidation();
    let final_metadata = fs::metadata(path).map_err(PrepareLocalOpenError::RevalidationIo)?;
    let final_modified_at = final_metadata
        .modified()
        .map_err(PrepareLocalOpenError::RevalidationIo)?;
    let final_fingerprint = LocalMediaFingerprint::new(final_metadata.len(), final_modified_at);
    if final_fingerprint != actual_fingerprint {
        return Err(PrepareLocalOpenError::SourceChangedDuringPreparation);
    }
    ensure_not_cancelled(&is_cancelled)?;

    Ok(PreparedLocalOpenResult {
        prepared_media: PreparedMedia::from_local_file(path.to_path_buf(), Box::new(demuxer)),
        media_kind,
        tracks,
        duration,
        metadata,
        fingerprint: actual_fingerprint,
        source_path: path.to_path_buf(),
        safe_label,
        fingerprint_validation,
    })
}

impl PreparedLocalOpenResult {
    /// Преобразует local-specific envelope в единый coordinator payload.
    pub(super) fn into_prepared_open(self) -> PreparedMediaOpen {
        let descriptor = super::PreparedMediaDescriptor::Local {
            media_kind: self.media_kind,
            tracks: self.tracks,
            duration: self.duration,
            metadata: self.metadata,
            fingerprint: self.fingerprint,
            source: ActiveMediaSource::LocalFile(self.source_path),
            safe_label: self.safe_label,
            fingerprint_validation: self.fingerprint_validation,
        };
        PreparedMediaOpen {
            prepared_media: self.prepared_media,
            descriptor,
        }
    }
}

fn fingerprint_from_snapshot(snapshot: LocalFileMetadataSnapshot) -> LocalMediaFingerprint {
    LocalMediaFingerprint::new(snapshot.file_size_bytes, snapshot.modified_at)
}

fn ensure_not_cancelled(is_cancelled: &impl Fn() -> bool) -> Result<(), PrepareLocalOpenError> {
    if is_cancelled() {
        Err(PrepareLocalOpenError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;

    #[test]
    fn local_envelope_uses_one_prepared_demux_and_captures_fingerprint() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("single-open.wav");
        fs::write(&path, pcm_wav_bytes()).expect("write fixture");

        let prepared = prepare_local_open(&path, &PlayerDemuxConfig::default(), None, || false)
            .expect("prepare local envelope");

        assert_eq!(prepared.media_kind, LocalMediaKind::AudioOnly);
        assert!(!prepared.tracks.is_empty());
        assert!(prepared.duration.is_some());
        assert_eq!(prepared.source_path, path);
        assert_eq!(prepared.safe_label.as_str(), "single-open.wav");
        assert_eq!(
            prepared.fingerprint_validation,
            LocalFingerprintValidation::NotRequested
        );
        assert_eq!(
            prepared.fingerprint.file_size_bytes(),
            fs::metadata(&path).expect("fixture metadata").len()
        );
        assert_eq!(prepared.prepared_media.tracks(), prepared.tracks);
    }

    #[test]
    fn cache_mismatch_is_revalidated_by_same_prepared_envelope() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("changed-cache.wav");
        fs::write(&path, pcm_wav_bytes()).expect("write fixture");
        let stale = LocalMediaFingerprint::new(1, std::time::UNIX_EPOCH);

        let prepared =
            prepare_local_open(&path, &PlayerDemuxConfig::default(), Some(stale), || false)
                .expect("mismatch uses actual open as source of truth");

        assert_eq!(
            prepared.fingerprint_validation,
            LocalFingerprintValidation::CacheMismatch
        );
        assert_ne!(prepared.fingerprint, stale);
    }

    #[test]
    fn second_locator_change_fails_without_retry_loop() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("changes-twice.wav");
        fs::write(&path, pcm_wav_bytes()).expect("write fixture");

        let result = prepare_local_open_with_hook(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            || false,
            || {
                OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .expect("reopen fixture")
                    .write_all(&[0])
                    .expect("mutate fixture");
            },
        );

        assert!(matches!(
            result,
            Err(PrepareLocalOpenError::SourceChangedDuringPreparation)
        ));
    }

    fn pcm_wav_bytes() -> Vec<u8> {
        let samples = [0_i16, 500, -500, 250, -250, 0];
        let data_size = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&48_000_u32.to_le_bytes());
        bytes.extend_from_slice(&96_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }
}
