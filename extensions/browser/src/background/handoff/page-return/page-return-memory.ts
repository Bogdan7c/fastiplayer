// Память возврата страницы (сессия 11b2, решения 240–241): что фон должен знать, чтобы открыть ушедшую
// страницу заново и найти на ней то же видео.
//
// Владелец — сеанс передачи; живёт только в памяти фона (решение 240: адрес страницы не уходит ни плееру,
// ни в хранилище). Помнит три вещи:
// - где жила страница: адрес, окно, инкогнито — их сеанс записывает при старте и после каждого возврата;
// - что за видео в окне плеера: длина фильма (сверка ± 2 с) или «эфир». Реклама фильма не называет: пока на
//   экране реклама, возвращать нечего (фильм неизвестен), и уход страницы кончается прежним `tab_gone`;
// - последнюю секунду плеера — из его часов (4 раза в секунду, и в сироте тоже). На неё и встанет
//   вернувшаяся страница: плеер всё это время играет скачанное, секунда просьбы `reopen_page` уже устарела.

import type { PageReturnTarget } from "../../../shared/handoff-contract.ts";
import type { MediaTimeMicros } from "../../../protocol/time.ts";

const MICROS_PER_SECOND = 1_000_000;

/** Где жила страница. */
export interface ReturnablePage {
  readonly url: string;
  readonly windowId: number;
  readonly incognito: boolean;
}

/** Видео, которое сеанс поставил в плеер: роль и длина (`null` — неизвестна или бесконечна, эфир). */
export interface VideoInPlayer {
  readonly role: "ad" | "film";
  readonly durationS: number | null;
}

/** Что известно о видео в плеере. */
type RememberedVideo = { readonly kind: "recorded"; readonly durationS: number } | { readonly kind: "live" } | { readonly kind: "unknown" };

export interface PageReturnMemory {
  /** Страница теперь живёт здесь (`undefined` — браузер вкладку не описал: вернуть её нельзя). */
  notePage(page: ReturnablePage | undefined): void;
  /** Тень встала на видео (`handoff_began{started}`). */
  noteVideoInPlayer(video: VideoInPlayer): void;
  /** Тик часов плеера. */
  notePlayerSecond(positionUs: MediaTimeMicros): void;
  /** Где жила страница; `undefined` — неизвестно. */
  page(): ReturnablePage | undefined;
  /** Цель взвода возврата; `undefined` — фильм неизвестен (реклама на экране или видео ещё не встало). */
  returnTarget(): PageReturnTarget | undefined;
}

export function createPageReturnMemory(): PageReturnMemory {
  let page: ReturnablePage | undefined;
  let video: RememberedVideo = { kind: "unknown" };
  let playerSecond = 0;

  return {
    notePage(next) {
      page = next;
    },
    noteVideoInPlayer(next) {
      if (next.role === "ad") video = { kind: "unknown" };
      else video = next.durationS === null ? { kind: "live" } : { kind: "recorded", durationS: next.durationS };
    },
    notePlayerSecond(positionUs) {
      playerSecond = Math.max(0, positionUs / MICROS_PER_SECOND);
    },
    page: () => page,
    returnTarget() {
      switch (video.kind) {
        case "unknown":
          return undefined;
        case "live":
          return { kind: "live" };
        case "recorded":
          // Секунда не дальше конца ролика: часы на «доиграно» дали бы перемотку за край.
          return { kind: "recorded", duration_s: video.durationS, start_s: Math.min(playerSecond, video.durationS) };
      }
    },
  };
}
