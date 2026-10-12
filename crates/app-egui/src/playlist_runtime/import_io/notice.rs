//! Уведомления о том, как был прочитан файл плейлиста (решение владельца B1).
//!
//! Парсеры `playlist-io` сообщают типизированную кодировку; этот модуль переводит её в
//! короткий факт для пользователя и хранит его до тех пор, пока приложение не заберёт
//! уведомления ровно один раз (по образцу `drain_automatic_queue_notices`).
//! Текст и вид показа выбирает приложение, а не runtime.

use playlist_io::{CueTextEncoding, LocalPlaylistExpansionSummary};

/// Человеческий текст уведомления: что именно сделало приложение, без технических деталей.
pub(crate) const WINDOWS_1251_NOTICE_MESSAGE: &str = "Playlist read using Windows-1251 encoding";

/// Факт о чтении плейлиста, о котором стоит сказать пользователю.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistImportNotice {
    /// Файл не был валидным UTF-8 и прочитан как Windows-1251.
    TextReadAsWindows1251,
}

impl PlaylistImportNotice {
    /// Текст для информационного toast-а.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::TextReadAsWindows1251 => WINDOWS_1251_NOTICE_MESSAGE,
        }
    }
}

/// Уведомления для результата рекурсивного раскрытия `.m3u`/`.m3u8`/`.xspf`.
///
/// Несколько вложенных файлов в Windows-1251 дают одно уведомление, а не по одному на файл.
pub(super) fn notices_for_expansion(
    summary: LocalPlaylistExpansionSummary,
) -> Vec<PlaylistImportNotice> {
    if summary.windows_1251_documents() > 0 {
        vec![PlaylistImportNotice::TextReadAsWindows1251]
    } else {
        Vec::new()
    }
}

/// Уведомления для одиночного `.cue`.
pub(super) fn notices_for_cue(encoding: CueTextEncoding) -> Vec<PlaylistImportNotice> {
    match encoding {
        CueTextEncoding::Windows1251 => vec![PlaylistImportNotice::TextReadAsWindows1251],
        CueTextEncoding::Utf8
        | CueTextEncoding::Utf8WithBom
        | CueTextEncoding::Utf16LittleEndianWithBom
        | CueTextEncoding::Utf16BigEndianWithBom => Vec::new(),
    }
}

/// Очередь ещё не показанных уведомлений; принадлежит `PlaylistImportIoOwner`.
#[derive(Debug, Default)]
pub(super) struct PlaylistImportNoticeQueue {
    pending: Vec<PlaylistImportNotice>,
}

impl PlaylistImportNoticeQueue {
    /// Добавляет уведомления разобранного плейлиста.
    pub(super) fn extend(&mut self, notices: impl IntoIterator<Item = PlaylistImportNotice>) {
        self.pending.extend(notices);
    }

    /// Отдаёт накопленное ровно один раз.
    pub(super) fn drain(&mut self) -> Vec<PlaylistImportNotice> {
        std::mem::take(&mut self.pending)
    }
}
