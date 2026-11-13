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
// Эфир кончается (протокол 1.11 `recording_window`, 16d1, решения 325–327): плеер говорит, что шкала поколения —
// запись. Дальше страница видит конечную длительность (край записи + сдвиг) и одно `durationchange` на каждую её
// смену, как у настоящего элемента при `ENDLIST`; перемотка — по окну записи; возврат — на ту же секунду, считая
// от конца (решение 326). Запись бывает и сразу, без эфира: эфир кончился ровно к открытию плеером (бэклог п. 41) —
// тогда окно записи — первое окно поколения, и сдвиг часов фиксируется по нему так же, как по окну эфира.
//
// Окно чужого поколения сюда не доходит: его отсекает фон (`player-message-translator.ts`), владелец нумерации.

import type { HandoffEndPosition } from "../../../shared/handoff-contract.ts";
import type { MediaTimeMicros } from "../../../protocol/time.ts";
import type { ShadowSourceView } from "../file-source-mode.ts";
import type { MappedTime, PlayerLiveWindow, PlayerRecordingWindow, ReturnPosition, ShadowTimeSource } from "../shadow-time-source.ts";
import type { ClockTick, PageSeekRange } from "../shadow-timeline.ts";
import { LiveClockAlignment } from "./live-clock-alignment.ts";

/** Промежуток во времени страницы, мкс: `[начало, конец]`, начало не позже конца. */
export type PageSpanUs = readonly [MediaTimeMicros, MediaTimeMicros];

const NOT_SEEKABLE: PageSeekRange = { kind: "not_seekable" };
const JOIN_LIVE_ON_RETURN: ReturnPosition = { kind: "where_browser_joins_live" };

/** Как построить вид источника поверх снимка файла: его даёт `live-hls-source-view.ts`. */
export type LiveViewFactory = (source: LiveHlsTimeSource, snapshot: ShadowSourceView) => ShadowSourceView;

/** Последнее окно шкалы от плеера (во времени плеера): эфир или запись. */
type PlayerTimelineWindow =
  | { readonly kind: "live"; readonly window: PlayerLiveWindow }
  | { readonly kind: "recording"; readonly window: PlayerRecordingWindow };

/** Шкала, которую видит страница сейчас: эфир (`duration = Infinity`) или запись конечной длины. */
export type PageTimelineKind = "live" | "recording";

export class LiveHlsTimeSource implements ShadowTimeSource {
  readonly #alignment: LiveClockAlignment;
  readonly #createView: LiveViewFactory;
  readonly #announceDurationChange: () => void;
  /** Последнее окно шкалы от плеера (во времени плеера); `undefined` — ещё не приходило. */
  #window: PlayerTimelineWindow | undefined;
  #view: ShadowSourceView | undefined;
  /** Длительность записи, которую страница уже узнала (`durationchange`), мкс страницы; `undefined` — эфир. */
  #announcedDurationUs: MediaTimeMicros | undefined;

  /**
   * @param handoffPageUs секунда страницы в момент передачи (P0).
   * @param createView вид источника для страницы (длительность, `seekable`, `buffered`).
   * @param announceDurationChange отправить странице `durationchange` (длительность записи появилась или сменилась).
   */
  constructor(handoffPageUs: MediaTimeMicros, createView: LiveViewFactory, announceDurationChange: () => void) {
    this.#alignment = new LiveClockAlignment(handoffPageUs);
    this.#createView = createView;
    this.#announceDurationChange = announceDurationChange;
  }

  // ------------------------------------------------------------------ ShadowTimeSource

