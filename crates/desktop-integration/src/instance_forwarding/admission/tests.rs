use super::{
    ForwardedRequestRejection, MAX_ACTIVATION_TOKEN_BYTES, MAX_FORWARDED_URI_BYTES,
    MAX_FORWARDED_URIS, MAX_TOTAL_FORWARDED_URI_BYTES, admit_open_uris, admit_peer,
    is_valid_activation_token,
};
use crate::instance_forwarding::{ForwardedInstanceAction, ForwardedInstanceRequest};

fn open_uris(action: &ForwardedInstanceAction) -> Vec<&str> {
    match action {
        ForwardedInstanceAction::Open(uris) => uris.iter().map(|uri| uri.as_str()).collect(),
        ForwardedInstanceAction::Activate => panic!("ожидалось действие Open"),
    }
}

#[test]
fn open_keeps_every_uri_in_request_order() {
    let action = admit_open_uris(vec![
        "file:///видео/б.mkv".to_owned(),
        "https://example.org/a".to_owned(),
        "file:///%FF.mkv".to_owned(),
    ])
    .expect("корректные URI");

    assert_eq!(
        open_uris(&action),
        [
            "file:///видео/б.mkv",
            "https://example.org/a",
            "file:///%FF.mkv"
        ]
    );
}

#[test]
fn empty_open_means_activate_only() {
    assert_eq!(
        admit_open_uris(Vec::new()),
        Ok(ForwardedInstanceAction::Activate)
    );
}

#[test]
fn too_many_uris_are_rejected() {
    let uris = vec!["file:///a".to_owned(); MAX_FORWARDED_URIS + 1];
    assert_eq!(
        admit_open_uris(uris),
        Err(ForwardedRequestRejection::TooManyUris {
            count: MAX_FORWARDED_URIS + 1
        })
    );
}

#[test]
fn exactly_the_uri_count_limit_is_accepted() {
    let uris = vec!["file:///a".to_owned(); MAX_FORWARDED_URIS];
    let action = admit_open_uris(uris).expect("ровно лимит допустим");
    assert_eq!(open_uris(&action).len(), MAX_FORWARDED_URIS);
}

#[test]
fn single_uri_over_length_limit_is_rejected_with_its_position() {
    let long_uri = format!("file:///{}", "a".repeat(MAX_FORWARDED_URI_BYTES));
    assert_eq!(
        admit_open_uris(vec!["file:///ok".to_owned(), long_uri]),
        Err(ForwardedRequestRejection::UriTooLong { position: 2 })
    );
}

#[test]
fn total_size_over_limit_is_rejected_even_if_each_uri_fits() {
    let one_uri = format!("file:///{}", "a".repeat(MAX_FORWARDED_URI_BYTES - 16));
    let count = MAX_TOTAL_FORWARDED_URI_BYTES / one_uri.len() + 1;
    assert!(
        count <= MAX_FORWARDED_URIS,
        "тест должен упереться в суммарный лимит"
    );
    assert_eq!(
        admit_open_uris(vec![one_uri; count]),
        Err(ForwardedRequestRejection::TotalTooLarge)
    );
}

#[test]
fn garbage_strings_without_scheme_are_rejected() {
    for garbage in [
        "",
        "relative/path.mkv",
        "/absolute/path.mkv",
        "1http://x",
        ":nothing",
    ] {
        assert_eq!(
            admit_open_uris(vec![garbage.to_owned()]),
            Err(ForwardedRequestRejection::UriWithoutScheme { position: 1 }),
            "строка {garbage:?} должна быть отклонена"
        );
    }
}

#[test]
fn literal_control_characters_are_rejected() {
    for uri in ["file:///a\nb.mkv", "file:///a\0b.mkv", "https://x/\u{7f}"] {
        assert_eq!(
            admit_open_uris(vec![uri.to_owned()]),
            Err(ForwardedRequestRejection::UriWithControlCharacter { position: 1 })
        );
    }
}

#[test]
fn sender_side_constructor_applies_the_same_rules() {
    assert_eq!(
        ForwardedInstanceRequest::open(vec!["not a uri".to_owned()], None),
        Err(ForwardedRequestRejection::UriWithoutScheme { position: 1 })
    );
    let request =
        ForwardedInstanceRequest::open(Vec::new(), None).expect("пустой список — активация");
    assert_eq!(request.action(), &ForwardedInstanceAction::Activate);
}

#[test]
fn activation_token_must_be_short_printable_ascii() {
    assert!(is_valid_activation_token("kwin-0123_abc"));
    assert!(!is_valid_activation_token(""));
    assert!(!is_valid_activation_token("с пробелом"));
    assert!(!is_valid_activation_token("tab\there"));
    assert!(!is_valid_activation_token(
        &"a".repeat(MAX_ACTIVATION_TOKEN_BYTES + 1)
    ));
    assert!(is_valid_activation_token(
        &"a".repeat(MAX_ACTIVATION_TOKEN_BYTES)
    ));
}

#[test]
fn only_the_same_user_is_admitted() {
    assert_eq!(admit_peer(Some(1000), 1000), Ok(()));
    assert_eq!(
        admit_peer(Some(1001), 1000),
        Err(ForwardedRequestRejection::ForeignUser { peer_uid: 1001 })
    );
    assert_eq!(
        admit_peer(Some(0), 1000),
        Err(ForwardedRequestRejection::ForeignUser { peer_uid: 0 }),
        "даже root не может подсунуть файлы чужому сеансу через этот протокол"
    );
    assert_eq!(
        admit_peer(None, 1000),
        Err(ForwardedRequestRejection::UnidentifiedPeer)
    );
}
