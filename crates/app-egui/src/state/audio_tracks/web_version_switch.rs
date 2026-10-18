//! Выбор web-версии звука из меню «♪» и клавишей `B`: запрос → same-item component switch.
//!
//! # Поток
//! 1. Меню/`B` публикуют [`ComponentVariantSelectionAction`] (axis = Audio). Во время egui
//!    pass-а у `AppState` нет `PlaylistRuntime`/`Renderer`, поэтому выбор сохраняется как
//!    запрос (последний выигрывает) — [`AppState::request_web_audio_version_by_user`].
//! 2. После egui pass-а `frame_prepare` вызывает [`AppState::apply_requested_web_audio_version`]:
//!    запрос уходит в тот же same-item component switch, что строка URL sidebar
//!    (`apply_audio_track_menu_component_action`): позиция и Play/Pause сохраняются, старое
//!    воспроизведение живёт до успешной установки, каталог сам проверяет generation-fence-ы.
//! 3. Старт → toast «Audio: …»; отказ старта или терминальный отказ → toast с причиной,
//!    текущий звук остаётся (switch до barrier-а ничего не меняет).
//!
//! # Память выбора
//! Выбор web-версии НЕ пишется в `track-memory.json`: идентичность версии — semantic key
//! каталога, а восстановление при открытии потребовало бы component reopen на старте
//! (решение владельца, см. memory `app-egui/web-audio-versions-menu`).

use player_core::PlayerSnapshot;
use render_wgpu_shell::Renderer;

use media_source_open::web_media_open::audio_languages::WebAudioLanguageAction;

use super::labels::audio_selected_toast;
use super::menu_view::{AudioTrackMenuView, WebAudioMenuSource};
use super::web_versions::{WebAudioVersionEntry, web_audio_versions};
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::AppState;
use crate::state::notifications::TransientReplaceKey;
use crate::state::same_item_candidate_switch::{SameItemSwitchError, UrlSidebarActionApplyOutcome};
use crate::web_media_stream_model::UrlSidebarSafeError;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantSelectionAction,
};

/// Toast: переключение запущено, а подписи версии в меню уже нет (каталог обновился).
pub(super) const VERSION_SWITCH_STARTED_MESSAGE: &str = "Switching audio version";
/// Toast: переключение версии звука не удалось (текущий звук остаётся).
pub(super) const VERSION_SWITCH_FAILED_MESSAGE: &str = "Could not switch audio version";
/// Toast: уже идёт другое переключение того же видео (версия, качество).
pub(super) const VERSION_SWITCH_BUSY_MESSAGE: &str = "Another switch is already in progress";
/// Toast: список версий устарел (источник переоткрылся), выбор нужно повторить.
pub(super) const VERSION_SWITCH_STALE_MESSAGE: &str =
    "The audio version list is outdated; select the version again";
/// Toast: переключение отменено (например, Stop или сворачивание).
pub(super) const VERSION_SWITCH_CANCELED_MESSAGE: &str = "Audio version switch canceled";

/// Отложенный выбор звука web-источника: версия (HLS/DASH) либо язык (yt-dlp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum WebAudioSwitchRequest {
    /// Другая версия звука того же видео (component switch).
    Version(ComponentVariantSelectionAction),
    /// Другой язык звука (дубляж) того же ролика.
    Language(WebAudioLanguageAction),
    /// Автоматическое восстановление запомненного языка после открытия видео; без toast-ов.
    /// `attempts_left` ограничивает ожидание завершения открытия (`web_audio_language_memory`).
    RestoreRemembered {
        action: WebAudioLanguageAction,
        attempts_left: u16,
    },
}

impl AppState {
    /// Единый снимок меню «♪» на кадр: встроенные дорожки + web-звук активного источника.
    #[must_use]
    pub(in crate::state) fn audio_track_menu_view(
        &self,
        player_snapshot: &PlayerSnapshot,
    ) -> AudioTrackMenuView {
        AudioTrackMenuView::compose(player_snapshot, self.web_audio_menu_source())
    }

    /// Какой выбор звука публикует активный источник: версии HLS/DASH, иначе языки yt-dlp.
    ///
    /// Одновременно источник показывает только один вид выбора, чтобы в меню не было двух
    /// отметок «играет» и круг клавиши `B` оставался однозначным.
    fn web_audio_menu_source(&self) -> Option<WebAudioMenuSource> {
        if let Some(versions) = self.web_audio_version_entries() {
            return Some(WebAudioMenuSource::Versions(versions));
        }
        self.web_audio_language_entries()
            .map(WebAudioMenuSource::Languages)
    }

    /// Web-версии звука активного источника; `None` — источник не web или выбора нет.
    fn web_audio_version_entries(&self) -> Option<Vec<WebAudioVersionEntry>> {
        let configuration = self
            .active_media_source
            .as_ref()?
            .web_media_stream_configuration()?;
        web_audio_versions(
            configuration.generation(),
            &configuration.component_variant_projection(),
        )
    }