  pageTickOf(tick: ClockTick): ClockTick {
    const pageTick = this.#alignment.pageTickOf(tick);
    // Сдвиг фиксируется на тике: запись, пришедшая раньше него, получает длительность страницы только теперь.
    this.#announceDurationIfChanged();
    return pageTick;
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
    // Запись в эфир не превращается (эфир решается при открытии): запоздавшее окно эфира её не отменяет.
    if (this.#window?.kind === "recording") return;
    this.#window = { kind: "live", window };
  }

  acceptRecordingWindow(window: PlayerRecordingWindow): void {
    // Окно записи — тоже окно показанного поколения: по нему сдвиг фиксируется так же, как по окну эфира.
    this.#alignment.notePlayerShowsGeneration();
    this.#window = { kind: "recording", window };
    this.#announceDurationIfChanged();
  }

  returnPositionFor(end: HandoffEndPosition, shadowPositionUs: MediaTimeMicros): ReturnPosition {
    const timeline = this.#window;
    if (timeline?.kind !== "recording") {
      // Позиция плеера — момент ЕГО шкалы; нативный элемент после перезагрузки начинает свою шкалу заново,
      // и записать туда T значило бы промахнуться. Решение 232: встать на край эфира, как браузер сам.
      return JOIN_LIVE_ON_RETURN;
    }
    // Законченный эфир (решение 326): та же секунда, считая от конца — конец у плеера и у браузера общий.
    const playerPositionUs = this.#playerPositionAtReturn(end, shadowPositionUs);
    if (playerPositionUs === undefined) return JOIN_LIVE_ON_RETURN;
    return { kind: "counted_from_the_end", untilEndUs: Math.max(0, timeline.window.end_us - playerPositionUs) };
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
    const window = this.#window?.window;
    if (window === undefined || window.seekable.kind === "none") return undefined;
    const start = this.#alignment.pageTimeOf(window.seekable.start_us);
    const end = this.#alignment.pageTimeOf(window.seekable.end_us);
    if (start.kind === "offset_unknown" || end.kind === "offset_unknown") return undefined;
    // Окно целиком раньше нуля страницы (сдвиг отрицательный) — перематывать странице некуда. Судим по сырому
    // моменту до прижатия: окно, кончающееся ровно в нуле страницы, законно (точка [0, 0]).
    if (this.#alignment.isBeforePageStart(window.seekable.end_us)) return undefined;
    return [start.us, end.us];
  }

  /** Край шкалы во времени страницы (эфира или записи); `undefined` — окна или сдвига ещё нет. */
  pageEdgeUs(): MediaTimeMicros | undefined {
    const timeline = this.#window;
    if (timeline === undefined) return undefined;
    const edgeUs = timeline.kind === "live" ? timeline.window.edge_us : timeline.window.end_us;
    const edge = this.#alignment.pageTimeOf(edgeUs);
    return edge.kind === "mapped" ? edge.us : undefined;
  }

  /** Что страница видит сейчас: эфир или запись (запись — с первого окна записи). */
  pageTimelineKind(): PageTimelineKind {
    return this.#window?.kind === "recording" ? "recording" : "live";
  }

  /**
   * Длительность записи во времени страницы; `undefined` — эфир или сдвиг ещё неизвестен (тогда страница видит
   * `Infinity`, как до окна записи).
   */
  pageDurationUs(): MediaTimeMicros | undefined {
    return this.pageTimelineKind() === "recording" ? this.pageEdgeUs() : undefined;
  }

  // ------------------------------------------------------------------ внутреннее

  /** Длительность страницы сменилась с последнего объявления — одно `durationchange`, как у элемента. */
  #announceDurationIfChanged(): void {
    const durationUs = this.pageDurationUs();
    if (durationUs === undefined || durationUs === this.#announcedDurationUs) return;
    this.#announcedDurationUs = durationUs;
    this.#announceDurationChange();
  }

  /** Где плеер в момент возврата (время плеера): его позиция или место тени, переведённое обратно. */
  #playerPositionAtReturn(end: HandoffEndPosition, shadowPositionUs: MediaTimeMicros): MediaTimeMicros | undefined {
    if (end.kind === "at") return end.position_us;
    const mapped = this.#alignment.playerTimeOf(shadowPositionUs);
    return mapped.kind === "mapped" ? mapped.us : undefined;
  }
}
