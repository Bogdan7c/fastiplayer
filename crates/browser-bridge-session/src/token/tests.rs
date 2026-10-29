use super::*;

#[test]
fn base64url_matches_rfc4648_vectors_without_padding() {
    assert_eq!(encode_base64url(b""), "");
    assert_eq!(encode_base64url(b"f"), "Zg");
    assert_eq!(encode_base64url(b"fo"), "Zm8");
    assert_eq!(encode_base64url(b"foo"), "Zm9v");
    assert_eq!(encode_base64url(b"foob"), "Zm9vYg");
    assert_eq!(encode_base64url(b"fooba"), "Zm9vYmE");
    assert_eq!(encode_base64url(b"foobar"), "Zm9vYmFy");
    // Байты, дающие «+» и «/» в обычном base64, здесь — «-» и «_».
    assert_eq!(encode_base64url(&[0xFB, 0xFF, 0xBF]), "-_-_");
}

#[test]
fn os_tokens_are_32_url_safe_characters_and_differ() {
    let mut source = OsRandomTokenSource;
    let first = source.next_token().unwrap();
    let second = source.next_token().unwrap();
    // 24 байта → ровно 32 символа (192 бита), и SessionToken их принял.
    assert_eq!(first.as_str().len(), 32);
    assert!(
        first
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    );
    assert!(!first.matches(&second), "два пропуска совпали");
}

/// Источник, который всегда ломается: проверяет, что ошибка доходит до вызывающего.
struct FailingSource;

impl SessionTokenSource for FailingSource {
    fn next_token(&mut self) -> Result<SessionToken, TokenSourceError> {
        Err(TokenSourceError::EntropyUnavailable {
            reason: "тест".to_owned(),
        })
    }
}

#[test]
fn failing_source_error_is_returned_through_the_trait_object() {
    let mut source: Box<dyn SessionTokenSource> = Box::new(FailingSource);
    let error = source.next_token().unwrap_err();
    assert!(matches!(error, TokenSourceError::EntropyUnavailable { .. }));
}

#[test]
fn too_short_token_text_is_reported_as_malformed_token() {
    let violation = SessionToken::new("short").unwrap_err();
    let error = TokenSourceError::from(violation);
    assert!(matches!(error, TokenSourceError::MalformedToken(_)));
}
