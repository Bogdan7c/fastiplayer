//! «Чёрный ящик» браузерного режима: отладочная запись всего, что сеанс получил от
//! экстеншена (сессия 09b2).
//!
//! ВНИМАНИЕ, ПРИВАТНОСТЬ (инвариант 7). Браузерный режим ничего не пишет на диск о
//! просмотре. Единственное исключение — этот модуль, и только когда владелец ЯВНО задал
//! переменную окружения [`DEBUG_RECORD_VARIABLE`] (путь к каталогу): тогда на диск
//! попадают сырые байты сайта (видео) и сообщения страницы. Это отладочный инструмент
//! для разбора чужих сбоев; в продукте переменной нет и модуль не делает ничего:
//! отвод провода не ставится вовсе, цена нулевая. Записи не попадают в git и отчёты.
//!
//! Что пишется (формат — как у фикстур протокола, см. [`session_files`]):
//! - каждое сообщение WebSocket от страницы подключённого сеанса — текст и двоичные
//!   рамки как есть, до разбора и сторожа поколений, с временем прихода `at_ms`;
//! - `outgoing.jsonl` — что плеер отправил странице.
//!
//! Повтор записи: `pushed-media-source/tests/support/recorded.rs` (в источник) и
//! тесты процесса `tests/browser_session_process/replay_recording.rs` (в настоящий процесс).
//!
//! Ошибка записи (нет места, каталог только для чтения) — одно предупреждение в журнал,
//! запись выключается до конца процесса, плеер играет как обычно.

mod session_files;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use browser_bridge_session::{SessionInstance, SessionRef, SessionWireTap};
use tracing::{info, warn};

use self::session_files::{RecordingError, SessionFiles};

/// Переменная окружения: каталог, куда писать запись. Нет переменной — записи нет.
pub(crate) const DEBUG_RECORD_VARIABLE: &str = "FASTIPLAYER_DEBUG_RECORD_BROWSER_STREAM";

/// Метка каталога сеанса: `<каталог записи>/session-<n>/`.
const SESSION_DIRECTORY_LABEL: &str = "session";

/// Единственная строка предупреждения об остановке записи (по ней тесты считают
/// «предупреждение одно»).
pub(crate) const RECORDING_STOPPED_MESSAGE: &str = "Debug stream recording stopped";

/// Отвод провода, если запись включена переменной окружения; иначе `None`.
pub(super) fn wire_tap_from_environment() -> Option<Arc<dyn SessionWireTap>> {
    let root = std::env::var_os(DEBUG_RECORD_VARIABLE).filter(|value| !value.is_empty())?;
    Some(Arc::new(StreamRecorder::new(PathBuf::from(root))))
}

/// Записывающий отвод провода: по каталогу на каждый экземпляр сеанса.
pub(crate) struct StreamRecorder {
    root: PathBuf,
    state: Mutex<RecorderState>,
}

struct RecorderState {
    sessions: HashMap<SessionInstance, SessionFiles>,
    /// Запись остановлена ошибкой: больше ничего не пишем и не предупреждаем.
    stopped: bool,
}

impl StreamRecorder {
    /// Запись в `root`; каталог создаётся лениво, при первом сообщении сеанса.
    pub(crate) fn new(root: PathBuf) -> Self {
        // Громко и один раз: на диск пойдут данные сайта (только по явной просьбе владельца).
        warn!(
            variable = DEBUG_RECORD_VARIABLE,
            "Debug recording is ON: raw page stream bytes will be written to disk"
        );
        Self {
            root,
            state: Mutex::new(RecorderState {
                sessions: HashMap::new(),
                stopped: false,
            }),
        }
    }

    /// Выполняет запись для `session`; ошибка останавливает запись с одним предупреждением.
    fn record(
        &self,
        session: &SessionRef,
        action: impl FnOnce(&mut SessionFiles) -> Result<(), RecordingError>,
    ) {
        // Отравление замка не страшно: внутри только запись файлов, паниковать нечему.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopped {
            return;
        }
        let result = Self::files_for(&mut state, &self.root, session).and_then(action);
        if let Err(error) = result {
            // Остановка: освобождаем файлы, предупреждаем ровно один раз.
            state.stopped = true;
            state.sessions.clear();
            warn!(%error, "{RECORDING_STOPPED_MESSAGE}; playback continues without recording");
        }
    }

    /// Запись сеанса; создаётся при первом обращении (каталог `session-<n>`).
    fn files_for<'state>(
        state: &'state mut RecorderState,
        root: &std::path::Path,
        session: &SessionRef,
    ) -> Result<&'state mut SessionFiles, RecordingError> {
        let files = match state.sessions.entry(session.instance) {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => {
                let files = SessionFiles::create(root, SESSION_DIRECTORY_LABEL)?;
                info!(
                    directory = %files.directory().display(),
                    "Debug recording session directory created"
                );
                vacant.insert(files)
            }
        };
        Ok(files)
    }
}

impl SessionWireTap for StreamRecorder {
    fn page_text(&self, session: &SessionRef, bytes: &[u8]) {
        self.record(session, |files| files.record_page_text(bytes));
    }

    fn page_binary(&self, session: &SessionRef, bytes: &[u8]) {
        self.record(session, |files| files.record_page_binary(bytes));
    }

    fn player_text(&self, session: &SessionRef, json: &str) {
        self.record(session, |files| files.record_player_message(json));
    }

    fn connection_closed(&self, session: &SessionRef) {
        // Закрытие — повод дописать манифест, чтобы запись была целой.
        self.record(session, SessionFiles::flush_manifest);
    }
}

#[cfg(test)]
mod tests;
