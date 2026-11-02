//! Шов к плееру: «позвонить в дверь» и получить ответ.
//!
//! [`PlayerDoor`] не знает, чем звонить. На Linux звонок — метод `Entry` службы
//! D-Bus браузерного режима ([`EntryClientDoor`] поверх клиента
//! `desktop_integration::BrowserEntryClient`); на других ОС клиент отвечает
//! `UnsupportedPlatform`, а будущий транспорт Windows (named pipe) встанет сюда
//! же без правки остального посредника.
//!
//! Сообщение и ответ — непрозрачный JSON: дверь их не разбирает, это делают
//! [`crate::relay`] и сам плеер.

use desktop_integration::{
    BrowserEntryBus, BrowserEntryCallError, BrowserEntryClient, BrowserEntryClientConfig,
};

/// Почему звонок не дал ответа плеера. Каждому виду — свой отказ браузеру
/// (`crate::relay`), поэтому виды не сливаются в один «не вышло».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorFailure {
    /// Плеер не установлен, не запустился или шины нет.
    PlayerUnavailable,
    /// Плеер запущен, но не ответил в срок.
    PlayerNotResponding,
    /// Плеер завершается и новых звонков не принимает.
    PlayerShuttingDown,
    /// Транспорт отказался везти сообщение (например, по размеру).
    MessageRefusedByTransport,
    /// На этой ОС транспорта к плееру пока нет.
    UnsupportedPlatform,
}

impl DoorFailure {
    /// Имя вида для журнала.
    #[must_use]
    pub const fn log_name(self) -> &'static str {
        match self {
            Self::PlayerUnavailable => "player_unavailable",
            Self::PlayerNotResponding => "player_not_responding",
            Self::PlayerShuttingDown => "player_shutting_down",
            Self::MessageRefusedByTransport => "message_refused_by_transport",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

/// Дверь плеера.
pub trait PlayerDoor {
    /// Передаёт сообщение входа и ждёт ответ плеера.
    ///
    /// Все звонки одного посредника обязаны идти одним «звонящим»: плеер помнит
    /// `hello` по звонящему, и `open_session` без него получит `hello_required`.
    ///
    /// # Errors
    /// Плеера нет, он молчит, завершается или транспорт недоступен.
    fn ring(&mut self, message_json: &str) -> Result<String, DoorFailure>;
}

/// Дверь через клиент D-Bus браузерного режима.
///
/// Соединение открывается при первом звонке, а не при старте: посредник, у
/// которого браузер сразу закрыл порт, шину не трогает вовсе. Дальше все звонки
/// идут одним соединением (одним звонящим). Если соединение сломалось, следующий
/// звонок открывает новое: старое «помнило» `hello` только у плеера, которого,
/// скорее всего, уже нет, и плеер честно ответит `hello_required`.
#[derive(Debug)]
pub struct EntryClientDoor {
    bus: BrowserEntryBus,
    config: BrowserEntryClientConfig,
    client: Option<BrowserEntryClient>,
}

impl EntryClientDoor {
    /// Дверь на сессионной шине пользователя со сроками клиента по умолчанию
    /// (запуск плеера до 15 с, ответ до 8 с — плеер сам отвечает за 5 с).
    #[must_use]
    pub fn session_bus() -> Self {
        Self::new(
            BrowserEntryBus::Session,
            BrowserEntryClientConfig::default(),
        )
    }

    /// Дверь на заданной шине с заданными сроками.
    #[must_use]
    pub fn new(bus: BrowserEntryBus, config: BrowserEntryClientConfig) -> Self {
        Self {
            bus,
            config,
            client: None,
        }
    }
}

impl PlayerDoor for EntryClientDoor {
    fn ring(&mut self, message_json: &str) -> Result<String, DoorFailure> {
        // Соединение берётся «в руки» на время звонка и возвращается, если оно цело.
        let client = match self.client.take() {
            Some(client) => client,
            None => BrowserEntryClient::connect(self.bus.clone(), self.config)
                .map_err(|error| door_failure_for(&error))?,
        };
        let reply = client.call(message_json);
        let keep_connection = reply
            .as_ref()
            .err()
            .is_none_or(|error| !connection_is_broken(error));
        if keep_connection {
            self.client = Some(client);
        }
        reply.map_err(|error| door_failure_for(&error))
    }
}

/// Сломано ли само соединение с шиной (тогда следующий звонок откроет новое).
const fn connection_is_broken(error: &BrowserEntryCallError) -> bool {
    matches!(
        error,
        BrowserEntryCallError::SessionBusUnavailable(_) | BrowserEntryCallError::Transport(_)
    )
}

/// Перевод ошибки клиента D-Bus в вид отказа (договор сессии 02).
#[must_use]
pub fn door_failure_for(error: &BrowserEntryCallError) -> DoorFailure {
    match error {
        // Шины нет, службы нет, запуск упал, имя держит чужой пользователь,
        // соединение сломалось — для браузера всё это «плеер недоступен».
        BrowserEntryCallError::SessionBusUnavailable(_)
        | BrowserEntryCallError::NotInstalled
        | BrowserEntryCallError::ActivationFailed { .. }
        | BrowserEntryCallError::ForeignOwner
        | BrowserEntryCallError::Transport(_) => DoorFailure::PlayerUnavailable,
        BrowserEntryCallError::NotResponding => DoorFailure::PlayerNotResponding,
        BrowserEntryCallError::ShuttingDown => DoorFailure::PlayerShuttingDown,
        BrowserEntryCallError::Rejected { .. } => DoorFailure::MessageRefusedByTransport,
        BrowserEntryCallError::UnsupportedPlatform => DoorFailure::UnsupportedPlatform,
    }
}

#[cfg(test)]
mod tests;
