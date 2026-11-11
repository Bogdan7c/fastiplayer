// «Ждём следующую серию» → плееру `awaiting_next_episode{patience_ms}` (протокол 1.9, сессия 16a, решения
// 303, 306): окно держит последний кадр с «Waiting for the next episode…», новое поколение надпись снимает.
//
// Один владелец на оба пути ожидания: тень ждёт на том же элементе (донесение `next_episode_wait_began`)
// или сеанс ждёт переезда на новый элемент или файл (`video-relocation.ts`). Слово уходит ОДИН раз на
// видео: повторное начало ожидания (неудавшийся переезд, тень сначала ждала, потом страница поставила
// чужой источник) второго слова не даёт. Новое видео или новое поколение — новая серия, её конец снова
// даст слово (`videoBegan`).

import { pageSessionIntroducedIn, versionIncludes, type PageSessionMessage, type ProtocolVersion } from "../../../protocol/index.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../../shared/next-episode-wait.ts";

/** Что сделал модуль с началом ожидания (для тестов и журнала разработчика). */
export type NextEpisodeNoticeOutcome =
  /** Плееру ушло `awaiting_next_episode`. */
  | "sent"
  /** Для этого видео слово уже ушло. */
  | "already_sent"
  /** Плеер договорился на версию до 1.9: слова не знает (срок ожидания всё равно тот же). */
  | "player_too_old"
  /** Плеер сейчас страницу не слушает (страница ушла или возвращается). */
  | "player_not_listening";

export interface NextEpisodeNoticePorts {
  readonly agreedProtocol: () => ProtocolVersion;
  readonly playerHearsPage: () => boolean;
  readonly send: (message: PageSessionMessage) => void;
}

export interface NextEpisodeNotice {
  /** Началось ожидание следующей серии (любым путём). */
  waitBegan(): NextEpisodeNoticeOutcome;
  /** На экране плеера началось новое видео или поколение: его конец — новое ожидание. */
  videoBegan(): void;
}

export function createNextEpisodeNotice(ports: NextEpisodeNoticePorts): NextEpisodeNotice {
  let sentForCurrentVideo = false;
  return {
    waitBegan() {
      if (sentForCurrentVideo) return "already_sent";
      if (!versionIncludes(ports.agreedProtocol(), pageSessionIntroducedIn("awaiting_next_episode"))) return "player_too_old";
      if (!ports.playerHearsPage()) return "player_not_listening";
      ports.send({ type: "awaiting_next_episode", patience_ms: NEXT_EPISODE_WAIT_MS });
      sentForCurrentVideo = true;
      return "sent";
    },
    videoBegan() {
      sentForCurrentVideo = false;
    },
  };
}
