//! Единый владелец пользовательских уведомлений приложения (UX edge cases, сессия 04).
//!
//! До этого модуля каждое сообщение жило в своём канале: красный текст в центре висел до
//! следующего открытия файла, безобидный отказ перемотки не исчезал никогда, «занято» при
//! повторном Open не показывалось вовсе. Теперь все сообщения проходят через один владелец
//! с явным жизненным циклом:
//!
//! - **фатальная ошибка текущего media** («файл не открылся», воспроизведение умерло) — одна
//!   на экране, в центре; не исчезает сама, закрывается кнопкой × или снимается успешным
//!   открытием / началом нового открытия;
//! - **временное уведомление** (перемотка недоступна, файл ещё открывается) — toast в углу,
//!   исчезает само через [`TRANSIENT_NOTIFICATION_LIFETIME`];
//! - **информационное уведомление** (позиция недоступна, открыто с другого места) — toast,
//!   исчезает само через [`INFO_NOTIFICATION_LIFETIME`].
//!
//! Прогресс («Открываем…») остаётся у shell-состояния `AppState`; здесь только правило, что
//! он важнее старой ошибки (см. [`NotificationCenter::frame`]).
//!
//! Инварианты:
//! - модуль только показывает: он не меняет состояние player-а, очереди и не отдаёт команд;
//! - время не читается из системных часов внутри модуля — `now` передаёт вызывающий кадр,
//!   поэтому тесты управляют временем явно;
//! - одинаковое сообщение не дублируется: повтор продлевает уже видимый toast.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ui::animation::UiMotion;

mod player_feed;

#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;

/// Сколько показывается временное уведомление.
///
/// Решение владельца (2026-10-05): 5 с — достаточно, чтобы прочитать короткую фразу,
/// и не мешает просмотру. Закрыть раньше можно кнопкой ×.
pub(crate) const TRANSIENT_NOTIFICATION_LIFETIME: Duration = Duration::from_secs(5);

/// Сколько показывается информационное уведомление.
///
/// Информация обычно длиннее и не связана с только что нажатой кнопкой, поэтому живёт
/// дольше временного (решение владельца: 8 с).
pub(crate) const INFO_NOTIFICATION_LIFETIME: Duration = Duration::from_secs(8);

/// Сколько toast-ов видно одновременно (решение владельца: стопка до 3).
///
/// Новое уведомление встаёт первым, самое старое вытесняется — экран не заваливается
/// плашками, если события сыплются подряд.
pub(crate) const MAX_VISIBLE_TOASTS: usize = 3;

/// Текст временного уведомления о повторном Open, пока прошлое открытие не закончилось.
pub(crate) const OPEN_STILL_IN_PROGRESS_MESSAGE: &str = "Файл ещё открывается";

/// Стабильный идентификатор уведомления: по нему UI сообщает, какую плашку закрыли.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NotificationId(u64);

/// Вид toast-уведомления: определяет время жизни и тон отрисовки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    /// Реакция на только что сделанное действие (отказ seek, Open занят).
    Transient,
    /// Информация, не требующая действия пользователя.
    Info,
}

impl ToastKind {
    /// Время жизни уведомления этого вида.
    const fn lifetime(self) -> Duration {
        match self {
            Self::Transient => TRANSIENT_NOTIFICATION_LIFETIME,
            Self::Info => INFO_NOTIFICATION_LIFETIME,
        }
    }
}

/// Откуда пришла фатальная ошибка: от этого зависит, кто вправе её снять.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaFailureOrigin {
    /// Открытие/установка media не удалась (shell-слой, до или вместо player-а).
    MediaOpen,
    /// Player перешёл в `Failed`; ошибка снимается, когда player выходит из `Failed`.
    PlayerFailed,
}

/// Исход закрытия уведомления пользователем.
///
/// Отдельный вариант для «уже нет» нужен, потому что × мог быть нажат в кадре, когда
/// уведомление уже истекло или было заменено, — это не ошибка, а штатный no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotificationDismissOutcome {
    /// Уведомление найдено и убрано.
    Dismissed,
    /// Такого уведомления уже нет; состояние не менялось.
    AlreadyGone,
}

/// Идёт ли сейчас открытие media, которое нужно показать в центре вместо ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenProgress<'a> {
    /// Открытие не идёт.
    Idle,
    /// Открытие идёт; текст прогресса для пользователя.
    InProgress(&'a str),
}

