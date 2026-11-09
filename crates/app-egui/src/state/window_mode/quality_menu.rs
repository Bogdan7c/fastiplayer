//! Меню кнопки качества «1080p» в окне браузерного режима (сессия 13c, решения 267, 277, 278).
//!
//! Качество выбирает плеер страницы. Кнопка **показывает**, что играет сейчас (разрешение,
//! битрейт, кодек, частота кадров), и даёт пункт «Change quality on the page», который просит
//! браузер показать вкладку. Если адаптер сайта сообщил меню качеств (14a), над этим пунктом
//! стоит список качеств страницы с отметкой выбранного: выбор уходит странице. Подпись кнопки
//! по-прежнему по картинке на экране (13c), а не по пункту страницы.
//!
//! # Владение и слои
//! - [`QualityMenuView`] — снимок на кадр: что показать. Собирается из снимка плеера
//!   ([`PlayerSnapshot`]) и фактов сеанса ([`PageQualityFacts`]); ничего не применяет.
//! - `tier` — чистая функция «картинка → подпись 1080p».
//! - `format` — тексты меню (строки отделены от логики снимка).
//! - `journal` — строка журнала при смене подписи (её читает стенд моста).
//!
//! Нет кадра или нет видео — «показывать нечего»: [`QualityMenuView::Hidden`], кнопки нет.

mod format;
pub(super) mod journal;
mod tier;

use media_core::TrackKind;
use player_core::PlayerSnapshot;

pub(crate) use self::format::QualityMenuText;
pub(super) use self::journal::QualityLabelJournal;
use crate::page_quality::{PageQualityFacts, PageViewOffer};
use crate::page_variants::{PageVariantAvailability, PageVariantMenuView};
use pushed_media_source::MediaBitrate;

/// Размер картинки, которую видит зритель (в пикселях, без отступов декодера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShownPicture {
    /// Ширина картинки.
    pub(crate) width: u32,
    /// Высота картинки.
    pub(crate) height: u32,
}

/// Качества плеера страницы для меню (14a) и можно ли их сейчас выбирать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageQualityChoices {
    menu: PageVariantMenuView,
    availability: PageVariantAvailability,
}

impl PageQualityChoices {
    /// Меню качеств страницы (подписи очищены сеансом, отметка — выбранное на странице).
    pub(crate) const fn menu(&self) -> &PageVariantMenuView {
        &self.menu
    }

    /// Во время рекламы выбор заблокирован (пункты неактивны).
    pub(crate) const fn availability(&self) -> PageVariantAvailability {
        self.availability
    }
}

/// Что меню показывает о воспроизводимом видео.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QualityMenuContent {
    picture: ShownPicture,
    bitrate: Option<MediaBitrate>,
    codec_id: Option<String>,
    frames_per_second: Option<u32>,
    page_view: PageViewOffer,
    /// Список качеств страницы (`None` — адаптер меню не сообщил).
    page_qualities: Option<PageQualityChoices>,
}

/// Снимок меню качества на кадр.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum QualityMenuView {
    /// Показывать нечего (обычное окно, нет видео или кадра): кнопки нет.
    #[default]
    Hidden,
    /// Кнопка с подписью и меню.
    Shown(QualityMenuContent),
}

impl QualityMenuView {
    /// Готовый вид для тестов UI: кадр размера `render`, страницу просить можно или нельзя
    /// по `page_view`, битрейт не известен.
    #[cfg(test)]
    pub(crate) fn sample_for_tests(render: (u32, u32), page_view: PageViewOffer) -> Self {
        test_support::sample_quality_view(render, page_view)
    }

    /// Снимок для окна, где кнопки качества нет вовсе.
    pub(crate) const fn hidden() -> Self {
        Self::Hidden
    }

