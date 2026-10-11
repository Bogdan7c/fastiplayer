//! Общий словарь отмены pending-подготовки источника до появления coordinator-request.
//!
//! Подготовку (диалог → чтение файла → сеть/yt-dlp) ведут разные владельцы: стартовый
//! `StartupMediaController` и `AppState` (кнопка Open, drag & drop, Retry). Транспортная
//! команда Stop не должна знать, кто из них сейчас занят: она вызывает у каждого
//! владельца intent-метод `cancel_pending_*_preparation(cause)` и получает типизированный
//! итог. Здесь живёт только словарь; само состояние и токены остаются у владельцев.

/// Почему отменяется подготовка источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationCancelCause {
    /// Пользователь нажал Stop (кнопка, MPRIS, горячая клавиша Stop).
    ///
    /// Семантика та же, что у тихой отмены C5 (`UserCancelled`/`TransportStop`): никаких
    /// ошибок и уведомлений, очередь не перебирается, элемент не считается битым.
    UserStop,
}

/// Что получилось у владельца подготовки по запросу отмены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationCancelOutcome {
    /// У владельца не было подготовки до coordinator-request: ничего не изменилось.
    NothingPending,
    /// Подготовка отменена: её результат больше никогда не будет установлен.
    Cancelled,
}

impl PreparationCancelOutcome {
    /// Объединяет итоги нескольких владельцев: достаточно, чтобы отменил хотя бы один.
    #[must_use]
    pub(crate) const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::NothingPending, Self::NothingPending) => Self::NothingPending,
            _ => Self::Cancelled,
        }
    }

    /// `true`, если хотя бы одна подготовка была отменена.
    #[must_use]
    pub(crate) const fn cancelled_anything(self) -> bool {
        matches!(self, Self::Cancelled)
    }
}
