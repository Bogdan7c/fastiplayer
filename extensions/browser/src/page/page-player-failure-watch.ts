// Сторож «плеер страницы сломался» (сессия 11a, решение 237): переданный MSE-элемент получил ошибку.
//
// Под тенью MediaSource принадлежит странице: её плеер льёт в него куски, а браузер их раскладывает. Если
// куски битые (или страница сама позвала `endOfStream("decode")`), у настоящего элемента появляется
// `error`, MediaSource закрывается, и плеер страницы либо оправляется (новый MediaSource — новое поколение),
// либо умирает. Во втором случае без сторожа заглушка висела бы над мёртвым видео, а наш плеер ждал бы
// байтов, которых не будет. Поэтому:
//  1. ошибка держится дольше `PAGE_PLAYER_FAILURE_GRACE_MS` → контроллер сам кончает передачу (тень снята
//     на её позиции, на паузе), причина конца — `page_player_failed`;
//  2. страница в пределах `PAGE_PLAYER_FAILURE_RELEASE_WINDOW_MS` после ошибки бросила источник (тень
//     сказала `element_released`) → это тоже сломавшийся плеер, а не «страница забрала видео»: причина
//     переписывается на `page_player_failed` (как флуд переписывает причину в контроллере);
//  3. после ошибки к элементу подключён новый MediaSource → плеер страницы оправился, сторож забывает ошибку.
//
// Только для MSE: у файла и HLS под тенью источник настоящего элемента снят нами, и его ошибки — наши, а не
// страницы. Время проверяется на тиках часов плеера (как `empty-source-watch.ts`): таймеры скрытой вкладки
// браузер будит раз в минуту, а часы плеера идут раз в ~250 мс и на паузе.

import type { HandoffEndCause } from "../shared/handoff-contract.ts";

/**
 * Сколько ошибка элемента может держаться, прежде чем считать плеер страницы мёртвым (решение 237).
 * Плееры, умеющие оправляться (hls.js `recoverMediaError`, Shaka, dash.js), ставят новый MediaSource за
 * сотни миллисекунд; 5 с — с запасом и столько же, сколько допуск пустого источника (`EMPTY_SOURCE_GRACE_MS`).
 */
export const PAGE_PLAYER_FAILURE_GRACE_MS = 5_000;

/**
 * Сколько после ошибки отказ страницы от источника считается следствием ошибки, а не решением страницы.
 * Пустой источник тень терпит 5 с (`EMPTY_SOURCE_GRACE_MS`), убранный элемент — свой допуск; 10 с покрывают
 * «ошибка → страница сняла источник → допуск тени вышел» с запасом на медленные тики скрытой вкладки.
 */
export const PAGE_PLAYER_FAILURE_RELEASE_WINDOW_MS = 10_000;

/** Что делать контроллеру по итогам тика. */
export type PagePlayerFailureTickDecision = "keep_handoff" | "end_for_page_player_failure";

/** Чтения и подписки, нужные сторожу (оригиналы, снятые на document_start). */
export interface PagePlayerFailureReaders {
  /** Подписаться на доверенные `error` самого элемента (не его `<source>`); вернуть отписку. */
  onElementError(element: object, listener: () => void): () => void;
  /**
   * У настоящего элемента сейчас ошибка воспроизведения (сеть или декодирование, оригинальные геттеры).
   * Ошибка «источник не годится» сюда не входит: так браузер отвечает и на `src = ""` страницы.
   */
  hasPlaybackFailureError(element: object): boolean;
  /** Монотонные часы окна, мс. */
  now(): number;
}

/** Сторож одной передачи (его зовёт контроллер; шов, чтобы тесты контроллера подставляли фейк). */
export interface PagePlayerFailureHooks {
  onClockTick(): PagePlayerFailureTickDecision;
  /** К элементу подключён новый MediaSource (новое поколение). */
  mediaSourceArrived(): void;
  /** Причина конца с учётом ошибки плеера страницы. */
  endCauseOf(reported: HandoffEndCause): HandoffEndCause;
  /** Передача кончилась: снять подписку. */
  stop(): void;
}

/** Поставить сторож на элемент MSE-передачи. */
export type PagePlayerFailureWatchStarter = (element: object) => PagePlayerFailureHooks;

export class PagePlayerFailureWatch implements PagePlayerFailureHooks {
  readonly #element: object;
  readonly #readers: PagePlayerFailureReaders;
  readonly #unsubscribe: () => void;
  /** Когда была последняя ошибка (`null` — ошибки нет или плеер оправился). */
  #lastErrorAtMs: number | null = null;
  /** Сторож уже решил, что плеер страницы мёртв: любой конец передачи — `page_player_failed`. */
  #declaredFailed = false;

