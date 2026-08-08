//! Site-agnostic policy восстановления после перехвата cross-host platform embed.

use serde_json::Value;
use url::Url;

pub(crate) const GENERIC_IMPERSONATE_EXTRACTOR_ARGS: [&str; 2] =
    ["--extractor-args", "generic:impersonate"];

const CANDIDATE_ARGUMENTS_BEFORE_POLICY: [&str; 5] = [
    "--quiet",
    "--no-warnings",
    "--simulate",
    "--dump-single-json",
    "--no-playlist",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlatformFamily {
    Youtube,
    Vimeo,
    Dailymotion,
    Twitter,
    Tiktok,
    Instagram,
    Facebook,
}

pub(crate) fn candidate_arguments(video_url: &str) -> Vec<&str> {
    let mut arguments = Vec::with_capacity(8);
    arguments.extend(CANDIDATE_ARGUMENTS_BEFORE_POLICY);
    arguments.extend(GENERIC_IMPERSONATE_EXTRACTOR_ARGS);
    arguments.push(video_url);
    arguments
}

pub(crate) fn write_pages_arguments(video_url: &str) -> Vec<&str> {
    let mut arguments = Vec::with_capacity(7);
    arguments.extend([
        "--quiet",
        "--no-warnings",
        "--skip-download",
        "--write-pages",
    ]);
    arguments.extend(GENERIC_IMPERSONATE_EXTRACTOR_ARGS);
    arguments.push(video_url);
    arguments
}

/// Извлекает абсолютные non-platform embed URL из сохранённой HTML-страницы.
///
/// Это намеренно узкая эвристика, а не второй HTML parser: рассматриваются
/// только iframe/source attributes, а malformed/relative значения игнорируются.
pub(crate) fn discover_non_platform_embed_urls(html: &str) -> Vec<String> {
    let mut ordinary = Vec::new();
    let mut player_like = Vec::new();
    let mut cursor = 0;

    while let Some(relative_start) = html[cursor..].find('<') {
        let tag_start = cursor + relative_start + 1;
        let Some(relative_end) = html[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + relative_end;
        let tag = &html[tag_start..tag_end];
        cursor = tag_end + 1;

        let Some((tag_name, attributes)) = split_tag(tag) else {
            continue;
        };
        let wanted_attributes: &[&str] = if tag_name.eq_ignore_ascii_case("iframe") {
            &["src", "data-src"]
        } else if tag_name.eq_ignore_ascii_case("source") {
            &["src"]
        } else {
            continue;
        };

        for attribute_name in wanted_attributes {
            let Some(raw_url) = attribute_value(attributes, attribute_name) else {
                continue;
            };
            let candidate = decode_basic_html_entities(raw_url.trim());
            let Ok(parsed) = Url::parse(&candidate) else {
                continue;
            };
            if !matches!(parsed.scheme(), "http" | "https") {
                continue;
            }
            let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else {
                continue;
            };
            if host_is_any_platform(&host)
                || locator_looks_like_authentication(&host, parsed.path())
            {
                continue;
            }
            if ordinary.contains(&candidate) || player_like.contains(&candidate) {
                continue;
            }

            if path_looks_like_player(parsed.path()) {
                player_like.push(candidate);
            } else {
                ordinary.push(candidate);
            }
        }
    }

    player_like.extend(ordinary);
    player_like
}

/// Извлекает bounded display title без попытки интерпретировать остальную HTML-страницу.
pub(crate) fn discover_page_title(html: &str) -> Option<String> {
    let lowercase = html.to_ascii_lowercase();
    if let Some(title_start) = lowercase.find("<title") {
        let after_name = title_start + "<title".len();
        if let Some(open_end) = lowercase[after_name..].find('>') {
            let content_start = after_name + open_end + 1;
            if let Some(close_start) = lowercase[content_start..].find("</title>")
                && let Some(title) =
                    normalize_page_title(&html[content_start..content_start + close_start])
            {
                return Some(title);
            }
        }
    }

    let mut cursor = 0;
    while let Some(relative_start) = html[cursor..].find('<') {
        let tag_start = cursor + relative_start + 1;
        let Some(relative_end) = html[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + relative_end;
        let tag = &html[tag_start..tag_end];
        cursor = tag_end + 1;

        let Some((tag_name, attributes)) = split_tag(tag) else {
            continue;
        };
        if !tag_name.eq_ignore_ascii_case("meta") {
            continue;
        }
        let is_open_graph_title = ["property", "name"].into_iter().any(|attribute| {
            attribute_value(attributes, attribute)
                .is_some_and(|value| value.eq_ignore_ascii_case("og:title"))
        });
        if is_open_graph_title
            && let Some(title) =
                attribute_value(attributes, "content").and_then(normalize_page_title)
        {
            return Some(title);
        }
    }

    None
}

fn normalize_page_title(raw_title: &str) -> Option<String> {
    let decoded = decode_basic_html_entities(raw_title);
    let normalized = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.chars().take(512).collect())
    }
}

#[must_use]
pub(crate) fn should_attempt_platform_embed_recovery(input_url: &str, document: &Value) -> bool {
    let Some(platform) = document_platform(document) else {
        return false;
    };
    let Some(input_host) = parsed_host(input_url) else {
        return false;
    };
    let Some(result_host) = result_host(document) else {
        return false;
    };

    // Extractor key недостаточен сам по себе: result URL подтверждает, что
    // wrapper действительно был заменён media другой platform family.
    host_is_platform(&result_host, platform) && !host_is_platform(&input_host, platform)
}

fn document_platform(document: &Value) -> Option<PlatformFamily> {
    ["extractor_key", "extractor"]
        .into_iter()
        .filter_map(|field| document.get(field)?.as_str())
        .find_map(classify_extractor)
}

fn classify_extractor(extractor: &str) -> Option<PlatformFamily> {
    let normalized = extractor
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    match normalized.as_str() {
        "youtube" | "youtubetab" => Some(PlatformFamily::Youtube),
        "vimeo" => Some(PlatformFamily::Vimeo),
        "dailymotion" => Some(PlatformFamily::Dailymotion),
        "twitter" | "x" => Some(PlatformFamily::Twitter),
        "tiktok" => Some(PlatformFamily::Tiktok),
        "instagram" => Some(PlatformFamily::Instagram),
        "facebook" => Some(PlatformFamily::Facebook),
        _ => None,
    }
}

fn result_host(document: &Value) -> Option<String> {
    ["webpage_url", "original_url"]
        .into_iter()
        .filter_map(|field| document.get(field)?.as_str())
        .find_map(parsed_host)
}

fn parsed_host(locator: &str) -> Option<String> {
    Url::parse(locator)
        .ok()?
        .host_str()
        .map(|host| host.to_ascii_lowercase())
}

fn split_tag(tag: &str) -> Option<(&str, &str)> {
    let tag = tag.trim_start();
    if tag.starts_with(['/', '!', '?']) {
        return None;
    }
    let name_end = tag
        .find(|character: char| character.is_ascii_whitespace() || character == '/')
        .unwrap_or(tag.len());
    let name = &tag[..name_end];
    (!name.is_empty()).then_some((name, &tag[name_end..]))
}

fn attribute_value<'a>(attributes: &'a str, wanted_name: &str) -> Option<&'a str> {
    let bytes = attributes.as_bytes();
    let mut cursor = 0;

    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        if name_start == cursor {
            cursor += 1;
            continue;
        }
        let name = &attributes[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            return None;
        }

        let (value_start, value_end) = if matches!(bytes[cursor], b'"' | b'\'') {
            let quote = bytes[cursor];
            cursor += 1;
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            let end = cursor;
            cursor = (cursor + 1).min(bytes.len());
            (start, end)
        } else {
            let start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'>'
            {
                cursor += 1;
            }
            (start, cursor)
        };

        if name.eq_ignore_ascii_case(wanted_name) {
            return Some(&attributes[value_start..value_end]);
        }
    }

    None
}

