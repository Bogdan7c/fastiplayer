// Смена серии на той же странице (протокол 1.9, сессия 16a, решения 303, 306-308). Зеркало
// session/episode_change.rs.
//
// Страница -> плеер, оба слова вне поколений потока и только при общей версии сеанса не ниже 1.9:
// - `awaiting_next_episode{patience_ms}` - серия у плеера доиграла (часы сказали `ended`), страница сняла
//   источник, экстеншен ждёт новый не дольше `patience_ms` миллисекунд (любое u32). Окно плеера держит
//   последний кадр с надписью ожидания, новое поколение надпись снимает.
// - `page_title_changed{title}` - заголовок вкладки сменился. Правило то же, что у `title` в `open_session`
//   (`pageTitleCodec`: до 1024 байт, без управляющих символов и переворота направления, пустой допустим);
//   чистка до отправки - та же `sanitizePageTitle` (`pageTitleChangedMessage` ниже).
// Пока страница ушла (`page_left`), сторож плеера оба слова отвергает.
//
// Срок вышел - экстеншен закрывает связь кодом 4011 `next_episode_not_started`. Код на проводе не
// версионируется, но старый плеер (общая версия ниже 1.9) его не знает и молча кончил бы сеанс без
// объяснения, поэтому со старым плеером закрывается прежним 4009 (`closeReasonForUnstartedNextEpisode`).

import { plainInteger } from "../numbers.ts";
import type { Codec } from "../schema.ts";
import { sanitizePageTitle } from "../sender.ts";
import { PROTOCOL_VERSION_1_9, type ProtocolVersion, versionIncludes } from "../version.ts";
import type { SessionCloseReason } from "./close-reason.ts";

/** Срок ожидания следующей серии в миллисекундах: весь диапазон u32, как `NextEpisodePatience` в Rust. */
export const nextEpisodePatienceCodec: Codec<number> = plainInteger("u32");

/**
 * С какой общей версии экстеншен вправе закрыть связь кодом 4011 `next_episode_not_started`.
 * Зеркало `NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN` (episode_change.rs).
 */
export const NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN: ProtocolVersion = PROTOCOL_VERSION_1_9;

/** Слово `page_title_changed` (форма - как в `PageSessionMessage`). */
export interface PageTitleChangedMessage {
  readonly type: "page_title_changed";
  readonly title: string;
}

/**
 * Собирает `page_title_changed` из сырого заголовка вкладки: та же чистка и тот же предел, что у
 * заголовка при открытии сеанса (`sanitizePageTitle`), поэтому результат всегда проходит строгую
 * проверку протокола. Слать его можно только при общей версии не ниже 1.9 (`pageSessionIntroducedIn`).
 */
export function pageTitleChangedMessage(rawTitle: string): PageTitleChangedMessage {
  return { type: "page_title_changed", title: sanitizePageTitle(rawTitle) };
}

/**
 * Каким кодом закрыть связь, когда страница не поставила следующую серию за срок ожидания:
 * при общей версии не ниже 1.9 - `next_episode_not_started` (4011), со старым плеером - прежним
 * `page_took_video_back` (4009), который он знает и объясняет зрителю.
 */
export function closeReasonForUnstartedNextEpisode(
  agreed: ProtocolVersion,
): Extract<SessionCloseReason, "next_episode_not_started" | "page_took_video_back"> {
  return versionIncludes(agreed, NEXT_EPISODE_NOT_STARTED_INTRODUCED_IN) ? "next_episode_not_started" : "page_took_video_back";
}
