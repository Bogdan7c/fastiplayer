//! Intent-API владельца для правки внешнего вида субтитров на видео.
//!
//! # Кто чем владеет
//! - Сессия ([`AppearanceEditSession`]) - в `SubtitleOwner`; её данные и правила изменения -
//!   в `appearance_edit`. Здесь только связка с остальным владельцем.
//! - Какой стиль показывать СЕЙЧАС ([`SubtitleOwner::displayed_style_offsets`] и
//!   `sync_presenter_appearance`): черновик, если сессия открыта; иначе - стиль, ждущий записи
//!   в конфиг; иначе закоммиченный. Так между «Готово» и подтверждением записи кадр не
//!   дёргается обратно на старое место.
//! - Запись в конфиг НЕ здесь: владелец кладёт стиль в очередь
//!   ([`SubtitleOwner::take_style_save_request`]), транзакцию выполняет `AppState`/`SettingsRuntime`.
//!
//! # Инварианты
//! - Позиция меняет только сдвиг слоя на GPU: перерастеризации нет. Размер, цвета и оформление
//!   дают ровно одну перерастеризацию на изменение (`replace_appearance` идемпотентен).
//! - Конфиг, подтверждённый во время сессии, черновик не перезаписывает
//!   ([`SubtitleOwner::apply_config`]); «Отмена» возвращает ТЕКУЩИЙ закоммиченный стиль.
//! - Видео сменилось на другое (следующий в плейлисте) - сессия живёт; видео исчезло совсем
//!   ([`SubtitleOwner::clear_media`]) - сессия завершается сохранением.

use super::SubtitleOwner;
use super::appearance_edit::{
    AppearanceEdit, AppearanceEditBegin, AppearanceEditChange, AppearanceEditFinish,
    AppearanceEditOutcome, AppearanceEditSession, AppearanceEditView,
};
use super::user_style::UserSubtitleStyle;

impl SubtitleOwner {
    /// Открывает сессию правки. Не ставит воспроизведение на паузу (решение владельца 1).
    ///
    /// Если растеризатора ещё нет (дорожка никогда не загружалась), заказывает построение
    /// библиотеки шрифтов в фоне: образец появится, когда она будет готова.
    pub(in crate::state) fn begin_appearance_edit(&mut self) -> AppearanceEditBegin {
        if self.appearance_edit.is_some() {
            return AppearanceEditBegin::AlreadyActive;
        }
        if self.media.is_none() {
            return AppearanceEditBegin::NoMedia;
        }
        self.appearance_edit = Some(AppearanceEditSession::begin(&self.settings.style));
        // Старт без правок ничего не меняет на экране: стиль черновика равен закоммиченному.
        self.request_rasterizer_in_background();
        AppearanceEditBegin::Started
    }

    /// Применяет изменение к черновику вживую.
    pub(in crate::state) fn apply_appearance_edit(
        &mut self,
        edit: AppearanceEdit,
    ) -> AppearanceEditChange {
        let Some(session) = self.appearance_edit.as_mut() else {
            return AppearanceEditChange::NotEditing;
        };
        let change = session.apply(edit);
        if change == AppearanceEditChange::Changed {
            self.sync_presenter_appearance();
        }
        change
    }

    /// Пользователь схватил рамку: дальнейшие `MoveBy` считаются от места, где слой ВИДЕН.
    ///
    /// Слоя ещё не было на экране (образец не готов, нет кадра) - позиция не трогается.
    /// Только позиция: перерастеризации нет, меняется лишь сдвиг слоя на GPU.
    pub(in crate::state) fn start_appearance_layer_move(&mut self) -> AppearanceEditChange {
        let Some(session) = self.appearance_edit.as_mut() else {
            return AppearanceEditChange::NotEditing;
        };
        let (Some(offset), Some(picture)) =
            (self.last_placed_layer_offset, self.last_placed_picture_rect)
        else {
            return AppearanceEditChange::Unchanged;
        };
        // Размеры картинки округляются так же, как при размещении (целые пиксели холста).
        let picture_width = picture.width.round();
        let picture_height = picture.height.round();
        if picture_width < 1.0 || picture_height < 1.0 {
            return AppearanceEditChange::Unchanged;
        }
        // Экранная ось Y смотрит вниз, а доля позиции - вверх.
        let change = session
            .start_move_from_shown_offset(offset.x / picture_width, -offset.y / picture_height);
        if change == AppearanceEditChange::Changed {
            self.sync_presenter_appearance();
        }
        change
    }

