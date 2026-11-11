//! Склад вариантов страницы на сеанс (протокол 1.0 `variant_menu_changed` и 1.7
//! `page_navigation`, сессия 14a, решения 279–283).
//!
//! Адаптер сайта сообщает меню озвучек и качеств плеера страницы и «есть ли следующая
//! серия». Склад хранит последнее слово каждого вида и отвечает на два вопроса:
//! - что показать окну ([`PageVariantShelf::menu_view`], [`PageVariantShelf::next_episode`]);
//! - какой `variant_id` странице отправить по выбору зрителя ([`PageVariantShelf::lookup`]),
//!   и не устарел ли этот выбор.
//!
//! **Кто владеет.** Склад живёт в медиа-части сеанса (`SessionMedia`): она знает, какой сеанс
//! текущий, и уведомление чужого экземпляра сюда не доходит. Новый сеанс — пустой склад
//! ([`PageVariantShelf::start_new_session`]); поколения склад не трогают (меню — про страницу,
//! а не про `MediaSource`).
//!
//! **Инварианты.**
//! 1. Без меню нет пунктов; меню одного вида не трогает другой вид.
//! 2. Меню `subtitles` хранится как остальные (15a2): это строки меню CC. Сами документы
//!    субтитров приходят дорожками 13b отдельно, склад о них не знает.
//! 3. Ревизия растёт при каждой смене содержимого меню (любого вида) и не сбрасывается
//!    новым сеансом: выбор по меню прошлой страницы не совпадёт с ревизией новой. Повтор того
//!    же меню ревизию не меняет — иначе страница, которая шлёт меню по таймеру, делала бы
//!    устаревшим каждый выбор зрителя.
//! 4. Пустой список пунктов — меню пропало (так говорит протокол).
//! 5. До первого `page_navigation` следующей серии нет.

use std::sync::Arc;

use browser_bridge_protocol::session::{
    NextEpisodeAvailability, SubtitleChoice, VariantKind, VariantMenu, VariantOutcome,
    VariantRequest, VariantSelection,
};
use browser_bridge_protocol::text::VariantId;

use crate::page_variants::{
    PageVariantChoice, PageVariantIndex, PageVariantKind, PageVariantMenuRevision,
    PageVariantMenuView, sanitize_page_variant_label,
};

/// Что слово страницы о вариантах (`variant_menu_changed` / `page_navigation`) несёт
/// медиа-части. Переходник протокола переводит в него сообщение страницы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageVariantUpdate {
    /// Новое меню одного вида (озвучки, качества или субтитров).
    Menu(VariantMenu),
    /// Есть ли на странице следующая серия.
    Navigation(NextEpisodeAvailability),
    /// Подсказка субтитров для этой серии (`subtitle_preference`, 1.7). Её склад — не этот, а
    /// склад субтитров сеанса (`page_subtitles::shelf`): там её прочтёт владелец субтитров.
    SubtitlePreference(SubtitleChoice),
    /// Исход выбора зрителя (`variant_outcome`, 1.8, сессия 15c2): склад меню его не читает,
    /// сверяет со своим ожидающим выбором медиа-часть (`session_media::page_variant_outcome`).
    Outcome(PageVariantOutcomeWord),
}

/// Слово страницы «чем кончилась просьба плеера» (протокол 1.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageVariantOutcomeWord {
    /// На какую просьбу плеера это ответ.
    pub(crate) request: VariantRequest,
    /// Что сделала страница.
    pub(crate) outcome: VariantOutcome,
}

/// О чём исход: у каждого предмета своё место в ящике, чтобы меню или навигация не вытеснили
/// ещё не прочитанный исход, а новый исход того же предмета заменил старый.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeSubject {
    /// Выбор пункта меню этого вида.
    Menu(VariantKind),
    /// Следующая серия.
    NextEpisode,
}

/// Место слова в ящике: новое слово того же места вытесняет неисполненное старое
/// (меню каждого вида и навигация — свои места).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageVariantSlot {
    /// Меню этого вида.
    Menu(VariantKind),
    /// Навигация страницы.
    Navigation,
    /// Подсказка субтитров.
    SubtitlePreference,
    /// Исход выбора зрителя по этому предмету (свои места, не пересекаются с меню).
    Outcome(OutcomeSubject),
}

