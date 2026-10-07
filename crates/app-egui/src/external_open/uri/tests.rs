use super::*;

fn local(path: &str) -> ExternalOpenItem {
    ExternalOpenItem::LocalPath(PathBuf::from(path))
}

#[test]
fn spaces_and_utf8_in_file_uri_become_exact_path() {
    let item = item_from_uri("file:///home/user/My%20Movies/%D1%84%D0%B8%D0%BB%D1%8C%D0%BC.mkv");
    assert_eq!(item, local("/home/user/My Movies/фильм.mkv"));
}

#[cfg(unix)]
#[test]
fn non_utf8_bytes_survive_percent_decoding() {
    use std::os::unix::ffi::OsStrExt;
    let ExternalOpenItem::LocalPath(path) = item_from_uri("file:///data/bad%FFname%20x.mkv") else {
        panic!("file URI must become a local path");
    };
    assert_eq!(path.as_os_str().as_bytes(), b"/data/bad\xFFname x.mkv");
}

#[test]
fn localhost_host_is_accepted_and_foreign_host_is_rejected() {
    assert_eq!(
        item_from_uri("file://localhost/tmp/a.mkv"),
        local("/tmp/a.mkv")
    );
    assert_eq!(
        item_from_uri("FILE://LocalHost/tmp/a.mkv"),
        local("/tmp/a.mkv")
    );
    assert_eq!(
        item_from_uri("file://other-host/tmp/a.mkv"),
        ExternalOpenItem::Unsupported {
            scheme: "file".to_owned()
        }
    );
}

#[test]
fn malformed_percent_sequences_stay_literal_and_nul_is_rejected() {
    assert_eq!(
        item_from_uri("file:///tmp/100%zz%.mkv"),
        local("/tmp/100%zz%.mkv")
    );
    assert_eq!(
        item_from_uri("file:///tmp/a%00b"),
        ExternalOpenItem::Unsupported {
            scheme: "file".to_owned()
        }
    );
}

#[test]
fn fragment_and_query_are_not_part_of_local_path() {
    assert_eq!(
        item_from_uri("file:///tmp/a%23b.mkv#frag"),
        local("/tmp/a#b.mkv")
    );
}

#[test]
fn http_and_https_become_web_urls_and_other_schemes_are_unsupported() {
    let ExternalOpenItem::WebUrl(url) = item_from_uri("  HTTPS://example.org/v?t=1 ") else {
        panic!("https must become a web url");
    };
    assert_eq!(url.as_str(), "HTTPS://example.org/v?t=1");
    assert_eq!(
        item_from_uri("ftp://example.org/a.mkv"),
        ExternalOpenItem::Unsupported {
            scheme: "ftp".to_owned()
        }
    );
    assert_eq!(
        item_from_uri("just some text"),
        ExternalOpenItem::Unsupported {
            scheme: String::new()
        }
    );
}

#[test]
fn payload_keeps_source_order_and_skips_blank_uris() {
    let uris = vec![
        "file:///b.mkv".to_owned(),
        "  ".to_owned(),
        "file:///a.mkv".to_owned(),
        "https://example.org/x".to_owned(),
    ];
    let items = items_from_drop_payload(&uris, Some("https://ignored.example/"));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0], local("/b.mkv"));
    assert_eq!(items[1], local("/a.mkv"));
    assert!(matches!(items[2], ExternalOpenItem::WebUrl(_)));
}

#[test]
fn plain_text_is_used_only_without_uris_and_only_for_web_links() {
    let items = items_from_drop_payload(
        &[],
        Some("  https://a.example/1 \nпривет\nhttp://b.example/2\nftp://c/3"),
    );
    let urls: Vec<&str> = items
        .iter()
        .map(|item| match item {
            ExternalOpenItem::WebUrl(url) => url.as_str(),
            other => panic!("only web urls expected, got {other:?}"),
        })
        .collect();
    assert_eq!(urls, ["https://a.example/1", "http://b.example/2"]);
    assert!(items_from_drop_payload(&[], Some("просто текст")).is_empty());
    assert!(items_from_drop_payload(&[], None).is_empty());
}

#[test]
fn web_url_debug_never_reveals_the_link() {
    let ExternalOpenItem::WebUrl(url) = item_from_uri("https://user:secret@example.org/?token=abc")
    else {
        panic!("https must become a web url");
    };
    assert!(!format!("{url:?}").contains("secret"));
}

#[test]
fn query_and_fragment_are_cut_but_encoded_marks_stay_in_the_name() {
    // Настоящие `?query` и `#fragment` — не часть пути.
    assert_eq!(
        item_from_uri("file:///tmp/a.mkv?download=1#t=5"),
        local("/tmp/a.mkv")
    );
    // Закодированные `%3F` и `%23` — буквальные символы имени файла.
    assert_eq!(
        item_from_uri("file:///tmp/what%3F%23.mkv"),
        local("/tmp/what?#.mkv")
    );
}