    /// «Сброс»: черновик становится стилем по умолчанию (флаг ASS остаётся); сессия открыта.
    pub(in crate::state) fn reset_appearance_draft(&mut self) -> AppearanceEditChange {
        let Some(session) = self.appearance_edit.as_mut() else {
            return AppearanceEditChange::NotEditing;
        };
        let change = session.reset_to_defaults();
        if change == AppearanceEditChange::Changed {
            self.sync_presenter_appearance();
        }
        change
    }

    /// Завершает сессию.
    ///
    /// - `Cancel`: показ возвращается к закоммиченному стилю, в конфиг ничего не пишется;
    /// - `Save`: стиль ставится в очередь записи, показ остаётся на нём до подтверждения.
    pub(in crate::state) fn finish_appearance_edit(
        &mut self,
        finish: AppearanceEditFinish,
    ) -> AppearanceEditOutcome {
        let Some(session) = self.appearance_edit.take() else {
            return AppearanceEditOutcome::NotEditing;
        };
        match finish {
            AppearanceEditFinish::Cancel => {
                // `before` следует за закоммиченным конфигом, так что это ТЕКУЩИЙ закоммиченный стиль.
                self.presenter
                    .replace_appearance(session.before().to_appearance());
                AppearanceEditOutcome::Cancelled
            }
            AppearanceEditFinish::Save => {
                self.style_save_request = Some(session.draft().clone());
                self.sync_presenter_appearance();
                AppearanceEditOutcome::SaveRequested
            }
        }
    }

    /// Запись не удалась: показывается закоммиченный стиль, ожидавший запись стиль забыт.
    pub(in crate::state) fn discard_unsaved_style(&mut self) {
        self.style_save_request = None;
        self.sync_presenter_appearance();
    }

    /// Стиль, ждущий записи в конфиг. Отдаётся один раз; показ при этом не меняется
    /// (подтверждённый конфиг придёт через `apply_config`).
    pub(in crate::state) fn take_style_save_request(&mut self) -> Option<UserSubtitleStyle> {
        self.style_save_request.take()
    }

    /// Снимок для UI правки или `None`, если сессии нет.
    pub(in crate::state) fn appearance_edit_view(&self) -> Option<AppearanceEditView> {
        let session = self.appearance_edit.as_ref()?;
        Some(AppearanceEditView {
            draft: session.draft().clone(),
            layer_rect: self.last_placed_layer_rect,
            picture_rect: self.last_placed_picture_rect,
        })
    }

    /// Есть ли установленное видео, на котором можно начать правку (иначе `begin_appearance_edit`
    /// вернёт `NoMedia`). Нужен окну настроек, чтобы выключить кнопку входа.
    pub(in crate::state) fn has_installed_media(&self) -> bool {
        self.media.is_some()
    }

    /// Идёт ли сессия правки.
    pub(in crate::state) fn is_editing_appearance(&self) -> bool {
        self.appearance_edit.is_some()
    }

    /// Видео исчезло: идущая сессия завершается сохранением (неявный выход, решение владельца 6).
    pub(super) fn end_appearance_edit_for_cleared_media(&mut self) {
        let _outcome = self.finish_appearance_edit(AppearanceEditFinish::Save);
    }

    /// Сдвиг слоя (доли картинки) для текущего показываемого стиля: `(горизонталь, вертикаль)`.
    ///
    /// В сессии берётся дробная позиция черновика (плавное перетаскивание); иначе целые
    /// проценты ожидающего записи либо закоммиченного стиля.
    pub(super) fn displayed_style_offsets(&self) -> (f32, f32) {
        if let Some(session) = &self.appearance_edit {
            return (
                session.horizontal_offset_fraction(),
                session.vertical_offset_fraction(),
            );
        }
        let style = self.displayed_style();
        (
            style.horizontal_offset_fraction(),
            style.vertical_offset_fraction(),
        )
    }

    /// Показываемый стиль вне дробной точности: черновик, ожидающий запись или закоммиченный.
    fn displayed_style(&self) -> &UserSubtitleStyle {
        if let Some(session) = &self.appearance_edit {
            return session.draft();
        }
        self.style_save_request
            .as_ref()
            .unwrap_or(&self.settings.style)
    }

    /// Отдаёт презентатору внешний вид показываемого стиля (no-op, если он не менялся).
    pub(super) fn sync_presenter_appearance(&mut self) {
        let appearance = self.displayed_style().to_appearance();
        self.presenter.replace_appearance(appearance);
    }

    /// Подтверждённый конфиг изменил стиль: обновляет `before` открытой сессии, не трогая
    /// черновик пользователя. Без сессии ничего не делает.
    pub(super) fn follow_committed_style_in_session(&mut self) {
        if let Some(session) = self.appearance_edit.as_mut() {
            session.follow_committed_style(&self.settings.style);
        }
    }
}
