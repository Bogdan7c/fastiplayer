use super::*;
use crate::limits::MAX_PAGE_TITLE_BYTES;

#[test]
fn display_text_accepts_real_world_unicode() {
    for title in [
        "",
        "Шрек 2 — серія 3 (українська озвучка)",
        "مرحبا بالعالم",
        "שלום עולם",
        "👨‍👩‍👧‍👦 family vlog 🎬",
        "日本語のタイトル",
        "left\u{200E}mark and right\u{200F}mark",
    ] {
        let parsed = PageTitle::new(title);
        assert_eq!(
            parsed.as_ref().map(PageTitle::as_str),
            Ok(title),
            "{title:?}"
        );
    }
}

#[test]
fn display_text_rejects_controls_line_breaks_and_bidi_overrides() {
    for title in [
        "nul\0byte",
        "tab\there",
        "line\nbreak",
        "del\u{7F}",
        "c1\u{85}next-line",
        "line\u{2028}separator",
        "paragraph\u{2029}separator",
        "spoof\u{202E}4pm.exe",
        "embed\u{202A}ltr",
        "isolate\u{2066}ltr\u{2069}",
    ] {
        assert_eq!(
            PageTitle::new(title),
            Err(ValueViolation::TextForbiddenCharacter),
            "{title:?}"
        );
        assert_eq!(
            VariantLabel::new(title),
            Err(ValueViolation::TextForbiddenCharacter),
            "{title:?}"
        );
    }
}

#[test]
fn title_length_limit_is_counted_in_utf8_bytes_at_the_boundary() {
    // «ї» — 2 байта UTF-8: ровно на границе принимается, на байт дальше — нет.
    let at_limit = "ї".repeat(MAX_PAGE_TITLE_BYTES / 2);
    assert_eq!(at_limit.len(), MAX_PAGE_TITLE_BYTES);
    assert!(PageTitle::new(at_limit.clone()).is_ok());
    let over_limit = format!("{at_limit}a");
    assert_eq!(PageTitle::new(over_limit), Err(ValueViolation::TextTooLong));
}

#[test]
fn required_texts_reject_empty_values() {
    assert_eq!(VariantLabel::new(""), Err(ValueViolation::TextEmpty));
    assert_eq!(SiteDomain::new(""), Err(ValueViolation::TextEmpty));
    assert_eq!(MimeType::new(""), Err(ValueViolation::TextEmpty));
    assert_eq!(SessionKey::new(""), Err(ValueViolation::TextEmpty));
}

#[test]
fn domain_accepts_hostnames_punycode_and_ipv6_but_not_paths_or_uppercase() {
    for domain in [
        "www.youtube.com",
        "xn--80ak6aa92e.com",
        "127.0.0.1",
        "[::1]",
    ] {
        assert!(SiteDomain::new(domain).is_ok(), "{domain:?}");
    }
    for domain in [
        "WWW.YOUTUBE.COM",
        "youtube.com/watch",
        "user@host",
        "host name",
        "україна.укр",
    ] {
        assert_eq!(
            SiteDomain::new(domain),
            Err(ValueViolation::TextForbiddenCharacter),
            "{domain:?}"
        );
    }
}

#[test]
fn mime_type_accepts_codec_parameters_and_rejects_non_ascii() {
    assert!(MimeType::new("video/mp4; codecs=\"avc1.64001f,mp4a.40.2\"").is_ok());
    assert!(MimeType::new("audio/webm; codecs=\"opus\"").is_ok());
    assert_eq!(
        MimeType::new("video/mp4\ncodecs"),
        Err(ValueViolation::TextForbiddenCharacter)
    );
    assert_eq!(
        MimeType::new("відео/mp4"),
        Err(ValueViolation::TextForbiddenCharacter)
    );
}

#[test]
fn session_token_has_length_bounds_and_base64url_alphabet() {
    let shortest = "A".repeat(MIN_SESSION_TOKEN_BYTES);
    assert!(SessionToken::new(shortest.clone()).is_ok());
    assert_eq!(
        SessionToken::new(&shortest[1..]),
        Err(ValueViolation::TextTooShort)
    );
    let longest = "z".repeat(MAX_SESSION_TOKEN_BYTES);
    assert!(SessionToken::new(longest.clone()).is_ok());
    assert_eq!(
        SessionToken::new(format!("{longest}z")),
        Err(ValueViolation::TextTooLong)
    );
    let with_padding = format!("{}==", "A".repeat(MIN_SESSION_TOKEN_BYTES));
    assert_eq!(
        SessionToken::new(with_padding),
        Err(ValueViolation::TextForbiddenCharacter)
    );
}

#[test]
fn session_token_matches_only_identical_token_and_never_prints_itself() {
    let issued = SessionToken::new("Q2hlY2tUb2tlbi0xMjM0NTY3ODkwX2FiY2RlZg");
    let same = SessionToken::new("Q2hlY2tUb2tlbi0xMjM0NTY3ODkwX2FiY2RlZg");
    let last_char_differs = SessionToken::new("Q2hlY2tUb2tlbi0xMjM0NTY3ODkwX2FiY2RlZw");
    let longer = SessionToken::new("Q2hlY2tUb2tlbi0xMjM0NTY3ODkwX2FiY2RlZgA");
    let (Ok(issued), Ok(same), Ok(last_char_differs), Ok(longer)) =
        (issued, same, last_char_differs, longer)
    else {
        panic!("тестовые токены должны проходить проверку");
    };
    assert!(issued.matches(&same));
    assert!(!issued.matches(&last_char_differs));
    assert!(!issued.matches(&longer));
    let printed = format!("{issued:?}");
    assert_eq!(printed, "SessionToken(<redacted>)");
    assert!(!printed.contains("Q2hl"));
}

#[test]
fn deserialization_applies_the_same_rules_as_the_constructor() {
    let parsed: Result<PageTitle, _> = serde_json::from_str("\"ok \\u202e title\"");
    assert!(parsed.is_err());
    let parsed: Result<SiteDomain, _> = serde_json::from_str("\"example.com\"");
    assert_eq!(
        parsed.ok().map(String::from),
        Some("example.com".to_owned())
    );
}
