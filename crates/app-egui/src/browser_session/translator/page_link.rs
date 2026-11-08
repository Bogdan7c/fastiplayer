//! Страница сеанса ушла и вернулась, а связь держится («сирота», протокол 1.4, сессия
//! 11b1, решения 240–241).
//!
//! Что здесь живёт (и только это — остальное в [`super`]):
//! - `page_left` запечатывает текущий поток так же, как конец экземпляра: страницы нет,
//!   байтов больше не будет, плеер досматривает принятое (файл — `seal`, MSE — «скачивание
//!   оборвано», HLS — печать очереди ресурсов). Но экземпляр остаётся активным: страница
//!   может вернуться в ТОТ ЖЕ сеанс и начать новое поколение;
//! - `page_returned` и `page_reopen_failed` переходник только передаёт главному циклу:
//!   что делать с досмотром, решает медиа-часть, а с выходом процесса — его машина.
//!
//! Все три идут уведомлением [`IntakeNotice::PageLink`] в порядке кадров на проводе:
//! «ушла» — после всех данных, принятых до ухода, «вернулась» — раньше данных нового
//! поколения. Поэтому главный цикл никогда не увидит новое поколение раньше возврата.
//! Что страница вправе слать, пока её нет, проверяет сторож протокола на сервере
//! (`StreamGate`): сюда доходит только разрешённое.

use browser_bridge_protocol::session::TabGoneReason;
use browser_bridge_session::SessionRef;
use tracing::info;

use super::file_generation::SealOccasion;
use super::{InstancePhase, InstanceState, ProtocolTranslator, StreamFactory};
use crate::browser_session::intake_notice::{IntakeNotice, PageLinkChange};

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// Страница ушла, связь держится: текущий поток запечатан и забыт, экземпляр жив.
    pub(super) fn follow_page_left(
        &mut self,
        session: &SessionRef,
        reason: TabGoneReason,
        out: &mut Vec<IntakeNotice>,
    ) {
        self.seal_current_stream(SealOccasion::PageLeft);
        // Писатель запечатанного поколения больше не нужен: после возврата страница
        // начнёт новое поколение, а его `generation_started` не должен печатать этот
        // поток второй раз.
        if let Some(InstanceState {
            phase: InstancePhase::Open(current),
            ..
        }) = self.instance.as_mut()
        {
            *current = None;
        }
        info!(
            ?reason,
            "The page left but the link stays; its stream input is closed"
        );
        out.push(IntakeNotice::PageLink {
            session: session.clone(),
            change: PageLinkChange::Left(reason),
        });
    }

    /// Страница вернулась или не смогла вернуться: решает главный цикл.
    pub(super) fn report_page_link_change(
        session: &SessionRef,
        change: PageLinkChange,
        out: &mut Vec<IntakeNotice>,
    ) {
        info!(?change, "The page link changed");
        out.push(IntakeNotice::PageLink {
            session: session.clone(),
            change,
        });
    }
}
