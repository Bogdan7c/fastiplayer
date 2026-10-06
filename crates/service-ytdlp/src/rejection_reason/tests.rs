use super::{MAX_CLASSIFIED_LINE_BYTES, StderrRejectionClassifier, YtDlpRejectionReason};

/// Прогоняет весь stderr одним куском через потоковый классификатор.
fn classify(stderr: &str) -> YtDlpRejectionReason {
    let mut classifier = StderrRejectionClassifier::default();
    classifier.observe(stderr.as_bytes());
    classifier.finish()
}

/// Реальные формулировки `yt-dlp` 2026.08.19 → ожидаемая причина.
#[test]
fn real_yt_dlp_error_lines_map_to_user_reasons() {
    let cases = [
        (
            "ERROR: [youtube] abc123: Private video. Sign in if you've been granted access to this video. Use --cookies-from-browser or --cookies for the authentication.\n",
            YtDlpRejectionReason::PrivateMedia,
        ),
        (
            "ERROR: [youtube] abc123: Sign in to confirm your age. This video may be inappropriate for some users. Use --cookies-from-browser or --cookies for the authentication.\n",
            YtDlpRejectionReason::LoginRequired,
        ),
        (
            "ERROR: [youtube] abc123: Sign in to confirm you\u{2019}re not a bot. Use --cookies-from-browser or --cookies for the authentication.\n",
            YtDlpRejectionReason::LoginRequired,
        ),
        (
            "ERROR: [vimeo] 1234: This video is only available for registered users. Use --username and --password\n",
            YtDlpRejectionReason::LoginRequired,
        ),
        (
            "ERROR: [youtube] abc123: The uploader has not made this video available in your country\n",
            YtDlpRejectionReason::GeoRestricted,
        ),
        (
            "ERROR: [generic] abc: This video is not available from your location due to geo restriction\n",
            YtDlpRejectionReason::GeoRestricted,
        ),
        (
            "ERROR: [youtube] abc123: Video unavailable. This video has been removed by the uploader\n",
            YtDlpRejectionReason::MediaUnavailable,
        ),
        (
            "ERROR: [youtube] BaW_jenozKc: This video is unavailable\n",
            YtDlpRejectionReason::MediaUnavailable,
        ),
        (
            "ERROR: [generic] Unable to download webpage: HTTP Error 404: Not Found (caused by <HTTPError 404: Not Found>)\n",
            YtDlpRejectionReason::MediaUnavailable,
        ),
        (
            "ERROR: [generic] Unable to download webpage: HTTP Error 403: Forbidden\n",
            YtDlpRejectionReason::AccessDenied,
        ),
        (
            "ERROR: [generic] Unable to download webpage: HTTP Error 429: Too Many Requests\n",
            YtDlpRejectionReason::RateLimited,
        ),
        (
            "ERROR: Unsupported URL: https://example.test/page\n",
            YtDlpRejectionReason::UnsupportedUrl,
        ),
        (
            "ERROR: [generic] Unable to download webpage: <urlopen error [Errno -3] Temporary failure in name resolution> (caused by TransportError)\n",
            YtDlpRejectionReason::NetworkUnavailable,
        ),
        (
            "ERROR: [generic] Unable to download webpage: Failed to resolve 'www.example.test' ([Errno -2] Name or service not known)\n",
            YtDlpRejectionReason::NetworkUnavailable,
        ),
        (
            "ERROR: [generic] Unable to download webpage: [Errno 111] Connection refused\n",
            YtDlpRejectionReason::NetworkUnavailable,
        ),
        (
            "ERROR: [youtube] abc123: Something completely new happened\n",
            YtDlpRejectionReason::Unclassified,
        ),
    ];
    for (stderr, expected) in cases {
        assert_eq!(classify(stderr), expected, "stderr: {stderr}");
    }
}

/// Предупреждения не являются причиной отказа, даже если в них есть те же слова.
#[test]
fn warning_lines_are_ignored_and_error_line_wins() {
    let stderr = "WARNING: [youtube] Private video playlists are skipped\n\
                  ERROR: [youtube] abc123: Video unavailable\n";

    assert_eq!(classify(stderr), YtDlpRejectionReason::MediaUnavailable);
}

/// Пустой stderr и stderr без `ERROR:` — честный `Unclassified`, без догадок.
#[test]
fn stderr_without_error_marker_is_unclassified() {
    assert_eq!(classify(""), YtDlpRejectionReason::Unclassified);
    assert_eq!(
        classify("[debug] Command-line config: ['--dump-json']\nPrivate video\n"),
        YtDlpRejectionReason::Unclassified
    );
}

/// Строка без завершающего `\n` и строка, разрезанная между кусками чтения pipe-а,
/// классифицируются так же, как целая.
#[test]
fn line_split_across_chunks_and_without_newline_is_classified() {
    let mut classifier = StderrRejectionClassifier::default();
    classifier.observe(b"ERROR: [youtube] abc: Priv");
    classifier.observe(b"ate video");

    assert_eq!(classifier.finish(), YtDlpRejectionReason::PrivateMedia);
}

/// Первая распознанная строка `ERROR:` побеждает следующие.
#[test]
fn first_recognized_error_line_wins() {
    let stderr = "ERROR: [generic] Unsupported URL: https://a.test/x\n\
                  ERROR: [youtube] abc: Private video\n";

    assert_eq!(classify(stderr), YtDlpRejectionReason::UnsupportedUrl);
}

/// Гигантская строка не растит буфер сверх лимита, а метка в её начале распознаётся.
#[test]
fn oversized_line_is_bounded_and_still_classified_by_its_prefix() {
    let mut classifier = StderrRejectionClassifier::default();
    classifier.observe(b"ERROR: [youtube] abc: Private video ");
    let filler = vec![b'x'; MAX_CLASSIFIED_LINE_BYTES * 4];
    classifier.observe(&filler);

    assert!(classifier.current_line.len() <= MAX_CLASSIFIED_LINE_BYTES);
    assert_eq!(classifier.finish(), YtDlpRejectionReason::PrivateMedia);
}

/// Битый UTF-8 не ломает разбор соседней корректной строки.
#[test]
fn invalid_utf8_is_tolerated() {
    let mut classifier = StderrRejectionClassifier::default();
    classifier.observe(b"ERROR: \xff\xfe garbage\nERROR: [x] y: HTTP Error 404: Not Found\n");

    assert_eq!(classifier.finish(), YtDlpRejectionReason::MediaUnavailable);
}

/// Диагностический код причины не содержит пробелов и годится для логов.
#[test]
fn display_is_stable_diagnostic_code() {
    assert_eq!(
        YtDlpRejectionReason::LoginRequired.to_string(),
        "login-required"
    );
    assert_eq!(
        YtDlpRejectionReason::Unclassified.to_string(),
        "unclassified"
    );
}