/// Видимый toast в проекции текущего кадра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToastView {
    /// Идентификатор для кнопки ×.
    pub(crate) id: NotificationId,
    /// Вид (тон отрисовки).
    pub(crate) kind: ToastKind,
    /// Текст для пользователя.
    pub(crate) message: Arc<str>,
    /// Сколько toast уже на экране — UI по нему плавно проявляет новую плашку.
    pub(crate) age: Duration,
}

/// Фатальная ошибка в проекции текущего кадра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaFailureView {
    /// Идентификатор для кнопки ×.
    pub(crate) id: NotificationId,
    /// Текст для пользователя.
    pub(crate) message: Arc<str>,
}

/// Что показывать в центре экрана.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CenterNotice {
    /// Идёт открытие media («Открываем …»).
    Progress(Arc<str>),
    /// Фатальная ошибка текущего media с кнопкой ×.
    MediaFailure(MediaFailureView),
}

/// Неизменяемая проекция уведомлений для одного кадра UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationsFrame {
    /// Сообщение в центре, если есть.
    pub(crate) center: Option<CenterNotice>,
    /// Видимые toast-ы, самый новый первым.
    pub(crate) toasts: Vec<ToastView>,
    /// Политика движения: при reduced motion toast появляется без анимации.
    pub(crate) motion: UiMotion,
}

/// Toast внутри владельца.
#[derive(Debug, Clone)]
struct ToastEntry {
    id: NotificationId,
    kind: ToastKind,
    message: Arc<str>,
    /// Когда плашка впервые появилась (повтор не перезапускает появление).
    shown_at: Instant,
    /// Когда плашка исчезнет сама.
    expires_at: Instant,
}

/// Фатальная ошибка внутри владельца.
#[derive(Debug, Clone)]
struct MediaFailureEntry {
    id: NotificationId,
    origin: MediaFailureOrigin,
    message: Arc<str>,
}

/// Что владелец последний раз видел в состоянии player-а (для `player_feed`).
///
/// Нужен, чтобы закрытая пользователем ошибка `Failed` не всплывала заново на каждом
/// кадре: новая фатальная ошибка показывается только при переходе, а не при повторе.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum ObservedPlayerFailure {
    /// Player не в `Failed`.
    #[default]
    Healthy,
    /// Player в `Failed` с этим текстом ошибки (или без текста).
    Failed { message: Option<Arc<str>> },
}

/// Единственный владелец пользовательских уведомлений.
#[derive(Debug, Default)]
pub(crate) struct NotificationCenter {
    /// Видимые toast-ы, самый новый первым; длина не больше [`MAX_VISIBLE_TOASTS`].
    toasts: VecDeque<ToastEntry>,
    /// Единственная фатальная ошибка текущего media.
    media_failure: Option<MediaFailureEntry>,
    /// Последнее увиденное состояние отказа player-а.
    observed_player_failure: ObservedPlayerFailure,
    /// Счётчик для выдачи уникальных [`NotificationId`].
    next_id: u64,
}

impl NotificationCenter {
    /// Создаёт владельца с уже известной ошибкой открытия (например, ошибка CLI-аргумента).
    pub(crate) fn with_media_open_failure(message: impl Into<Arc<str>>) -> Self {
        let mut center = Self::default();
        center.show_media_failure(message, MediaFailureOrigin::MediaOpen);
        center
    }

    /// Показывает временное уведомление (исчезнет само через 5 с).
    pub(crate) fn notify_transient(
        &mut self,
        message: impl Into<Arc<str>>,
        now: Instant,
    ) -> NotificationId {
        self.push_toast(ToastKind::Transient, message.into(), now)
    }

    /// Показывает информационное уведомление (исчезнет само через 8 с).
    pub(crate) fn notify_info(
        &mut self,
        message: impl Into<Arc<str>>,
        now: Instant,
    ) -> NotificationId {
        self.push_toast(ToastKind::Info, message.into(), now)
    }

