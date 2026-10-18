//! Общие помощники тестов web-дорожек: фальшивый загрузчик, источник, выбор из меню.
//!
//! Фальшивый загрузчик играет контракт `WebSubtitleDownloader` без сети: отдаёт заранее
//! заданные ответы по очереди, считает вызовы и запоминает токены отмены.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use media_source_open::web_subtitle_fetch::{
    WebSubtitleFetchError, WebSubtitleFormat, WebSubtitleTrackId,
};
use source_core::CancellationToken;
use web_media_core::{LanguageTag, SubtitleKind};

use super::super::web_download::{DownloadedWebSubtitle, WebSubtitleDownloader};
use super::super::web_track::{WebSubtitleSource, WebTrackInfo};
use super::track_selection::settle;
use super::*;
use crate::subtitle_message::{TrackNumber, track_text};
use crate::ui::player_controls::SubtitleControlAction;

/// Что делает фальшивый загрузчик на очередной вызов.
#[derive(Debug)]
pub(super) enum FakeReply {
    /// Вернуть эти байты.
    Bytes(DownloadedWebSubtitle),
    /// Вернуть эту ошибку.
    Error(WebSubtitleFetchError),
    /// Ждать, пока вызывающий не отменит токен, и вернуть `Cancelled` (проверка отмены).
    BlockUntilCancelled,
}

/// Фальшивый загрузчик.
#[derive(Debug, Default)]
pub(super) struct FakeWebDownloader {
    replies: Mutex<VecDeque<FakeReply>>,
    requested: Mutex<Vec<WebSubtitleTrackId>>,
    calls: AtomicUsize,
    cancellation_observed: AtomicBool,
    /// Лимит размера, с которым звали последний раз.
    last_max_bytes: Mutex<Option<NonZeroUsize>>,
}

impl FakeWebDownloader {
    /// Загрузчик с заданной очередью ответов; когда очередь кончилась, отвечает `NotFound`.
    pub(super) fn with_replies(replies: Vec<FakeReply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            ..Self::default()
        })
    }

    /// Сколько раз скачивание вызывали.
    pub(super) fn call_count(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }

    /// Какие дорожки просили, по порядку.
    pub(super) fn requested_tracks(&self) -> Vec<WebSubtitleTrackId> {
        self.requested.lock().expect("lock").clone()
    }

    /// Видел ли блокирующий вызов, что токен отменён.
    pub(super) fn observed_cancellation(&self) -> bool {
        self.cancellation_observed.load(Ordering::Acquire)
    }

    /// Лимит последнего вызова.
    pub(super) fn last_max_bytes(&self) -> Option<NonZeroUsize> {
        *self.last_max_bytes.lock().expect("lock")
    }
}

impl WebSubtitleDownloader for FakeWebDownloader {
    fn download(
        &self,
        track: &WebSubtitleTrackId,
        max_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<DownloadedWebSubtitle, WebSubtitleFetchError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.requested.lock().expect("lock").push(track.clone());
        *self.last_max_bytes.lock().expect("lock") = Some(max_bytes);
        let reply = self.replies.lock().expect("lock").pop_front();
        match reply {
            Some(FakeReply::Bytes(downloaded)) => Ok(downloaded),
            Some(FakeReply::Error(error)) => Err(error),
            Some(FakeReply::BlockUntilCancelled) => {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while std::time::Instant::now() < deadline {
                    if cancellation.is_cancelled() {
                        self.cancellation_observed.store(true, Ordering::Release);
                        return Err(WebSubtitleFetchError::Cancelled);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(WebSubtitleFetchError::Timeout)
            }
            None => Err(WebSubtitleFetchError::NotFound),
        }
    }
}

/// Ответ-успех: srv3 с этим текстом файла.
pub(super) fn srv3_reply(xml: &str) -> FakeReply {
    FakeReply::Bytes(DownloadedWebSubtitle {
        bytes: xml.as_bytes().to_vec(),
        format: WebSubtitleFormat::Srv3,
    })
}

/// Ручной srv3: одна реплика с 1 по 4 с.
pub(super) const MANUAL_SRV3: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3"><body><p t="1000" d="3000">Hello world</p></body></timedtext>"#;

/// Автосубтитры srv3: слово WORLD появляется через 2 с после начала абзаца (0 с).
pub(super) const ASR_SRV3: &str = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3"><head><ws id="0"/><ws id="1" mh="2" ju="0" sd="3"/><wp id="0"/><wp id="1" ap="6" ah="20" av="100" rc="2" cc="40"/></head><body><w t="0" id="1" wp="1" ws="1"/><p t="0" d="5000" w="1"><s ac="255">HELLO</s><s t="2000" ac="255"> WORLD</s></p></body></timedtext>"#;

/// Описание web-дорожки для теста.
pub(super) fn web_track(kind: SubtitleKind, language: &str, name: Option<&str>) -> WebTrackInfo {
    WebTrackInfo {
        id: web_track_id(kind, language),
        display_name: name.map(str::to_owned),
    }
}

/// Идентификатор web-дорожки.
pub(super) fn web_track_id(kind: SubtitleKind, language: &str) -> WebSubtitleTrackId {
    WebSubtitleTrackId::new(kind, LanguageTag::new(language).expect("valid language"))
}

/// Источник с этими дорожками и загрузчиком.
pub(super) fn web_source(
    tracks: Vec<WebTrackInfo>,
    downloader: &Arc<FakeWebDownloader>,
) -> WebSubtitleSource {
    WebSubtitleSource::new(
        tracks,
        Arc::clone(downloader) as Arc<dyn WebSubtitleDownloader>,
    )
}

/// Владелец с web-media: настоящий `begin_media` и ожидание фоновых задач.
pub(super) fn web_owner(
    config: &SubtitlesConfig,
    media: MediaInstanceId,
    source: WebSubtitleSource,
) -> SubtitleOwner {
    let mut owner = owner_with_config(config);
    owner.begin_media(media, SubtitleMediaSource::Web(source));
    settle(&mut owner);
    owner
}

/// Подписи меню в порядке списка: `(текст, это автоперевод)`.
pub(super) fn menu_texts(owner: &SubtitleOwner) -> Vec<(String, bool)> {
    owner
        .menu_view()
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            (
                track_text(&track.label, TrackNumber::from_menu_index(index)),
                track.label.is_auto_translation(),
            )
        })
        .collect()
}

/// Идентификатор строки меню по её подписи.
pub(super) fn menu_track_with_text(
    owner: &SubtitleOwner,
    text: &str,
) -> subtitle_core::SubtitleTrackId {
    let view = owner.menu_view();
    view.tracks
        .iter()
        .enumerate()
        .find(|(index, track)| {
            track_text(&track.label, TrackNumber::from_menu_index(*index)) == text
        })
        .map(|(_, track)| track.id)
        .unwrap_or_else(|| panic!("menu has no row `{text}`"))
}

/// Клик по строке меню — то же действие, что делает UI.
pub(super) fn click_menu_row(owner: &mut SubtitleOwner, text: &str) {
    let track_id = menu_track_with_text(owner, text);
    owner.apply_control_action(SubtitleControlAction::SelectTrack(track_id));
}

/// Тексты уведомлений, ожидающих показа.
pub(super) fn take_failures(owner: &mut SubtitleOwner) -> Vec<WebSubtitleFetchError> {
    owner.take_download_failures()
}
