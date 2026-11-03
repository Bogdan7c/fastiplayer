//! Отказ перемотки зрителя глазами окна (сессия 09a2, дыра Д-5, решение владельца).
//!
//! Во время досмотра (страница ушла) зритель может перематывать только по скачанному:
//! просить байты некому. Клик за скачанное отклоняется в
//! [`super::viewer_transport`] — плееру ничего не уходит, поэтому бегунок остаётся у
//! текущей позиции и спиннера нет. Чтобы зритель понял, почему клик «не сработал»,
//! окно показывает короткое сообщение «Only the downloaded part can be played».
//!
//! Здесь — только знание медиа-части «за этот шаг был отказ» ([`ViewerSeekNotice`]) и его
//! выдача окну один раз ([`SessionMedia::take_viewer_seek_notice`]). Текст и показ — у
//! окна (`window::app`); безоконный процесс сообщений не показывает (отказ и так в
//! журнале).

use tracing::info;

use super::SessionMedia;

/// Сказать ли зрителю окна об отказе его перемотки за этот шаг.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ViewerSeekNotice {
    /// Говорить нечего.
    #[default]
    Nothing,
    /// Перемотка зрителя за скачанное во время досмотра отклонена.
    OutsideDownloaded,
}

impl SessionMedia {
    /// Перемотка зрителя за скачанное отклонена (страницы нет): плееру ничего, окну —
    /// сообщение. Несколько отказов до забора окном — одно сообщение.
    pub(super) fn refuse_viewer_seek_outside_downloaded(&mut self) {
        info!("Viewer seek outside of what was downloaded refused: the page is gone");
        self.viewer_seek_notice = ViewerSeekNotice::OutsideDownloaded;
    }

    /// Отказ перемотки зрителя за шаг, о котором окно ещё не сказало (забирает его).
    pub(crate) fn take_viewer_seek_notice(&mut self) -> ViewerSeekNotice {
        std::mem::take(&mut self.viewer_seek_notice)
    }
}