  constructor(element: object, readers: PagePlayerFailureReaders) {
    this.#element = element;
    this.#readers = readers;
    this.#unsubscribe = readers.onElementError(element, () => this.#noteError());
    // Ошибка могла случиться до передачи: события уже не будет, а элемент уже сломан.
    if (this.#safeHasError()) this.#noteError();
  }

  onClockTick(): PagePlayerFailureTickDecision {
    const lastErrorAtMs = this.#lastErrorAtMs;
    if (lastErrorAtMs === null || this.#declaredFailed) return "keep_handoff";
    const sinceErrorMs = this.#readers.now() - lastErrorAtMs;
    const errorPersists = this.#safeHasError();
    if (errorPersists && sinceErrorMs >= PAGE_PLAYER_FAILURE_GRACE_MS) {
      this.#declaredFailed = true;
      return "end_for_page_player_failure";
    }
    // Ошибки уже нет, источник не брошен и окно вышло: страница что-то сделала и живёт дальше.
    if (!errorPersists && sinceErrorMs > PAGE_PLAYER_FAILURE_RELEASE_WINDOW_MS) this.#lastErrorAtMs = null;
    return "keep_handoff";
  }

  mediaSourceArrived(): void {
    // Новое поколение после ошибки = плеер страницы оправился. Решённое «мёртв» не отменяется: тень уже снята.
    if (!this.#declaredFailed) this.#lastErrorAtMs = null;
  }

  endCauseOf(reported: HandoffEndCause): HandoffEndCause {
    if (this.#declaredFailed) return "page_player_failed";
    if (reported !== "element_released" || this.#lastErrorAtMs === null) return reported;
    const sinceErrorMs = this.#readers.now() - this.#lastErrorAtMs;
    return sinceErrorMs <= PAGE_PLAYER_FAILURE_RELEASE_WINDOW_MS ? "page_player_failed" : reported;
  }

  stop(): void {
    this.#unsubscribe();
  }

  /**
   * Отсчёт идёт от ПОСЛЕДНЕЙ ошибки: новая ошибка после попытки оправиться — новый допуск. Событие `error`
   * без ошибки воспроизведения (страница убрала источник: `src = ""`) поломкой не считается.
   */
  #noteError(): void {
    if (this.#declaredFailed || !this.#safeHasError()) return;
    this.#lastErrorAtMs = this.#readers.now();
  }

  /** Чтение `error` на чужом «бренде» бросает: считаем, что ошибки нет (вывод «мёртв» требует доказательства). */
  #safeHasError(): boolean {
    try {
      return this.#readers.hasPlaybackFailureError(this.#element);
    } catch {
      return false;
    }
  }
}

/** Окно, с которого снимаются оригиналы событий сторожа. */
export interface FailureWatchWindow {
  readonly EventTarget: { readonly prototype: object };
  readonly Event: { readonly prototype: object };
}

/** Тип события ошибки медиаэлемента. */
const MEDIA_ERROR_EVENT_TYPE = "error";

/** Захват: `error` не всплывает, но слушатель захвата на самом элементе его слышит (фаза «на цели»). */
const LISTEN_IN_CAPTURE_PHASE = true;

/**
 * Собрать запуск сторожа на оригиналах, снятых СЕЙЧАС (зовётся на document_start). `null` — окружение без
 * нужных оригиналов (старый браузер, фейк теста): сторожа нет, передача работает как раньше.
 */
export function pagePlayerFailureWatchStarterFor(
  win: FailureWatchWindow,
  hasPlaybackFailureError: (element: object) => boolean,
  now: () => number,
): PagePlayerFailureWatchStarter | null {
  const addEventListener = ownOrInherited(win.EventTarget.prototype, "addEventListener", "value");
  const removeEventListener = ownOrInherited(win.EventTarget.prototype, "removeEventListener", "value");
  const eventTarget = ownOrInherited(win.Event.prototype, "target", "get");
  if (addEventListener === undefined || removeEventListener === undefined || eventTarget === undefined) return null;
  const readers: PagePlayerFailureReaders = {
    onElementError: (element, listener) => {
      const onError = (event: unknown): void => {
        // `isTrusted` — неподменяемое собственное свойство события: синтетическую ошибку страницы не считаем.
        if ((event as { isTrusted?: unknown }).isTrusted !== true) return;
        // Захват на элементе слышит и ошибки его дочерних `<source>`: нужны только ошибки самого элемента.
        if (Reflect.apply(eventTarget, event, []) !== element) return;
        listener();
      };
      Reflect.apply(addEventListener, element, [MEDIA_ERROR_EVENT_TYPE, onError, LISTEN_IN_CAPTURE_PHASE]);
      return () => void Reflect.apply(removeEventListener, element, [MEDIA_ERROR_EVENT_TYPE, onError, LISTEN_IN_CAPTURE_PHASE]);
    },
    hasPlaybackFailureError,
    now,
  };
  return (element) => new PagePlayerFailureWatch(element, readers);
}

function ownOrInherited(prototype: object, name: string, part: "get" | "value"): Function | undefined {
  let current: object | null = prototype;
  while (current !== null) {
    const descriptor = Object.getOwnPropertyDescriptor(current, name);
    const found: unknown = part === "get" ? descriptor?.get : descriptor?.value;
    if (typeof found === "function") return found;
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}
