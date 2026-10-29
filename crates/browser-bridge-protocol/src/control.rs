//! Участок «вход»: экстеншен ↔ посредник ↔ плеер (решение владельца 85).
//!
//! Посредник native messaging — «звонок в дверь». По этому участку идут
//! только короткие разговоры в начале:
//! 1. `hello` — версии и возможности сторон;
//! 2. `open_session` — открыть сеанс для видео во вкладке;
//! 3. ответ плеера: порт и одноразовый пропуск WebSocket (`session_granted`)
//!    или отказ с причиной (`refused`).
//!
//! Всё живое (байты, часы, команды, события) идёт дальше напрямую по
//! WebSocket — см. `crate::session`. Транспорт (native messaging, D-Bus) здесь
//! не описывается: это забота сессий 02 и 05, и у Windows он будет другим.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::codec::ProtocolMessage;
use crate::error::{MessageErrorKind, ValueViolation};
use crate::limits::MAX_CAPABILITY_TOKENS;
use crate::text::{
    CapabilityToken, PageTitle, SessionKey, SessionToken, SiteDomain, SoftwareVersion,
};
use crate::time::{MediaDuration, MediaTime};
use crate::version::{BridgeSoftware, ProtocolVersion};

/// Номер запроса на участке «вход»: ответ несёт тот же номер.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ControlRequestId(pub u32);

/// Порт WebSocket плеера на `127.0.0.1`.
///
/// Адреса в протоколе нет: соединение всегда на loopback, и подменить узел
/// нельзя даже испорченным ответом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct LoopbackPort(u16);

impl LoopbackPort {
    /// Порт из числа.
    ///
    /// # Errors
    /// Ноль — «любой порт» у ОС, а не настоящий адрес.
    pub const fn new(port: u16) -> Result<Self, ValueViolation> {
        if port == 0 {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(port))
    }

    /// Номер порта.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for LoopbackPort {
    type Error = ValueViolation;

    fn try_from(port: u16) -> Result<Self, Self::Error> {
        Self::new(port)
    }
}

impl From<LoopbackPort> for u16 {
    fn from(port: LoopbackPort) -> Self {
        port.0
    }
}

/// Приветствие стороны. **Форма не меняется никогда** (`crate::version`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Версия протокола, которую сторона реализует.
    pub protocol: ProtocolVersion,
    /// Версия программы — только для диагностики.
    pub software_version: SoftwareVersion,
    /// Метки возможностей (`crate::capability`).
    pub capabilities: Vec<CapabilityToken>,
}

impl Hello {
    /// Список меток не длиннее лимита и без повторов.
    fn validate(&self) -> Result<(), ValueViolation> {
        if self.capabilities.len() > MAX_CAPABILITY_TOKENS {
            return Err(ValueViolation::TooManyItems);
        }
        let unique: BTreeSet<&CapabilityToken> = self.capabilities.iter().collect();
        if unique.len() != self.capabilities.len() {
            return Err(ValueViolation::DuplicateId);
        }
        Ok(())
    }
}

/// Что видно о странице: домен и заголовок (инвариант 2 — без адреса и кук).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageIdentity {
    /// Заголовок вкладки.
    pub title: PageTitle,
    /// Домен страницы.
    pub domain: SiteDomain,
}

/// Как человек хочет смотреть.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionIntent {
    /// Обычное окно браузерного режима (режимы 1 и 2).
    Window,
    /// Мини-окно «картинка в картинке».
    PictureInPicture,
    /// Режим 3: плеер на месте плеера страницы («Театр» или «Наклейка»).
    InPage,
}

/// Где сейчас позиция плеера страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PositionHint {
    /// Позиция неизвестна (плеер страницы ещё не стартовал).
    Unknown {},
    /// Плеер страницы стоит на этой секунде («по клику» посреди просмотра).
    At {
        /// Текущая позиция.
        position_us: MediaTime,
    },
}

/// Подсказки для окна до прихода первых байтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackHints {
    /// Длина ролика, как её знает плеер страницы.
    pub duration: MediaDuration,
    /// Позиция плеера страницы.
    pub position: PositionHint,
}

