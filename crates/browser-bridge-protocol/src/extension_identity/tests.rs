//! Тесты списка «наших» экстеншенов.

use super::*;

#[test]
fn development_ids_of_both_browsers_are_allowed() {
    assert!(is_allowed_extension(
        BrowserFamily::Chromium,
        CHROMIUM_DEVELOPMENT_EXTENSION_ID
    ));
    assert!(is_allowed_extension(
        BrowserFamily::Firefox,
        FIREFOX_EXTENSION_ID
    ));
}

#[test]
fn an_id_is_allowed_only_for_its_own_browser_family() {
    // ID Chrome, предъявленный как ID Firefox (и наоборот), — чужой экстеншен.
    assert!(!is_allowed_extension(
        BrowserFamily::Firefox,
        CHROMIUM_DEVELOPMENT_EXTENSION_ID
    ));
    assert!(!is_allowed_extension(
        BrowserFamily::Chromium,
        FIREFOX_EXTENSION_ID
    ));
}

#[test]
fn similar_ids_are_foreign() {
    let uppercase = CHROMIUM_DEVELOPMENT_EXTENSION_ID.to_ascii_uppercase();
    let with_space = format!("{CHROMIUM_DEVELOPMENT_EXTENSION_ID} ");
    let truncated = &CHROMIUM_DEVELOPMENT_EXTENSION_ID[..31];
    for foreign in [uppercase.as_str(), with_space.as_str(), truncated, ""] {
        assert!(
            !is_allowed_extension(BrowserFamily::Chromium, foreign),
            "{foreign:?} must be foreign"
        );
    }
}

#[test]
fn chromium_ids_have_the_shape_chrome_derives_from_a_key() {
    // 32 буквы a–p: иначе Chrome никогда не передаст такой ID, и запись в списке
    // мертва (опечатка при добавлении ID магазина в сессии 24).
    for allowed in ALLOWED_EXTENSIONS
        .iter()
        .filter(|allowed| allowed.browser == BrowserFamily::Chromium)
    {
        assert_eq!(allowed.extension_id.len(), 32, "{allowed:?}");
        assert!(
            allowed
                .extension_id
                .bytes()
                .all(|letter| (b'a'..=b'p').contains(&letter)),
            "{allowed:?}"
        );
    }
}

#[test]
fn native_host_name_follows_chrome_naming_rules() {
    // Только строчные латинские буквы, цифры, `_` и точки; без точки в начале, в
    // конце и двух точек подряд — иначе Chrome отвергнет `connectNative`.
    let name = NATIVE_HOST_NAME;
    assert!(name.bytes().all(|byte| byte.is_ascii_lowercase()
        || byte.is_ascii_digit()
        || byte == b'_'
        || byte == b'.'));
    assert!(!name.starts_with('.') && !name.ends_with('.') && !name.contains(".."));
}
