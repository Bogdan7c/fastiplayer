use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use media_core::TrackInfo;

/// Читает единственный путь, который manual runner передал выбранному regression test.
pub fn selected_media_path() -> Result<PathBuf> {
    let selected_path = std::env::var_os("RUSTIPLAYER_MEDIA_PATH")
        .map(PathBuf::from)
        .context("RUSTIPLAYER_MEDIA_PATH must select one local media file")?;
    let metadata = std::fs::metadata(&selected_path)
        .with_context(|| format!("read selected media metadata: {}", selected_path.display()))?;
    ensure!(
        metadata.is_file(),
        "selected media path is not a regular file: {}",
        selected_path.display()
    );
    Ok(selected_path)
}

/// Печатает фактически разобранные признаки selected файла для ручного acceptance-отчёта.
pub fn report_selected_media(scenario: &str, path: &Path, tracks: &[TrackInfo]) -> Result<()> {
    let container = detect_container(path)?;
    let codecs = tracks
        .iter()
        .map(|track| format!("{:?}:{}", track.kind, track.codec_id))
        .collect::<Vec<_>>();
    println!(
        "MANUAL MEDIA: scenario={scenario}; path={}; container={container}; tracks={}",
        path.display(),
        codecs.join(",")
    );
    Ok(())
}

/// Определяет container по signature файла, а не по расширению выбранного пользователем пути.
fn detect_container(path: &Path) -> Result<&'static str> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut header = [0_u8; 12];
    let bytes_read = file
        .read(&mut header)
        .with_context(|| format!("read container signature: {}", path.display()))?;
    let header = &header[..bytes_read];

    Ok(match header {
        [0x1A, 0x45, 0xDF, 0xA3, ..] => "Matroska/WebM",
        bytes if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" => "ISO BMFF",
        bytes if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" => "WAV",
        bytes if bytes.starts_with(b"fLaC") => "FLAC",
        bytes if bytes.starts_with(b"OggS") => "Ogg",
        bytes if bytes.starts_with(b"wvpk") => "WavPack",
        _ => "unknown",
    })
}