    /// Собирает снимок из снимка плеера и фактов сеанса.
    ///
    /// **Размер картинки** — `render_width × render_height` кадра, а не `width × height`:
    /// последние — размер декодированной поверхности, которая у аппаратных декодеров
    /// выровнена по блокам (например 1920×1088), тогда как `render_*` — картинка после
    /// обрезки, ровно то, что видит зритель (так же её читают субтитры).
    #[must_use]
    pub(crate) fn from_snapshot(snapshot: &PlayerSnapshot, facts: &PageQualityFacts) -> Self {
        let Some(frame) = snapshot.current_video_frame else {
            return Self::Hidden;
        };
        if frame.render_width == 0 || frame.render_height == 0 {
            return Self::Hidden;
        }
        Self::Shown(QualityMenuContent {
            picture: ShownPicture {
                width: frame.render_width,
                height: frame.render_height,
            },
            bitrate: facts.bitrate,
            codec_id: selected_video_codec_id(snapshot),
            frames_per_second: format::frames_per_second(snapshot.video_frame_duration_estimate),
            page_view: facts.page_view,
            page_qualities: None,
        })
    }

    /// Тот же снимок со списком качеств страницы (`menu` — `None`: меню нет). У скрытой
    /// кнопки списка нет: показывать его негде.
    #[must_use]
    pub(crate) fn with_page_qualities(
        self,
        menu: Option<&PageVariantMenuView>,
        availability: PageVariantAvailability,
    ) -> Self {
        match self {
            Self::Hidden => Self::Hidden,
            Self::Shown(content) => Self::Shown(QualityMenuContent {
                page_qualities: menu.map(|menu| PageQualityChoices {
                    menu: menu.clone(),
                    availability,
                }),
                ..content
            }),
        }
    }

    /// Список качеств страницы (`None` — кнопки нет или меню страница не сообщила).
    #[must_use]
    pub(crate) const fn page_qualities(&self) -> Option<&PageQualityChoices> {
        match self {
            Self::Hidden => None,
            Self::Shown(content) => content.page_qualities.as_ref(),
        }
    }

    /// Подпись кнопки («1080p»); `None` — кнопки нет.
    #[must_use]
    pub(crate) fn tier_label(&self) -> Option<String> {
        match self {
            Self::Hidden => None,
            Self::Shown(content) => Some(tier::tier_label(content.picture)),
        }
    }

    /// Есть ли что показать.
    #[must_use]
    pub(crate) const fn is_shown(&self) -> bool {
        matches!(self, Self::Shown(_))
    }

    /// Тексты строк меню (`None` — кнопки нет).
    #[must_use]
    pub(crate) fn text(&self) -> Option<QualityMenuText> {
        match self {
            Self::Hidden => None,
            Self::Shown(content) => Some(QualityMenuText::of(content)),
        }
    }

    /// Можно ли предложить пункт «Change quality on the page» и в каком виде.
    #[must_use]
    pub(crate) const fn page_view_offer(&self) -> PageViewOffer {
        match self {
            Self::Hidden => PageViewOffer::NotAttached,
            Self::Shown(content) => content.page_view,
        }
    }

    /// Строка журнала при смене подписи; `None` — кнопки нет.
    #[must_use]
    pub(crate) fn journal_entry(&self) -> Option<journal::QualityJournalEntry> {
        match self {
            Self::Hidden => None,
            Self::Shown(content) => Some(journal::QualityJournalEntry {
                label: tier::tier_label(content.picture),
                picture: content.picture,
                bitrate: content.bitrate,
            }),
        }
    }
}

impl QualityMenuContent {
    pub(crate) const fn picture(&self) -> ShownPicture {
        self.picture
    }

    pub(crate) const fn bitrate(&self) -> Option<MediaBitrate> {
        self.bitrate
    }

    pub(crate) fn codec_id(&self) -> Option<&str> {
        self.codec_id.as_deref()
    }

    pub(crate) const fn frames_per_second(&self) -> Option<u32> {
        self.frames_per_second
    }
}

/// Идентификатор кодека выбранной видеодорожки (`None` — дорожка не выбрана или не найдена).
fn selected_video_codec_id(snapshot: &PlayerSnapshot) -> Option<String> {
    let selected = snapshot.selected_tracks.video_track?;
    snapshot
        .tracks
        .iter()
        .find(|track| track.id == selected && track.kind == TrackKind::Video)
        .map(|track| track.codec_id.clone())
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