fn decode_basic_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&#38;", "&")
        .replace("&#x26;", "&")
        .replace("&#X26;", "&")
}

fn path_looks_like_player(path: &str) -> bool {
    let normalized = path.to_ascii_lowercase();
    ["/vod/", "/embed/", "/video/", "/player/"]
        .into_iter()
        .any(|token| normalized.contains(token))
}

fn locator_looks_like_authentication(host: &str, path: &str) -> bool {
    let normalized_path = path.to_ascii_lowercase();
    ["login", "signin", "oauth", "accounts."]
        .into_iter()
        .any(|token| host.contains(token) || normalized_path.contains(token))
}

fn host_is_any_platform(host: &str) -> bool {
    [
        PlatformFamily::Youtube,
        PlatformFamily::Vimeo,
        PlatformFamily::Dailymotion,
        PlatformFamily::Twitter,
        PlatformFamily::Tiktok,
        PlatformFamily::Instagram,
        PlatformFamily::Facebook,
    ]
    .into_iter()
    .any(|platform| host_is_platform(host, platform))
}

fn host_is_platform(host: &str, platform: PlatformFamily) -> bool {
    match platform {
        // Любой subdomain youtube.com + короткие aliases; без хардкода каталожных сайтов.
        PlatformFamily::Youtube => {
            is_domain_or_subdomain(host, "youtube.com") || host == "youtu.be"
        }
        PlatformFamily::Vimeo => is_domain_or_subdomain(host, "vimeo.com"),
        PlatformFamily::Dailymotion => {
            is_domain_or_subdomain(host, "dailymotion.com") || host == "dai.ly"
        }
        PlatformFamily::Twitter => {
            is_domain_or_subdomain(host, "twitter.com") || is_domain_or_subdomain(host, "x.com")
        }
        PlatformFamily::Tiktok => is_domain_or_subdomain(host, "tiktok.com"),
        PlatformFamily::Instagram => is_domain_or_subdomain(host, "instagram.com"),
        PlatformFamily::Facebook => {
            is_domain_or_subdomain(host, "facebook.com") || host == "fb.watch"
        }
    }
}

