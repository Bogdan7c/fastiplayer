//! Общие заглушки для модульных тестов crate-а.

use std::sync::Mutex;

use browser_bridge_protocol::{PageSessionMessage, SessionToken};

use crate::registry::SessionRef;

use crate::sink::{EventSinkFull, SessionEventSink, SessionLifecycleEvent};
use crate::token::{SessionTokenSource, TokenSourceError};

/// Выдаёт пропуска «000…01», «000…02» по порядку; может «сломаться».
pub(crate) struct SequenceTokens {
    issued: u32,
    broken: bool,
}

impl SequenceTokens {
    pub(crate) fn new() -> Self {
        Self {
            issued: 0,
            broken: false,
        }
    }

    pub(crate) fn broken() -> Self {
        Self {
            issued: 0,
            broken: true,
        }
    }
}

impl SessionTokenSource for SequenceTokens {
    fn next_token(&mut self) -> Result<SessionToken, TokenSourceError> {
        if self.broken {
            return Err(TokenSourceError::EntropyUnavailable {
                reason: "сломан".to_owned(),
            });
        }
        self.issued += 1;
        Ok(SessionToken::new(format!("{:032}", self.issued))?)
    }
}

/// Запоминает события жизни и сообщения страницы.
#[derive(Default)]
pub(crate) struct RecordingEvents {
    pub(crate) lifecycle: Mutex<Vec<SessionLifecycleEvent>>,
    pub(crate) page_messages: Mutex<Vec<(SessionRef, PageSessionMessage)>>,
}

impl SessionEventSink for RecordingEvents {
    fn publish_page_message(
        &self,
        session: &SessionRef,
        message: PageSessionMessage,
    ) -> Result<(), EventSinkFull> {
        self.page_messages
            .lock()
            .unwrap()
            .push((session.clone(), message));
        Ok(())
    }

    fn publish_lifecycle(&self, event: SessionLifecycleEvent) {
        self.lifecycle.lock().unwrap().push(event);
    }
}
