//! Лимиты вложений MKV из конфига доходят до демуксера локального файла.
//!
//! Фикстура с вложенным шрифтом живёт в `symphonia-demux` (там же описана генерация);
//! здесь проверяется именно путь приложения: конфиг -> `AttachmentReadLimits` ->
//! `open_local_demuxer_from_source` -> `take_attachments()`.

use std::path::PathBuf;

use fastiplayer_config::{PlayerDemuxConfig, SubtitlesConfig};
use media_core::SkippedAttachmentReason;
use source_core::{CancellationToken, LocalFileSource};

use super::{
    AttachmentReadLimits, attachment_read_limits_from_config, open_local_demuxer_from_source,
};

/// Байтов в МБ конфига: независимая от кода копия константы, чтобы тест ловил ошибку перевода.
const EXPECTED_BYTES_PER_MB: u64 = 1_048_576;

/// MKV с ASS-дорожкой и одним вложенным шрифтом (генерируется `generate.sh` в `symphonia-demux`).
fn font_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../symphonia-demux/tests/fixtures/subtitles/ass-with-font.mkv")
}

/// Открывает фикстуру через тот же вход, что использует приложение для локальных файлов.
fn open_fixture(limits: AttachmentReadLimits) -> Box<dyn media_core::Demuxer + Send> {
    let source = LocalFileSource::open(font_fixture_path()).expect("fixture source opens");
    open_local_demuxer_from_source(
        source,
        Some("mkv"),
        &PlayerDemuxConfig::default(),
        limits,
        CancellationToken::never_cancelled(),
    )
    .expect("fixture demuxer opens")
}

/// Мегабайты конфига переводятся в байты точно, для обоих лимитов независимо.
#[test]
fn config_megabytes_become_exact_attachment_byte_limits() {
    let config = SubtitlesConfig {
        max_attached_font_mb: 3,
        max_attached_fonts_total_mb: 7,
        ..SubtitlesConfig::default()
    };

    let limits = attachment_read_limits_from_config(&config);

    assert_eq!(limits.max_bytes_per_attachment, 3 * EXPECTED_BYTES_PER_MB);
    assert_eq!(limits.max_total_bytes, 7 * EXPECTED_BYTES_PER_MB);
}

/// Значения по умолчанию конфига дают прежние 20 МиБ и 100 МиБ.
#[test]
fn default_config_keeps_former_attachment_limits() {
    let limits = attachment_read_limits_from_config(&SubtitlesConfig::default());

    assert_eq!(limits.max_bytes_per_attachment, 20 * EXPECTED_BYTES_PER_MB);
    assert_eq!(limits.max_total_bytes, 100 * EXPECTED_BYTES_PER_MB);
}

/// Лимиты из конфига по умолчанию пускают шрифт фикстуры: вложение реально прочитано.
#[test]
fn local_open_with_config_limits_delivers_the_attached_font() {
    let limits = attachment_read_limits_from_config(&SubtitlesConfig::default());
    let mut demuxer = open_fixture(limits);

    let attachments = demuxer.take_attachments();

    assert_eq!(attachments.attachments.len(), 1);
    assert_eq!(
        attachments.attachments[0].file_name,
        "NotoSansHebrew-Regular.ttf"
    );
    assert!(!attachments.attachments[0].bytes.is_empty());
    assert!(attachments.skipped.is_empty());
}

/// Лимит, переданный в открытие, действует: слишком большое вложение пропущено с размером в отчёте.
#[test]
fn local_open_passes_a_small_limit_down_to_the_demuxer() {
    let limits = attachment_read_limits_from_config(&SubtitlesConfig::default())
        .with_max_bytes_per_attachment(1_024);
    let mut demuxer = open_fixture(limits);

    let attachments = demuxer.take_attachments();

    assert!(attachments.attachments.is_empty());
    assert_eq!(attachments.skipped.len(), 1);
    assert!(matches!(
        attachments.skipped[0].reason,
        SkippedAttachmentReason::TooLarge {
            limit_bytes: 1_024,
            ..
        }
    ));
}
