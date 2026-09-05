//! Single-open local preparation для D64/D75.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fastiplayer_config::PlayerDemuxConfig;
use media_core::{MediaTagMetadata, TrackInfo};
use player_core::PreparedMedia;
use playlist_discovery::{LocalMediaFingerprint, LocalMediaKind, classify_local_media_tracks};
use source_core::{CancellationToken, LocalFileMetadataSnapshot, LocalFileSource};

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
    Demux(#[source] crate::local_media::LocalDemuxOpenError),
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
    source_cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedLocalOpenResult, PrepareLocalOpenError> {
    prepare_local_open_with_hook(
        path,
        demux_config,
        expected_fingerprint,
        source_cancellation,
        is_cancelled,
        || {},
    )
}

fn prepare_local_open_with_hook(
    path: &Path,
    demux_config: &PlayerDemuxConfig,
    expected_fingerprint: Option<LocalMediaFingerprint>,
    source_cancellation: CancellationToken,
    is_cancelled: impl Fn() -> bool,
    before_final_revalidation: impl FnOnce(),
) -> Result<PreparedLocalOpenResult, PrepareLocalOpenError> {
    ensure_not_cancelled(&is_cancelled)?;
    if source_cancellation.is_cancelled() {
        return Err(PrepareLocalOpenError::Cancelled);
    }
    let local_source = LocalFileSource::open(path).map_err(PrepareLocalOpenError::Source)?;
    let opened_snapshot = local_source.metadata_snapshot();
    let actual_fingerprint = fingerprint_from_snapshot(opened_snapshot);
    let fingerprint_validation = match expected_fingerprint {
        None => LocalFingerprintValidation::NotRequested,
        Some(expected) if expected == actual_fingerprint => LocalFingerprintValidation::Matched,
        Some(_) => LocalFingerprintValidation::CacheMismatch,
    };

    ensure_not_cancelled(&is_cancelled)?;
    let extension_hint = path.extension().and_then(|value| value.to_str());
    let safe_label = SafeMediaLabel::from_local_path(path);
    let demuxer = crate::local_media::open_local_demuxer_from_source(
        local_source,
        extension_hint,
        demux_config,
        source_cancellation,
    )
    .map_err(|error| {
        if error.is_cancelled() {
            PrepareLocalOpenError::Cancelled
        } else {
            PrepareLocalOpenError::Demux(error)
        }
    })?;

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
        prepared_media: PreparedMedia::from_local_file(path.to_path_buf(), demuxer),
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
pub(crate) mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;

    #[test]
    fn local_envelope_uses_one_prepared_demux_and_captures_fingerprint() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("single-open.wav");
        fs::write(&path, pcm_wav_bytes()).expect("write fixture");

        let prepared = prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            CancellationToken::never_cancelled(),
            || false,
        )
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

        let prepared = prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            Some(stale),
            CancellationToken::never_cancelled(),
            || false,
        )
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
            CancellationToken::never_cancelled(),
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

    #[test]
    fn extensionless_local_ts_uses_signature_and_same_open_envelope() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("transport-stream-without-extension");
        fs::write(&path, mpeg_ts_h264_aac_bytes()).expect("write TS fixture");

        let prepared = prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            CancellationToken::never_cancelled(),
            || false,
        )
        .expect("signature-selected local TS");

        assert_eq!(prepared.media_kind, LocalMediaKind::VideoContaining);
        assert_eq!(prepared.tracks.len(), 2);
        assert_eq!(prepared.prepared_media.tracks(), prepared.tracks);
    }

    #[test]
    fn conflicting_mp4_extension_does_not_override_ts_signature() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("actually-ts.mp4");
        fs::write(&path, mpeg_ts_h264_aac_bytes()).expect("write TS fixture");

        let prepared = prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            CancellationToken::never_cancelled(),
            || false,
        )
        .expect("content signature wins");

        assert!(
            prepared
                .tracks
                .iter()
                .any(|track| track.codec_id == "V_MPEG4/ISO/AVC")
        );
    }

    #[test]
    fn local_audio_only_ts_is_classified_without_network_path() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("audio-only.ts");
        fs::write(&path, mpeg_ts_audio_only_bytes()).expect("write audio TS fixture");

        let prepared = prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            CancellationToken::never_cancelled(),
            || false,
        )
        .expect("audio-only local TS");

        assert_eq!(prepared.media_kind, LocalMediaKind::AudioOnly);
        assert_eq!(prepared.tracks.len(), 1);
        assert_eq!(prepared.tracks[0].codec_id, "A_AAC");
    }

    #[test]
    fn cancelled_source_token_stops_before_demux_probe() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("cancelled.ts");
        fs::write(&path, mpeg_ts_h264_aac_bytes()).expect("write TS fixture");
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            cancellation,
            || false,
        );

        assert!(matches!(result, Err(PrepareLocalOpenError::Cancelled)));
    }

    #[test]
    fn malformed_local_ts_returns_safe_typed_error_without_path_disclosure() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("private-customer-name.ts");
        let mut malformed = mpeg_ts_h264_aac_bytes();
        malformed.truncate(188 * 2 + 17);
        fs::write(&path, malformed).expect("write malformed TS fixture");

        let error = match prepare_local_open(
            &path,
            &PlayerDemuxConfig::default(),
            None,
            CancellationToken::never_cancelled(),
            || false,
        ) {
            Ok(_) => panic!("malformed local TS must fail"),
            Err(error) => error,
        };

        assert!(matches!(error, PrepareLocalOpenError::Demux(_)));
        assert!(!error.to_string().contains("private-customer-name"));
    }

    pub(crate) fn pcm_wav_bytes() -> Vec<u8> {
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

    pub(crate) fn mpeg_ts_h264_aac_bytes() -> Vec<u8> {
        let pat = psi_section(vec![
            0x00, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xe1, 0x00,
        ]);
        let pmt = psi_section(vec![
            0x02, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe1, 0x01, 0xf0, 0x00, 0x1b, 0xe1,
            0x01, 0xf0, 0x00, 0x0f, 0xe1, 0x02, 0xf0, 0x00,
        ]);
        let h264 = [
            0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00,
            0x01, 0x65, 0x88,
        ];
        let aac = [0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 0x11, 0x22];
        let mut fixture = Vec::new();
        fixture.extend(ts_packet(0, true, 0, &with_pointer(pat)));
        fixture.extend(ts_packet(0x100, true, 0, &with_pointer(pmt)));
        fixture.extend(ts_packet(0x101, true, 0, &pes_bytes(90_000, &h264)));
        fixture.extend(ts_packet(0x102, true, 0, &pes_bytes(90_000, &aac)));
        fixture
    }

    fn mpeg_ts_audio_only_bytes() -> Vec<u8> {
        let pat = psi_section(vec![
            0x00, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xe1, 0x00,
        ]);
        let pmt = psi_section(vec![
            0x02, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe1, 0x02, 0xf0, 0x00, 0x0f, 0xe1,
            0x02, 0xf0, 0x00,
        ]);
        let aac = [0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 0x11, 0x22];
        let mut fixture = Vec::new();
        fixture.extend(ts_packet(0, true, 0, &with_pointer(pat)));
        fixture.extend(ts_packet(0x100, true, 0, &with_pointer(pmt)));
        fixture.extend(ts_packet(0x102, true, 0, &pes_bytes(0, &aac)));
        fixture
    }

    fn psi_section(mut section: Vec<u8>) -> Vec<u8> {
        let section_length = section.len() - 3 + 4;
        section[1] = 0xb0 | ((section_length >> 8) as u8 & 0x0f);
        section[2] = section_length as u8;
        let mut crc = 0xffff_ffff_u32;
        for byte in &section {
            crc ^= u32::from(*byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        section.extend_from_slice(&crc.to_be_bytes());
        section
    }

    fn with_pointer(section: Vec<u8>) -> Vec<u8> {
        let mut payload = vec![0];
        payload.extend(section);
        payload
    }

    fn pes_bytes(pts: u64, payload: &[u8]) -> Vec<u8> {
        let timestamp = [
            0x21 | (((pts >> 30) as u8 & 0x07) << 1),
            (pts >> 22) as u8,
            (((pts >> 15) as u8 & 0x7f) << 1) | 1,
            (pts >> 7) as u8,
            ((pts as u8 & 0x7f) << 1) | 1,
        ];
        let mut pes = vec![0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x80, 0x80, 0x05];
        pes.extend_from_slice(&timestamp);
        pes.extend_from_slice(payload);
        let length = (pes.len() - 6) as u16;
        pes[4..6].copy_from_slice(&length.to_be_bytes());
        pes
    }

    fn ts_packet(pid: u16, payload_start: bool, continuity: u8, payload: &[u8]) -> [u8; 188] {
        let mut packet = [0xff_u8; 188];
        packet[0] = 0x47;
        packet[1] = ((payload_start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
        packet[2] = pid as u8;
        packet[3] = 0x30 | (continuity & 0x0f);
        let adaptation_length = 183 - payload.len();
        packet[4] = adaptation_length as u8;
        if adaptation_length > 0 {
            packet[5] = 0;
        }
        let payload_offset = 5 + adaptation_length;
        packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        packet
    }
}
