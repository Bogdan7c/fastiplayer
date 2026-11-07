// Время источника тени: как часы и окна плеера превращаются во время страницы и обратно (сессия 10a3).
//
// Модель тени (`shadow-timeline.ts`) живёт ТОЛЬКО во времени страницы. Всё, что приходит от плеера
// (тик часов, перемотка, окно эфира, позиция возврата), и всё, что уходит к нему (`page_seek`,
// `player_seek_applied`), контроллер тени пропускает через этот шов. Так «если эфир» не расползается по
// контроллеру и модели: у MSE, файла и записи HLS время плеера И ЕСТЬ время страницы (поведение до 10a3),
// у эфира HLS — со сдвигом (`live/live-hls-time-source.ts`, решение владельца 232).

import type { HandoffEndPosition, PageEvent } from "../../shared/handoff-contract.ts";
import type { LiveSeekable } from "../../protocol/session/live-window.ts";
import type { MediaTimeMicros } from "../../protocol/time.ts";
import type { ShadowSourceView } from "./file-source-mode.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import type { ClockTick, PageSeekRange, ShadowTimeline } from "./shadow-timeline.ts";

/** Момент, переведённый в другое время: сдвиг известен — момент, неизвестен — переводить не во что. */
export type MappedTime = { readonly kind: "mapped"; readonly us: MediaTimeMicros } | { readonly kind: "offset_unknown" };

/**
 * Куда поставить настоящий элемент, когда тень снимается:
 * - `at` — на момент `targetUs` во времени страницы (MSE, файл, запись HLS: возврат на T);
 * - `where_browser_joins_live` — никуда не ставить: элемент заново загружает эфир и сам встаёт там, где
 *   браузер входит в эфир при свежей загрузке (решение 232, факты Chromium 156 — пакет D 10a3). Позиция T
 *   эфира плеера элементу ничего не значит: у нативного элемента после перезагрузки своя шкала от нуля.
 */
export type ReturnPosition = { readonly kind: "at"; readonly targetUs: MediaTimeMicros } | { readonly kind: "where_browser_joins_live" };

/**
 * Играть ли настоящему элементу после возврата. `requestedPlaying` — что просил плеер (или что видела страница).
 *
 * Эфир HLS возвращается ВСЕГДА с игрой (решение владельца 234, исключение из решения 51 «закрытие окна —
 * пауза»): встроенный HLS Chromium 156 падает с `DEMUXER_ERROR_COULD_NOT_PARSE` (`kNoDataEverAppended`
 * в `hls_rendition_impl.cc`), если эфир загрузить на паузе и потом нажать play, — опыт 10a3 без экстеншена:
 * 28 падений из 38. Загрузка вместе с `play()` в той же задаче (как у сайта) не падала ни разу. Остальные
 * источники — как просили.
 */
export function resumesPlayingOnReturn(position: ReturnPosition, requestedPlaying: boolean): boolean {
  return position.kind === "where_browser_joins_live" || requestedPlaying;
}

/** Окно эфира от плеера (команда тени `live_window`): всё во времени плеера. */
export interface PlayerLiveWindow {
  readonly edge_us: MediaTimeMicros;
  readonly seekable: LiveSeekable;
}

