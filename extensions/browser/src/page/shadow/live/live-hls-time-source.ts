// Время тени эфира HLS (сессия 10a3, решение владельца 232): окно `live_window` плеера и сдвиг часов
// (`live-clock-alignment.ts`) превращаются в то, что видит и может страница.
//
// Владеет последним окном эфира (во времени плеера) и сдвигом. Отвечает:
// - тиками и целями перемоток в нужном времени (через сдвиг);
// - окном перемотки страницы: окно плеера + сдвиг; пока сдвиг или окно неизвестны, или у эфира нет DVR
//   (`seekable.kind = none`) — перематывать некуда (`not_seekable`, как пустой `seekable` у браузера);
// - краем эфира во времени страницы (для `buffered`, `live-hls-source-view.ts`);
// - возвратом: на край эфира, куда встанет сам браузер, а не на T (п. 4 решения 232).
//
// Окно чужого поколения сюда не доходит: его отсекает фон (`player-message-translator.ts`), владелец нумерации.

import type { HandoffEndPosition } from "../../../shared/handoff-contract.ts";
import type { MediaTimeMicros } from "../../../protocol/time.ts";
import type { ShadowSourceView } from "../file-source-mode.ts";
import type { MappedTime, PlayerLiveWindow, ReturnPosition, ShadowTimeSource } from "../shadow-time-source.ts";
import type { ClockTick, PageSeekRange } from "../shadow-timeline.ts";
import { LiveClockAlignment } from "./live-clock-alignment.ts";

/** Промежуток во времени страницы, мкс: `[начало, конец]`, начало не позже конца. */
export type PageSpanUs = readonly [MediaTimeMicros, MediaTimeMicros];

const NOT_SEEKABLE: PageSeekRange = { kind: "not_seekable" };
const JOIN_LIVE_ON_RETURN: ReturnPosition = { kind: "where_browser_joins_live" };

/** Как построить вид источника поверх снимка файла: его даёт `live-hls-source-view.ts`. */
export type LiveViewFactory = (source: LiveHlsTimeSource, snapshot: ShadowSourceView) => ShadowSourceView;

export class LiveHlsTimeSource implements ShadowTimeSource {
  readonly #alignment: LiveClockAlignment;
  readonly #createView: LiveViewFactory;
  /** Последнее окно эфира от плеера (во времени плеера); `undefined` — ещё не приходило. */
  #window: PlayerLiveWindow | undefined;
  #view: ShadowSourceView | undefined;

  /**
   * @param handoffPageUs секунда страницы в момент передачи (P0).
   * @param createView вид источника для страницы (длительность, `seekable`, `buffered`).
   */
  constructor(handoffPageUs: MediaTimeMicros, createView: LiveViewFactory) {
    this.#alignment = new LiveClockAlignment(handoffPageUs);
    this.#createView = createView;
  }

  // ------------------------------------------------------------------ ShadowTimeSource

  pageTickOf(tick: ClockTick): ClockTick {
    return this.#alignment.pageTickOf(tick);
  }

  pageTimeOf(playerUs: MediaTimeMicros): MappedTime {
    return this.#alignment.pageTimeOf(playerUs);
  }

  playerTimeOf(pageUs: MediaTimeMicros): MappedTime {
    return this.#alignment.playerTimeOf(pageUs);
  }

  playerSeekReplyTarget(requestedPlayerUs: MediaTimeMicros, _landedPageUs: MediaTimeMicros): MediaTimeMicros {
    // Плеер — правда о своём потоке: отвечаем его целью. Обратный перевод места тени соврал бы, когда время
    // страницы прижато к нулю (отрицательный сдвиг): плеер просил X, а получил бы −сдвиг.
    return requestedPlayerUs;
  }

  pageSeekRange(): PageSeekRange {
    const span = this.pageSeekableSpanUs();
    return span === undefined ? NOT_SEEKABLE : { kind: "range", startUs: span[0], endUs: span[1] };
  }

  acceptLiveWindow(window: PlayerLiveWindow): void {
    // Окно приходит только после показа поколения (договор плеера): с него начинаются настоящие часы.
    this.#alignment.notePlayerShowsGeneration();
    this.#window = window;
  }

  returnPositionFor(_end: HandoffEndPosition, _shadowPositionUs: MediaTimeMicros): ReturnPosition {
    // Позиция плеера — момент ЕГО шкалы; нативный элемент после перезагрузки начинает свою шкалу заново,
    // и записать туда T значило бы промахнуться. Решение 232: встать на край эфира, как браузер сам.
    return JOIN_LIVE_ON_RETURN;
  }

  sourceView(snapshot: ShadowSourceView | null): ShadowSourceView | null {
    // Без снимка файла (не бывает: тень эфира всегда со снятым источником) показать нечего, кроме настоящего элемента.
    if (snapshot === null) return null;
    this.#view ??= this.#createView(this, snapshot);
    return this.#view;
  }

  // ------------------------------------------------------------------ для вида источника

  /**
   * Окно перемотки во времени страницы. `undefined` — перематывать некуда: окна или сдвига ещё нет, эфир без
   * DVR, или всё окно раньше нуля страницы (сдвиг отрицательный, а страница видела эфир совсем недолго).
   */
  pageSeekableSpanUs(): PageSpanUs | undefined {
    const window = this.#window;
    if (window === undefined || window.seekable.kind === "none") return undefined;
    const start = this.#alignment.pageTimeOf(window.seekable.start_us);
    const end = this.#alignment.pageTimeOf(window.seekable.end_us);
    if (start.kind === "offset_unknown" || end.kind === "offset_unknown") return undefined;
    // Окно целиком раньше нуля страницы (сдвиг отрицательный) — перематывать странице некуда. Судим по сырому
    // моменту до прижатия: окно, кончающееся ровно в нуле страницы, законно (точка [0, 0]).
    if (this.#alignment.isBeforePageStart(window.seekable.end_us)) return undefined;
    return [start.us, end.us];
  }

  /** Край эфира во времени страницы; `undefined` — окна или сдвига ещё нет. */
  pageEdgeUs(): MediaTimeMicros | undefined {
    if (this.#window === undefined) return undefined;
    const edge = this.#alignment.pageTimeOf(this.#window.edge_us);
    return edge.kind === "mapped" ? edge.us : undefined;
  }
}