/// Запрос «открой сеанс для этого видео».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenSessionRequest {
    /// Номер запроса.
    pub request_id: ControlRequestId,
    /// Ключ сеанса: повтор с тем же ключом — тот же сеанс.
    pub session_key: SessionKey,
    /// Подпись окна.
    pub page: PageIdentity,
    /// Обычное окно, PiP или режим 3.
    pub intent: SessionIntent,
    /// Подсказки до первых байтов.
    pub hints: PlaybackHints,
}

/// Ответ плеера на `hello` экстеншена.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloAccepted {
    /// Общая версия: на ней обе стороны говорят дальше.
    pub agreed_protocol: ProtocolVersion,
    /// Приветствие плеера: его версия и возможности (дешёвые кодеки, решение 43).
    pub player: Hello,
}

/// Чем стал запрос `open_session` (инвариант 6: повтор не создаёт второго окна).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantOutcome {
    /// Сеансов не было — открыт новый.
    Started,
    /// Сеанс с этим ключом уже идёт — выдан пропуск в него же.
    ReusedExisting,
    /// Шёл сеанс другой вкладки — он закрыт, открыт новый (решение 40).
    ReplacedPrevious,
}

/// Пропуск в сеанс.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionGrant {
    /// Номер запроса, на который это ответ.
    pub request_id: ControlRequestId,
    /// Ключ сеанса из запроса.
    pub session_key: SessionKey,
    /// Порт WebSocket на `127.0.0.1`.
    pub port: LoopbackPort,
    /// Одноразовый пропуск: первым сообщением сеанса.
    pub token: SessionToken,
    /// Новый сеанс, тот же или замена.
    pub outcome: GrantOutcome,
}

/// На что отказ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplyTarget {
    /// На `hello`.
    Hello {},
    /// На `open_session` с этим номером.
    OpenSession {
        /// Номер запроса.
        request_id: ControlRequestId,
    },
    /// На сообщение, которое не удалось разобрать (номера не узнать).
    UnparsedMessage {},
}

/// Причина отказа на участке «вход».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RefusalReason {
    /// Плеер не установлен или не запускается (посредник не достучался).
    PlayerUnavailable {},
    /// Плеер запущен, но не ответил вовремя.
    PlayerNotResponding {},
    /// Плеер завершается и новых сеансов не берёт.
    PlayerShuttingDown {},
    /// Защищённое видео (DRM): мост его не трогает (инвариант 4).
    ProtectedContent {},
    /// Версии несовместимы: кого обновить.
    IncompatibleVersion {
        /// Отставшая сторона.
        update: BridgeSoftware,
    },
    /// Кодек плееру не по силам.
    UnsupportedCodec {},
    /// Сообщение отвергнуто разбором — точная причина.
    MalformedMessage {
        /// Вид причины.
        error: MessageErrorKind,
    },
    /// До `hello` пришло другое сообщение.
    HelloRequired {},
}

/// Отказ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRefusal {
    /// На что отказ.
    pub in_reply_to: ReplyTarget,
    /// Почему.
    pub reason: RefusalReason,
}

/// Сообщения участка «вход» от экстеншена к плееру.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionControlMessage {
    /// Приветствие экстеншена.
    Hello(Hello),
    /// Открыть сеанс.
    OpenSession(OpenSessionRequest),
}

impl ProtocolMessage for ExtensionControlMessage {
    const KNOWN_TYPES: &'static [&'static str] = &["hello", "open_session"];

    fn validate(&self) -> Result<(), ValueViolation> {
        match self {
            Self::Hello(hello) => hello.validate(),
            Self::OpenSession(_) => Ok(()),
        }
    }
}

/// Сообщения участка «вход» от плеера (или посредника) к экстеншену.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlayerControlMessage {
    /// Приветствие принято.
    HelloAccepted(HelloAccepted),
    /// Пропуск в сеанс.
    SessionGranted(SessionGrant),
    /// Отказ. Его же шлёт посредник, если плеер недоступен.
    Refused(ControlRefusal),
}

impl ProtocolMessage for PlayerControlMessage {
    const KNOWN_TYPES: &'static [&'static str] = &["hello_accepted", "session_granted", "refused"];

    fn validate(&self) -> Result<(), ValueViolation> {
        match self {
            Self::HelloAccepted(accepted) => accepted.player.validate(),
            Self::SessionGranted(_) | Self::Refused(_) => Ok(()),
        }
    }
}
