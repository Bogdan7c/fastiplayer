use reqwest::blocking::Client;

use super::map_reqwest_error;
use crate::{SecretHttpUrl, SourceError};

/// Фиксирует non-timeout ветку без зависимости от момента остановки локального HTTP server-а.
#[test]
fn malformed_request_is_mapped_to_secret_safe_http_request_error() {
    // Некорректный относительный URL создаёт deterministic builder error до любого сетевого I/O.
    let request_error = Client::new()
        .get("relative/resource")
        .send()
        .expect_err("relative URL must be rejected before a request is sent");
    assert!(!request_error.is_timeout());

    // Внешний URL нужен только как redacted identity нормализованной source-ошибки.
    let normalized_error = map_reqwest_error(
        "deterministic-error-mapping",
        &SecretHttpUrl::from_secret_for_open("https://media.example.test/private?token=secret"),
        request_error,
    );

    // Нормализация сохраняет категорию и удаляет URL из вложенной reqwest error.
    match normalized_error {
        SourceError::HttpRequest {
            operation, source, ..
        } => {
            assert_eq!(operation, "deterministic-error-mapping");
            assert!(source.url().is_none());
        }
        other => panic!("expected HttpRequest normalization, got {other:?}"),
    }
}
