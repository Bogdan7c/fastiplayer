// Внутренняя команда «опроси вкладку N» (сессия 10b): попап при открытии узнаёт, какие видео есть на
// вкладке и стоит ли уже одно из них в плеере. Как и `hand_off_tab`, её шлют только страницы самого
// экстеншена. Опрос плеер НЕ будит: он только обновляет модель вкладки (решение 198) и отвечает.

import type { VideoRef } from "./handoff-contract.ts";
import type { TabVideoRecord } from "../background/tab-model/tab-snapshot.ts";

export const SURVEY_TAB_KIND = "survey_tab";

export interface SurveyTabRequest {
  readonly kind: typeof SURVEY_TAB_KIND;
  readonly tab_id: number;
}

/**
 * Что у вкладки с плеером.
 * - `no_session` — передачи нет. «Есть» считается только после исхода `started`: пока передача
 *   стартует, сеанса нет, попап шлёт `hand_off_tab` и получает исход той же передачи;
 * - `in_player` — видео вкладки в плеере. `video` — элемент, на котором сеанс стоит сейчас (с учётом
 *   переезда на следующее видео); `null` — сеанс между видео (реклама кончилась, фильм ещё не поставлен).
 */
export type TabPlayerPresence =
  | { readonly kind: "no_session" }
  | { readonly kind: "in_player"; readonly video: VideoRef | null };

/** Почему вкладку нельзя опросить. */
export type SurveyUnavailableReason =
  /** Страница браузера, магазин расширений, закрытая вкладка: внедрение запрещено. */
  | "tab_not_scriptable"
  /** Фреймы не ответили вовремя. */
  | "survey_timed_out"
  /** Внутренняя ошибка экстеншена при опросе (не должна случаться). */
  | "survey_failed";

export type SurveyTabReply =
  | {
      readonly kind: "surveyed";
      /** Найденные видео: главное первым, дальше самые вероятные (как `videosFoundIn`). */
      readonly videos: readonly TabVideoRecord[];
      /** Главное видео по правилу выбора; `null` — передавать нечего. */
      readonly main: VideoRef | null;
      readonly in_player: TabPlayerPresence;
    }
  | { readonly kind: "unavailable"; readonly reason: SurveyUnavailableReason };