/** Время источника тени (см. шапку). Методы не трогают ни элемент, ни модель тени: только числа. */
export interface ShadowTimeSource {
  /** Тик часов плеера → тик, который увидит модель тени (во времени страницы). */
  pageTickOf(tick: ClockTick): ClockTick;
  /** Момент плеера → момент страницы (цель перемотки, которую попросил плеер). */
  pageTimeOf(playerUs: MediaTimeMicros): MappedTime;
  /** Момент страницы → момент плеера (цель `page_seek`, ответ `player_seek_applied`). */
  playerTimeOf(pageUs: MediaTimeMicros): MappedTime;
  /**
   * Цель для ответа `player_seek_applied`: `requestedPlayerUs` — что просил плеер, `landedPageUs` — где встала
   * модель тени (время страницы, уже прижатое к шкале).
   */
  playerSeekReplyTarget(requestedPlayerUs: MediaTimeMicros, landedPageUs: MediaTimeMicros): MediaTimeMicros;
  /** Куда странице можно перематываться сейчас. */
  pageSeekRange(): PageSeekRange;
  /** Плеер прислал окно эфира (поколение уже сверил фон). Источник без эфира окна не ждёт и пропускает его. */
  acceptLiveWindow(window: PlayerLiveWindow): void;
  /** Куда ставить элемент при снятии тени; `shadowPositionUs` — где тень сейчас (время страницы). */
  returnPositionFor(end: HandoffEndPosition, shadowPositionUs: MediaTimeMicros): ReturnPosition;
  /**
   * Что страница видит вместо настоящего источника. `snapshot` — снимок файла (`null` у MSE: отвечает
   * настоящий элемент); источник может показать его как есть или поверх него.
   */
  sourceView(snapshot: ShadowSourceView | null): ShadowSourceView | null;
}

/**
 * Время плеера = время страницы: MSE, файл `<video src>`, запись HLS. Ровно прежнее поведение тени:
 * тики и цели проходят как есть, страница перематывается по всей шкале, возврат — на T.
 */
export const PLAYER_TIME_IS_PAGE_TIME: ShadowTimeSource = {
  pageTickOf: (tick) => tick,
  pageTimeOf: (playerUs) => ({ kind: "mapped", us: playerUs }),
  playerTimeOf: (pageUs) => ({ kind: "mapped", us: pageUs }),
  // Как до 10a3: ответ — цель, прижатая к шкале `[0, длительность]` (время плеера = время страницы).
  playerSeekReplyTarget: (_requestedPlayerUs, landedPageUs) => landedPageUs,
  pageSeekRange: () => ({ kind: "whole_timeline" }),
  // Окно эфира шлётся только для поколения live HLS: у этих источников его быть не может, а чужое окно
  // не должно менять то, что видит страница записи или файла.
  acceptLiveWindow: () => {},
  returnPositionFor: (end, shadowPositionUs) => ({ kind: "at", targetUs: end.kind === "at" ? end.position_us : shadowPositionUs }),
  sourceView: (snapshot) => snapshot,
};

/**
 * Перемотка, которую попросил плеер: модель тени встаёт на цель (во времени страницы), ответ
 * `player_seek_applied` — во времени плеера. Сдвиг эфира ещё неизвестен — странице показать нечего (она
 * стоит на P0 и ждёт), плееру отвечаем его же целью.
 */
export function applyPlayerSeek(
  timeSource: ShadowTimeSource,
  timeline: Pick<ShadowTimeline, "playerSeek">,
  command: { readonly seek_id: number; readonly target_us: MediaTimeMicros },
  nowMs: number,
): Extract<PageEvent, { kind: "player_seek_applied" }> {
  const pageTarget = timeSource.pageTimeOf(command.target_us);
  if (pageTarget.kind === "offset_unknown") return { kind: "player_seek_applied", seek_id: command.seek_id, target_us: command.target_us };
  const landedPageUs = timeline.playerSeek(pageTarget.us, nowMs);
  return { kind: "player_seek_applied", seek_id: command.seek_id, target_us: timeSource.playerSeekReplyTarget(command.target_us, landedPageUs) };
}

/**
 * `page_seek` плееру на цель страницы — во времени плеера (у эфира — цель минус сдвиг). `undefined` — сдвиг
 * неизвестен: окно перемотки бывает только при известном сдвиге, так что это внутренняя ошибка (со следом).
 */
export function pageSeekEventFor(timeSource: ShadowTimeSource, pageTargetUs: MediaTimeMicros): Extract<PageEvent, { kind: "page_seek" }> | undefined {
  const playerTarget = timeSource.playerTimeOf(pageTargetUs);
  if (playerTarget.kind === "mapped") return { kind: "page_seek", target_us: playerTarget.us };
  shadowProblemReporter.report("shadow: page seek before the live clock offset is known", undefined);
  return undefined;
}