impl PageVariantUpdate {
    /// Место этого слова (см. [`PageVariantSlot`]).
    pub(crate) const fn slot(&self) -> PageVariantSlot {
        match self {
            Self::Menu(menu) => PageVariantSlot::Menu(menu.kind),
            Self::Navigation(_) => PageVariantSlot::Navigation,
            Self::SubtitlePreference(_) => PageVariantSlot::SubtitlePreference,
            Self::Outcome(word) => PageVariantSlot::Outcome(match &word.request {
                VariantRequest::SelectVariant { menu, .. } => OutcomeSubject::Menu(*menu),
                VariantRequest::NextEpisode {} => OutcomeSubject::NextEpisode,
            }),
        }
    }
}

/// Что стало с присланным меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuStore {
    /// Меню легло на склад с новой ревизией.
    Stored(PageVariantMenuRevision),
    /// Пустой список: меню этого вида пропало (ревизия новая).
    Removed(PageVariantMenuRevision),
    /// То же меню, что уже лежит (или пустое при отсутствующем): ничего не сменилось.
    Unchanged,
}

/// Почему по выбору зрителя странице нечего отправить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariantLookupRefusal {
    /// Меню этого вида нет.
    NoMenu,
    /// Меню сменилось после того, как зритель его видел: выбор устарел.
    StaleMenu,
    /// В меню нет пункта с таким номером.
    NoSuchOption,
}

/// Пункт склада, найденный по выбору зрителя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoundVariant {
    /// Вид меню протокола.
    pub(crate) kind: VariantKind,
    /// Что вернуть странице в `select_variant`.
    pub(crate) variant_id: VariantId,
    /// Очищенная подпись (для тоста; в журнал не идёт).
    pub(crate) label: Arc<str>,
}

/// Меню на складе: оригинал страницы (ради идентификаторов) и вид для окна.
#[derive(Debug, Clone)]
struct StoredMenu {
    menu: VariantMenu,
    view: PageVariantMenuView,
}

/// Склад вариантов одного сеанса (см. описание модуля).
#[derive(Debug, Clone)]
pub(crate) struct PageVariantShelf {
    audio: Option<StoredMenu>,
    quality: Option<StoredMenu>,
    subtitles: Option<StoredMenu>,
    next_episode: NextEpisodeAvailability,
    /// Номер последней выданной ревизии (растёт за жизнь процесса, инвариант 3).
    last_revision: u64,
}

impl Default for PageVariantShelf {
    fn default() -> Self {
        Self {
            audio: None,
            quality: None,
            subtitles: None,
            // Инвариант 5: до первого слова страницы следующей серии нет.
            next_episode: NextEpisodeAvailability::Unavailable,
            last_revision: 0,
        }
    }
}

impl PageVariantShelf {
    /// Новое меню вида `menu.kind` заменяет прежнее того же вида; другой вид не трогается.
    pub(crate) fn store_menu(&mut self, menu: VariantMenu) -> MenuStore {
        let kind = page_variant_kind(menu.kind);
        let current = self.slot(kind);
        if menu.options.is_empty() {
            if current.is_none() {
                return MenuStore::Unchanged;
            }
            *self.slot_mut(kind) = None;
            return MenuStore::Removed(self.next_revision());
        }
        if current.is_some_and(|stored| stored.menu == menu) {
            return MenuStore::Unchanged;
        }
        let revision = self.next_revision();
        let view = menu_view(kind, revision, &menu);
        *self.slot_mut(kind) = Some(StoredMenu { menu, view });
        MenuStore::Stored(revision)
    }

    /// Слово страницы о следующей серии (последнее главнее); `true` — оно сменилось.
    pub(crate) fn note_navigation(&mut self, availability: NextEpisodeAvailability) -> bool {
        let changed = self.next_episode != availability;
        self.next_episode = availability;
        changed
    }