    /// Запоминает выбор web-версии до конца кадра; повторный выбор заменяет прежний.
    pub(super) fn request_web_audio_version_by_user(
        &mut self,
        version: ComponentVariantSelectionAction,
    ) {
        self.requested_web_audio_switch = Some(WebAudioSwitchRequest::Version(version));
        self.mark_pending_worker_redraw();
    }

    /// Запоминает выбор языка звука до конца кадра; последний выбор побеждает.
    pub(super) fn request_web_audio_language_by_user(&mut self, language: WebAudioLanguageAction) {
        self.requested_web_audio_switch = Some(WebAudioSwitchRequest::Language(language));
        self.mark_pending_worker_redraw();
    }

    /// Запускает отложенный выбор web-звука; `true` — запрос был (нужна перерисовка).
    pub(crate) fn apply_requested_web_audio_version(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> bool {
        let version = match self.requested_web_audio_switch.take() {
            Some(WebAudioSwitchRequest::Version(version)) => version,
            Some(WebAudioSwitchRequest::Language(language)) => {
                self.apply_requested_web_audio_language(language, playlist_runtime, renderer);
                return true;
            }
            Some(WebAudioSwitchRequest::RestoreRemembered {
                action,
                attempts_left,
            }) => {
                self.apply_requested_language_restore(
                    action,
                    attempts_left,
                    playlist_runtime,
                    renderer,
                );
                return true;
            }
            None => return false,
        };
        // Подпись берётся до запуска: после Installed каталог получит новое поколение.
        let label = self.web_audio_version_entries().and_then(|entries| {
            entries
                .into_iter()
                .find(|entry| entry.action == version)
                .map(|entry| entry.label)
        });
        let outcome =
            self.apply_audio_track_menu_component_action(version, playlist_runtime, renderer);
        if let Err(error) = &outcome {
            tracing::warn!(%error, "Audio version switch was rejected before start");
        }
        if let Some(notice) = start_notice(&outcome, label.as_deref()) {
            self.notify_audio_version(notice);
        }
        true
    }

    /// Терминальный отказ switch-а, запущенного из меню «♪»: toast с безопасной причиной.
    pub(in crate::state) fn report_web_audio_version_switch_failed(
        &mut self,
        error: UrlSidebarSafeError,
    ) {
        self.notify_audio_version(terminal_failure_notice(error).to_owned());
    }

    /// Одна заменяемая плашка «Audio: …» для всех сообщений о выборе звука.
    pub(super) fn notify_audio_version(&mut self, message: String) {
        self.notifications.notify_transient_replacing(
            TransientReplaceKey::AudioTrack,
            message,
            std::time::Instant::now(),
        );
        self.mark_pending_worker_redraw();
    }
}

/// Что сказать пользователю после попытки старта; `None` — версия уже активна, молчим.
pub(super) fn start_notice(
    outcome: &Result<UrlSidebarActionApplyOutcome, SameItemSwitchError>,
    label: Option<&str>,
) -> Option<String> {
    match outcome {
        Ok(UrlSidebarActionApplyOutcome::Started) => Some(label.map_or_else(
            || VERSION_SWITCH_STARTED_MESSAGE.to_owned(),
            audio_selected_toast,
        )),
        Ok(UrlSidebarActionApplyOutcome::NoChange) => None,
        Err(SameItemSwitchError::Busy) => Some(VERSION_SWITCH_BUSY_MESSAGE.to_owned()),
        // Каталог не смог собрать пару (например, версия доступна только с другим качеством
        // видео) — это отказ, а не устаревший список.
        Err(SameItemSwitchError::ComponentAction(
            ComponentVariantActionError::ReplacementFailed(_),
        )) => Some(VERSION_SWITCH_FAILED_MESSAGE.to_owned()),
        Err(SameItemSwitchError::Stale | SameItemSwitchError::ComponentAction(_)) => {
            Some(VERSION_SWITCH_STALE_MESSAGE.to_owned())
        }
        Err(_) => Some(VERSION_SWITCH_FAILED_MESSAGE.to_owned()),
    }
}

/// Текст терминального отказа по безопасной категории sidebar-а (без сырой цепочки ошибок).
pub(super) fn terminal_failure_notice(error: UrlSidebarSafeError) -> &'static str {
    match error {
        UrlSidebarSafeError::SameItemSwitchBusy => VERSION_SWITCH_BUSY_MESSAGE,
        UrlSidebarSafeError::SameItemSwitchStale => VERSION_SWITCH_STALE_MESSAGE,
        UrlSidebarSafeError::SameItemSwitchCancelled => VERSION_SWITCH_CANCELED_MESSAGE,
        UrlSidebarSafeError::SourceUnavailable => VERSION_SWITCH_FAILED_MESSAGE,
    }
}
