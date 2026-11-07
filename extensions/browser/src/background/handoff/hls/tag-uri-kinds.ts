// Вид ресурса по тегу, в котором стоит `URI="..."` (RFC 8216 / 8216bis). Вид решает кэш браузера
// (плейлист - без кэша) и то, как плеер дальше разбирает ответ.

import type { ResourceKind } from "./handle-desk.ts";

/** В мастер-плейлисте по URI лежат плейлисты (вариантов, дорожек, I-frame). */
const MASTER_PLAYLIST_URI_TAGS: ReadonlySet<string> = new Set(["#EXT-X-MEDIA", "#EXT-X-I-FRAME-STREAM-INF"]);
/** В медиа-плейлисте по URI лежит плейлист только у отчёта о рендишне (LL-HLS). */
const MEDIA_PLAYLIST_URI_TAGS: ReadonlySet<string> = new Set(["#EXT-X-RENDITION-REPORT"]);

/**
 * Вид ресурса по `URI` тега. Всё, чего нет в таблицах (EXT-X-KEY, EXT-X-MAP, EXT-X-PART,
 * EXT-X-PRELOAD-HINT, SESSION-KEY, SESSION-DATA, неизвестные теги), - медиа.
 */
export function uriKindOfTag(tagName: string, isMasterPlaylist: boolean): ResourceKind {
  const playlistTags = isMasterPlaylist ? MASTER_PLAYLIST_URI_TAGS : MEDIA_PLAYLIST_URI_TAGS;
  return playlistTags.has(tagName) ? "playlist" : "media";
}