    /// Есть ли на странице следующая серия (по последнему слову страницы).
    pub(crate) const fn next_episode(&self) -> NextEpisodeAvailability {
        self.next_episode
    }

    /// Меню этого вида для окна (`None` — меню нет).
    pub(crate) fn menu_view(&self, kind: PageVariantKind) -> Option<PageVariantMenuView> {
        self.slot(kind).map(|stored| stored.view.clone())
    }

    /// Пункт по выбору зрителя: только из ПОСЛЕДНЕГО меню этого вида и только если зритель
    /// видел именно его ревизию.
    pub(crate) fn lookup(
        &self,
        choice: PageVariantChoice,
    ) -> Result<FoundVariant, VariantLookupRefusal> {
        let stored = self.slot(choice.kind).ok_or(VariantLookupRefusal::NoMenu)?;
        if stored.view.revision() != choice.revision {
            return Err(VariantLookupRefusal::StaleMenu);
        }
        let option = stored
            .menu
            .options
            .get(choice.index.get())
            .ok_or(VariantLookupRefusal::NoSuchOption)?;
        let label = stored
            .view
            .label(choice.index)
            .map(Arc::from)
            .unwrap_or_else(|| fallback_label(choice.kind, choice.index));
        Ok(FoundVariant {
            kind: stored.menu.kind,
            variant_id: option.id.clone(),
            label,
        })
    }

    /// Новый сеанс: меню и навигация прежней страницы к новой не относятся. Счётчик ревизий
    /// не сбрасывается (инвариант 3).
    pub(crate) fn start_new_session(&mut self) {
        self.audio = None;
        self.quality = None;
        self.subtitles = None;
        self.next_episode = NextEpisodeAvailability::Unavailable;
    }

    fn next_revision(&mut self) -> PageVariantMenuRevision {
        self.last_revision += 1;
        PageVariantMenuRevision::new(self.last_revision)
    }

    const fn slot(&self, kind: PageVariantKind) -> Option<&StoredMenu> {
        match kind {
            PageVariantKind::Audio => self.audio.as_ref(),
            PageVariantKind::Quality => self.quality.as_ref(),
            PageVariantKind::Subtitles => self.subtitles.as_ref(),
        }
    }

    const fn slot_mut(&mut self, kind: PageVariantKind) -> &mut Option<StoredMenu> {
        match kind {
            PageVariantKind::Audio => &mut self.audio,
            PageVariantKind::Quality => &mut self.quality,
            PageVariantKind::Subtitles => &mut self.subtitles,
        }
    }
}

/// Вид меню протокола → вид окна.
pub(crate) const fn page_variant_kind(kind: VariantKind) -> PageVariantKind {
    match kind {
        VariantKind::Audio => PageVariantKind::Audio,
        VariantKind::Quality => PageVariantKind::Quality,
        VariantKind::Subtitles => PageVariantKind::Subtitles,
    }
}

/// Вид меню для окна: очищенные подписи (пустая после очистки — запасная «Audio 2»).
fn menu_view(
    kind: PageVariantKind,
    revision: PageVariantMenuRevision,
    menu: &VariantMenu,
) -> PageVariantMenuView {
    let labels = menu
        .options
        .iter()
        .enumerate()
        .map(|(position, option)| {
            let clean = sanitize_page_variant_label(option.label.as_str());
            if clean.is_empty() {
                fallback_label(kind, PageVariantIndex::new(position))
            } else {
                Arc::from(clean)
            }
        })
        .collect();
    let selected = menu
        .options
        .iter()
        .position(|option| option.selection == VariantSelection::Selected)
        .map(PageVariantIndex::new);
    PageVariantMenuView::new(kind, revision, labels, selected)
}

/// Подпись, когда от подписи страницы после очистки ничего не осталось («Audio 2»).
fn fallback_label(kind: PageVariantKind, index: PageVariantIndex) -> Arc<str> {
    Arc::from(format!("{} {}", kind.viewer_title(), index.get() + 1))
}

#[cfg(test)]
#[path = "page_variants_tests.rs"]
mod tests;