    /// Показывает фатальную ошибку текущего media, заменяя предыдущую.
    ///
    /// Фатальная ошибка одна: новая всегда актуальнее старой, стопка красных
    /// сообщений о прошлых файлах пользователю не нужна.
    pub(crate) fn show_media_failure(
        &mut self,
        message: impl Into<Arc<str>>,
        origin: MediaFailureOrigin,
    ) -> NotificationId {
        let id = self.allocate_id();
        self.media_failure = Some(MediaFailureEntry {
            id,
            origin,
            message: message.into(),
        });
        id
    }

    /// Снимает фатальную ошибку: media успешно открылось или началось новое открытие.
    ///
    /// Toast-ы не трогаются: «перемотка недоступна» о прошлом файле доживает свои секунды.
    pub(crate) fn resolve_media_failure(&mut self) {
        self.media_failure = None;
    }

    /// Закрывает уведомление по кнопке × (toast или фатальную ошибку).
    pub(crate) fn dismiss(&mut self, id: NotificationId) -> NotificationDismissOutcome {
        if self
            .media_failure
            .as_ref()
            .is_some_and(|failure| failure.id == id)
        {
            self.media_failure = None;
            return NotificationDismissOutcome::Dismissed;
        }
        let toast_count_before = self.toasts.len();
        self.toasts.retain(|toast| toast.id != id);
        if self.toasts.len() < toast_count_before {
            NotificationDismissOutcome::Dismissed
        } else {
            NotificationDismissOutcome::AlreadyGone
        }
    }

    /// Строит проекцию для кадра UI, предварительно убрав истёкшие toast-ы.
    ///
    /// Правило центра: идущее открытие важнее старой ошибки. Новая ошибка открытия
    /// сама снимает прогресс у shell-слоя, поэтому прогресс никогда не прячет свежую
    /// ошибку, а старая ошибка больше не прячет «Открываем …».
    pub(crate) fn frame(
        &mut self,
        progress: OpenProgress<'_>,
        motion: UiMotion,
        now: Instant,
    ) -> NotificationsFrame {
        self.expire(now);
        let center = match progress {
            OpenProgress::InProgress(message) => Some(CenterNotice::Progress(Arc::from(message))),
            OpenProgress::Idle => self.media_failure.as_ref().map(|failure| {
                CenterNotice::MediaFailure(MediaFailureView {
                    id: failure.id,
                    message: Arc::clone(&failure.message),
                })
            }),
        };
        let toasts = self
            .toasts
            .iter()
            .map(|toast| ToastView {
                id: toast.id,
                kind: toast.kind,
                message: Arc::clone(&toast.message),
                age: now.saturating_duration_since(toast.shown_at),
            })
            .collect();
        NotificationsFrame {
            center,
            toasts,
            motion,
        }
    }

    /// Ближайший момент, когда нужно перерисовать окно, чтобы toast исчез вовремя.
    ///
    /// Нужен на паузе: без воспроизведения кадры не рисуются, и без этого будильника
    /// toast висел бы до первого движения мыши.
    pub(crate) fn next_wake_deadline(&self) -> Option<Instant> {
        self.toasts.iter().map(|toast| toast.expires_at).min()
    }

    /// Добавляет toast или продлевает такой же уже видимый.
    fn push_toast(&mut self, kind: ToastKind, message: Arc<str>, now: Instant) -> NotificationId {
        let expires_at = now + kind.lifetime();
        // Повтор того же сообщения (пять нажатий ← подряд) — одна плашка с новым сроком.
        // Id и момент появления сохраняются, поэтому плашка не мигает повторным появлением.
        if let Some(position) = self
            .toasts
            .iter()
            .position(|toast| toast.kind == kind && toast.message == message)
            && let Some(mut existing) = self.toasts.remove(position)
        {
            existing.expires_at = expires_at;
            let id = existing.id;
            self.toasts.push_front(existing);
            return id;
        }
        let id = self.allocate_id();
        self.toasts.push_front(ToastEntry {
            id,
            kind,
            message,
            shown_at: now,
            expires_at,
        });
        // Самые старые плашки вытесняются, если их стало больше лимита.
        self.toasts.truncate(MAX_VISIBLE_TOASTS);
        id
    }

    /// Убирает toast-ы, срок которых наступил.
    fn expire(&mut self, now: Instant) {
        self.toasts.retain(|toast| toast.expires_at > now);
    }

    /// Выдаёт новый уникальный идентификатор.
    fn allocate_id(&mut self) -> NotificationId {
        self.next_id += 1;
        NotificationId(self.next_id)
    }
}
