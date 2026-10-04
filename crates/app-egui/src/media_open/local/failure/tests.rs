use std::io;

use demux_api::{DemuxFactoryOpenError, DemuxOpenError, DemuxProbeRejection};
use media_source_open::local_media::LocalDemuxOpenError;
use source_core::SourceError;

use super::super::PrepareLocalOpenError;
use super::{LocalOpenFailureOutcome, LocalOpenFailureReason};

fn local_io_error(kind: io::ErrorKind) -> PrepareLocalOpenError {
    PrepareLocalOpenError::Source(SourceError::LocalIo {
        context: "open",
        source: io::Error::from(kind),
    })
}

fn demux_open_error(open_error: DemuxOpenError) -> PrepareLocalOpenError {
    PrepareLocalOpenError::Demux(LocalDemuxOpenError::Open(open_error))
}

fn test_factory_id() -> demux_api::DemuxFactoryId {
    demux_api::DemuxFactoryId::new("test.factory").expect("валидный тестовый factory id")
}

fn failed(reason: LocalOpenFailureReason) -> LocalOpenFailureOutcome {
    LocalOpenFailureOutcome::Failed(reason)
}

/// Каждый `io::ErrorKind`, важный пользователю, получает свою причину.
#[test]
fn source_io_kinds_map_to_distinct_user_reasons() {
    let cases = [
        (
            io::ErrorKind::NotFound,
            LocalOpenFailureReason::FileNotFound,
        ),
        (
            io::ErrorKind::PermissionDenied,
            LocalOpenFailureReason::AccessDenied,
        ),
        (
            io::ErrorKind::IsADirectory,
            LocalOpenFailureReason::IsDirectory,
        ),
        (
            io::ErrorKind::TimedOut,
            LocalOpenFailureReason::ReadTimedOut,
        ),
        (
            io::ErrorKind::UnexpectedEof,
            LocalOpenFailureReason::ReadFailed,
        ),
    ];

    for (kind, expected_reason) in cases {
        assert_eq!(
            local_io_error(kind).user_outcome(),
            failed(expected_reason),
            "io kind: {kind:?}"
        );
    }
}

/// Повторная проверка файла после разбора использует ту же таблицу io-причин.
#[test]
fn revalidation_io_uses_same_kind_classification_as_open() {
    let error = PrepareLocalOpenError::RevalidationIo(io::Error::from(io::ErrorKind::NotFound));

    assert_eq!(
        error.user_outcome(),
        failed(LocalOpenFailureReason::FileNotFound)
    );
}

/// «Не узнали формат» и «узнали, но данные битые» — разные причины.
#[test]
fn demux_outcomes_separate_unrecognized_from_damaged() {
    let cases = [
        (
            DemuxOpenError::NoMatch,
            LocalOpenFailureReason::UnrecognizedFormat,
        ),
        (
            DemuxOpenError::ProbeRejected(DemuxProbeRejection::Truncated {
                available_bytes: 10,
                required_bytes: 564,
            }),
            LocalOpenFailureReason::DamagedOrTruncated,
        ),
        (
            DemuxOpenError::ProbeRejected(DemuxProbeRejection::Malformed {
                reason: "bad header".to_owned(),
            }),
            LocalOpenFailureReason::DamagedOrTruncated,
        ),
        (
            DemuxOpenError::ProbeRejected(DemuxProbeRejection::InputFailure {
                reason: "read failed".to_owned(),
            }),
            LocalOpenFailureReason::ReadFailed,
        ),
        (
            DemuxOpenError::ProbeRejected(DemuxProbeRejection::DeadlineExceeded {
                max_duration: std::time::Duration::from_secs(1),
            }),
            LocalOpenFailureReason::ReadTimedOut,
        ),
        (
            DemuxOpenError::FactoryRejected {
                factory_id: test_factory_id(),
                source: DemuxFactoryOpenError::Backend(anyhow::anyhow!("broken atom")),
            },
            LocalOpenFailureReason::DamagedOrTruncated,
        ),
        (
            DemuxOpenError::FactoryRejected {
                factory_id: test_factory_id(),
                source: DemuxFactoryOpenError::Rejected {
                    reason: "unsupported variant".to_owned(),
                },
            },
            LocalOpenFailureReason::UnrecognizedFormat,
        ),
    ];

    for (open_error, expected_reason) in cases {
        let description = format!("{open_error:?}");
        assert_eq!(
            demux_open_error(open_error).user_outcome(),
            failed(expected_reason),
            "demux error: {description}"
        );
    }
}

/// Сбой сборки набора разборщиков — проблема плеера, а не файла.
#[test]
fn registry_setup_failure_is_internal_error_not_file_problem() {
    let error = PrepareLocalOpenError::Demux(LocalDemuxOpenError::RegistrySetup(anyhow::anyhow!(
        "registry"
    )));

    assert_eq!(
        error.user_outcome(),
        failed(LocalOpenFailureReason::InternalError)
    );
}

/// Отмена на любом уровне — не ошибка для пользователя.
#[test]
fn cancellation_is_separate_outcome_not_failure_reason() {
    assert_eq!(
        PrepareLocalOpenError::Cancelled.user_outcome(),
        LocalOpenFailureOutcome::Cancelled
    );
    assert_eq!(
        PrepareLocalOpenError::Source(SourceError::Cancelled).user_outcome(),
        LocalOpenFailureOutcome::Cancelled
    );
}

/// Лог получает внутреннюю причину, которую верхний `Display` прячет.
#[test]
fn diagnostic_chain_includes_hidden_source_causes() {
    let error = demux_open_error(DemuxOpenError::ProbeRejected(
        DemuxProbeRejection::Malformed {
            reason: "pmt section crc mismatch".to_owned(),
        },
    ));

    let top_level_only = error.to_string();
    let chain = error.diagnostic_chain();

    assert!(!top_level_only.contains("pmt section crc mismatch"));
    assert!(chain.starts_with(&top_level_only));
    assert!(chain.contains("demux probe отклонён"));
    assert!(chain.contains("pmt section crc mismatch"));
}

/// Звено, уже встроенное в `Display` верхнего уровня, не повторяется в цепочке.
#[test]
fn diagnostic_chain_does_not_repeat_causes_already_in_display() {
    let error = local_io_error(io::ErrorKind::NotFound);
    let os_text = io::Error::from(io::ErrorKind::NotFound).to_string();

    let chain = error.diagnostic_chain();

    assert_eq!(chain.matches(&os_text).count(), 1, "{chain}");
}