fn is_domain_or_subdomain(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn primary_arguments_always_enable_generic_impersonation() {
        assert_eq!(
            candidate_arguments("https://input.invalid/watch"),
            [
                "--quiet",
                "--no-warnings",
                "--simulate",
                "--dump-single-json",
                "--no-playlist",
                "--extractor-args",
                "generic:impersonate",
                "https://input.invalid/watch",
            ]
        );
    }

    #[test]
    fn write_pages_arguments_use_generic_without_extractor_exclusions() {
        assert_eq!(
            write_pages_arguments("https://input.invalid/watch"),
            [
                "--quiet",
                "--no-warnings",
                "--skip-download",
                "--write-pages",
                "--extractor-args",
                "generic:impersonate",
                "https://input.invalid/watch",
            ]
        );
    }

    #[test]
    fn discovery_drops_platform_embed_and_prefers_anonymous_player() {
        let html = r#"
            <iframe src="https://www.youtube.com/embed/platform"></iframe>
            <iframe data-src="https://cdn.example/assets/preview"></iframe>
            <iframe src="https://ashdi.example/vod/42?token=a&amp;part=1"></iframe>
        "#;

        assert_eq!(
            discover_non_platform_embed_urls(html),
            [
                "https://ashdi.example/vod/42?token=a&part=1",
                "https://cdn.example/assets/preview",
            ]
        );
    }

    #[test]
    fn discovery_drops_login_and_account_locators() {
        let html = r#"
            <iframe src="https://accounts.google.com/ServiceLogin"></iframe>
            <iframe src="https://media.example/oauth/authorize"></iframe>
            <iframe src="https://signin.media.example/player/ignored"></iframe>
            <iframe src="https://media.example/player/42"></iframe>
        "#;

        assert_eq!(
            discover_non_platform_embed_urls(html),
            ["https://media.example/player/42"]
        );
    }

    #[test]
    fn page_title_prefers_title_and_is_bounded() {
        let long_suffix = "x".repeat(600);
        let html = format!(
            r#"<meta property="og:title" content="fallback"><title>  Film &amp; Test {long_suffix} </title>"#
        );

        let title = discover_page_title(&html).expect("title should be discovered");
        assert!(title.starts_with("Film & Test"));
        assert_eq!(title.chars().count(), 512);
    }

    #[test]
    fn page_title_accepts_open_graph_meta() {
        assert_eq!(
            discover_page_title(
                r#"<meta content="Recovered title" property="og:title"><body></body>"#
            )
            .as_deref(),
            Some("Recovered title")
        );
    }

    #[test]
    fn discovery_accepts_case_insensitive_attributes_and_absolute_http_only() {
        let html = r#"
            <IFRAME DATA-SRC='//relative.example/embed/1'></IFRAME>
            <iframe src="/relative/player/2"></iframe>
            <source SRC=https://media.example/video/3>
            <iframe src="javascript:alert(1)"></iframe>
            <iframe src="http://plain.example/vod/4"></iframe>
        "#;

        assert_eq!(
            discover_non_platform_embed_urls(html),
            [
                "https://media.example/video/3",
                "http://plain.example/vod/4",
            ]
        );
    }

    #[test]
    fn cross_host_youtube_embed_requests_recovery() {
        let document = json!({
            "extractor": "youtube",
            "extractor_key": "Youtube",
            "webpage_url": "https://www.youtube.com/watch?v=embedded"
        });

        assert!(should_attempt_platform_embed_recovery(
            "https://cinema.example/watch/42",
            &document
        ));
    }

    #[test]
    fn direct_youtube_family_never_requests_recovery() {
        let document = json!({
            "extractor_key": "YoutubeTab",
            "webpage_url": "https://www.youtube.com/playlist?list=direct"
        });

        for input in [
            "https://youtube.com/watch?v=direct",
            "https://www.youtube.com/watch?v=direct",
            "https://m.youtube.com/watch?v=direct",
            "https://youtu.be/direct",
            "https://music.youtube.com/watch?v=direct",
        ] {
            assert!(!should_attempt_platform_embed_recovery(input, &document));
        }
    }

    #[test]
    fn unknown_or_unconfirmed_platform_result_does_not_request_recovery() {
        let unknown = json!({
            "extractor_key": "Generic",
            "webpage_url": "https://www.youtube.com/watch?v=embedded"
        });
        let mismatched_result = json!({
            "extractor_key": "Youtube",
            "webpage_url": "https://wrapper.example/watch/42"
        });

        assert!(!should_attempt_platform_embed_recovery(
            "https://cinema.example/watch/42",
            &unknown
        ));
        assert!(!should_attempt_platform_embed_recovery(
            "https://cinema.example/watch/42",
            &mismatched_result
        ));
    }

    #[test]
    fn platform_extractor_variants_are_case_insensitive() {
        for (extractor, webpage_url) in [
            ("youtube_tab", "https://youtube.com/watch?v=x"),
            ("VIMEO", "https://player.vimeo.com/video/1"),
            ("Dailymotion", "https://www.dailymotion.com/video/x"),
            ("X", "https://x.com/user/status/1"),
            ("TikTok", "https://www.tiktok.com/@user/video/1"),
            ("Instagram", "https://www.instagram.com/reel/1"),
            ("Facebook", "https://www.facebook.com/watch/1"),
        ] {
            let document = json!({
                "extractor_key": extractor,
                "original_url": webpage_url
            });
            assert!(should_attempt_platform_embed_recovery(
                "https://wrapper.example/watch/1",
                &document
            ));
        }
    }
}
