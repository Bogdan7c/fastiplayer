//! Классификация фатальной ошибки чтения demuxer-а для player-а (сессия 16).
//!
//! player-core не знает про HTTP и `SourceError`. Сетевой источник, у которого
//! связь не вернулась за бюджет переподключения, помечает ошибку стандартной
//! меткой `std::io::ErrorKind::NetworkDown` (её ставят адаптеры source → demux в
//! `symphonia-demux` и `demux-api`). По этой метке player отличает «пропала сеть»
//! (`PlayerErrorKind::NetworkError`) от повреждённого файла (`DemuxError`), чтобы
//! приложение показало человеку правильную причину.

use crate::{PlayerError, PlayerErrorKind};

/// Строит фатальную ошибку player-а из ошибки `Demuxer::next_event`.
///
/// Текст сообщения сохраняет полную техническую цепочку для логов; что показать
/// пользователю, решает приложение по `kind`.
pub(super) fn player_error_for_demux_read_failure(error: &anyhow::Error) -> PlayerError {
    let kind = if is_network_unavailable(error) {
        PlayerErrorKind::NetworkError
    } else {
        PlayerErrorKind::DemuxError
    };
    PlayerError::new(kind, format!("Ошибка чтения packet: {error}"))
}

/// Ищет в цепочке причин метку «сеть недоступна».
fn is_network_unavailable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NetworkDown)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Обёртка как у Symphonia: `DemuxError::Io(io::Error)` отдаёт io-ошибку через `source()`.
    #[derive(Debug)]
    struct WrappedIo(std::io::Error);

    impl std::fmt::Display for WrappedIo {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "Ошибка чтения: {}", self.0)
        }
    }

    impl std::error::Error for WrappedIo {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn network_down_anywhere_in_chain_is_network_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NetworkDown, "connection refused");
        let error = anyhow::Error::new(WrappedIo(io_error)).context("demux worker failure");

        let player_error = player_error_for_demux_read_failure(&error);

        assert_eq!(player_error.kind, PlayerErrorKind::NetworkError);
        assert!(player_error.message.contains("demux worker failure"));
    }

    #[test]
    fn other_io_failure_stays_demux_error() {
        let io_error = std::io::Error::other("corrupted atom");
        let error = anyhow::Error::new(WrappedIo(io_error));

        assert_eq!(
            player_error_for_demux_read_failure(&error).kind,
            PlayerErrorKind::DemuxError
        );
    }

    #[test]
    fn non_io_failure_stays_demux_error() {
        let error = anyhow::anyhow!("parse failure");

        assert_eq!(
            player_error_for_demux_read_failure(&error).kind,
            PlayerErrorKind::DemuxError
        );
    }
}
