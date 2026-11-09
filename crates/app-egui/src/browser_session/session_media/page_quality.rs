//! Качество страницы глазами окна: просьба «показать страницу» и факты для меню (13c).
//!
//! Зритель в меню кнопки качества нажимает «Change quality on the page»: качество выбирает
//! плеер страницы, а не мы. Окно отдаёт намерение ([`ViewerTransportIntent::ShowPage`]),
//! здесь оно исполняется: странице уходит `show_page` (протокол 1.6, решение 278), а окну —
//! одноразовое слово [`PageViewNotice::Requested`], чтобы показать тост «переключитесь на
//! браузер». Настоящий подъём окна браузера на KDE Wayland — сессия 17 (решение 208).
//!
//! Второе — факты для меню на каждый проход ([`SessionMedia::page_quality_facts`]):
//! - битрейт — только у поколения, которое зритель видит и которое приведено к странице
//!   (те же условия, что у скачанного для скраба, [`super::scrub_range`]): иначе число
//!   описывало бы другой ролик;
//! - «можно ли просить страницу» — с различием причин ([`PageViewOffer`]).
//!
//! [`ViewerTransportIntent::ShowPage`]: crate::viewer_transport::ViewerTransportIntent::ShowPage

use pushed_media_source::MediaBitrate;
use tracing::{debug, info, warn};

use super::SessionMedia;
use crate::browser_session::outbound::{PageViewPermission, ShowPageRequestOutcome};
use crate::browser_session::playback::PlaybackObservation;
use crate::page_quality::{PageQualityFacts, PageViewOffer};

/// Просьба показать страницу ушла в очередь к странице (строку читает стенд и тесты).
pub(crate) const SHOW_PAGE_SENT_LINE: &str = "Show page requested by the viewer";

/// Сказать ли зрителю окна, что просьба показать страницу принята.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageViewNotice {
    /// Говорить нечего.
    #[default]
    Nothing,
    /// Просьба ушла странице: окно говорит «страница открыта в браузере — переключитесь».
    Requested,
}

impl SessionMedia {
    /// Зритель просит показать страницу: если она слушает и экстеншен знает `show_page` —
    /// просьба уходит, окну остаётся слово для тоста; иначе ничего не уходит и тоста нет.
    pub(super) fn viewer_asks_to_show_page(&mut self, page_listens: bool) {
        if !page_listens {
            debug!("Show page ignored: the page is gone");
            return;
        }
        match self.outbound.request_show_page() {
            ShowPageRequestOutcome::Queued => {
                info!("{SHOW_PAGE_SENT_LINE}");
                self.page_view_notice = PageViewNotice::Requested;
            }
            ShowPageRequestOutcome::ProtocolTooOld => {
                warn!("Show page not sent: the extension is older than protocol 1.6");
            }
            ShowPageRequestOutcome::NotAttachedYet => {
                debug!("Show page not sent: the page has not attached yet");
            }
        }
    }

    /// Слово окну о просьбе зрителя за шаг (забирает его): тост показывается один раз.
    pub(crate) fn take_page_view_notice(&mut self) -> PageViewNotice {
        std::mem::take(&mut self.page_view_notice)
    }

    /// Факты для меню качества на этот проход (чистое чтение, ничего не меняет). Что плеер
    /// показывает, берётся из наблюдения последнего прохода; пока наблюдения не было —
    /// битрейт неизвестен.
    pub(crate) fn page_quality_facts(&self) -> PageQualityFacts {
        PageQualityFacts {
            bitrate: self
                .last_observation
                .as_ref()
                .and_then(|observation| self.bitrate_for_viewer(observation)),
            page_view: self.page_view_offer(),
        }
    }

    /// Битрейт поколения, которое видит зритель, или `None` (условия — в описании модуля).
    fn bitrate_for_viewer(&self, observation: &PlaybackObservation) -> Option<MediaBitrate> {
        if self.pending.is_some() {
            return None;
        }
        match &self.loaded {
            Some(loaded) if loaded.is_shown_by(observation) && loaded.page_state_reconciled => {
                loaded.feed.video_bitrate()
            }
            _ => None,
        }
    }

    /// Можно ли сейчас предложить зрителю «показать страницу».
    fn page_view_offer(&self) -> PageViewOffer {
        let page_listens = self.drain.is_none() && self.current_session.is_some();
        if !page_listens {
            return PageViewOffer::PageAway;
        }
        match self.outbound.page_view_permission() {
            PageViewPermission::Allowed => PageViewOffer::Available,
            PageViewPermission::NotAttachedYet => PageViewOffer::NotAttached,
            PageViewPermission::ProtocolTooOld => PageViewOffer::ExtensionTooOld,
        }
    }
}
