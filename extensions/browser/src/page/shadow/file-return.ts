// Возврат файла `<video src>` странице (сессия 09b, решение 186): настоящему элементу уже вернули
// его источник (`srcObject = null`), он заново загружает файл; дальше — дождаться метаданных,
// поставить позицию T и дождаться `seeked`. Только после этого тень снимается, а страница получает
// элемент на T на паузе или в игре — без видимой ей перезагрузки (события загрузки гасит
// шлагбаум тени, пока возврат идёт).
//
// Этапы: `loading` (ждём `loadedmetadata`) → `seeking` (ждём `seeked`, если позицию пришлось
// ставить) → конец. Ошибка загрузки (`error`) и предел времени — тоже конец: тень снимается в
// любом случае, страница не остаётся навсегда под тенью без плеера.
//
// Браузер вправе вообще не качать файл до просьбы пользователя (`preload="none"`, находка 4 ревью
// 09b): тогда после `loadstart` он ставит `networkState = NETWORK_IDLE`, шлёт `suspend` и ждёт, а
// `loadedmetadata` не придёт никогда. Такой `suspend` при `readyState = HAVE_NOTHING` (после
// `loadstart` ЭТОЙ загрузки — не запоздавший от нашего пустого потока) значит «загрузку отложили»:
// позиция T пишется сразу — до метаданных это «позиция начала воспроизведения» по стандарту
// (`currentTime` её и возвращает, а браузер сам встанет на неё, когда загрузит файл), — и возврат
// кончается, не дожидаясь загрузки, которой не будет. Загрузку файла браузер начнёт потом сам (по
// `play()` возврата или страницы), и её события страница уже видит — как у любого элемента с
// `preload="none"`; скрыть их нечем, тень к тому мигу снята.
//
// Эфир HLS (10a3, решение 232) возвращается не на T: позиция `where_browser_joins_live` значит «ничего не
// писать» — после перезагрузки `src` нативный элемент сам встаёт в эфир (Chromium 156: на начало текущего
// окна плейлиста, `currentTime` от нуля его новой шкалы; разведка пакета D). Возврат кончается на
// `loadedmetadata` (у эфира Chromium шлёт его уже с readyState 4), а при отложенной загрузке — сразу.
//
// Предел времени — таймер окна. Таймеры скрытой вкладки Chromium будит раз в минуту, поэтому
// таймер здесь только страховка: обычный путь идёт по событиям элемента, а они не тормозятся.

import { type ElementOriginals, readOriginal, writeOriginal } from "./originals.ts";
import type { ReturnPosition } from "./shadow-time-source.ts";

/**
 * Сколько ждём, пока настоящий элемент заново загрузит файл и встанет на T. Метаданные файла с
 * поддержкой `Range` приходят за доли секунды (оглавление MP4 в начале файла); 10 с покрывают
 * медленную сеть и файл с оглавлением в конце, но не держат тень вечно.
 */
export const FILE_RETURN_TIMEOUT_MS = 10_000;

/**
 * Ставить позицию, только если элемент дальше 1 мс от цели (как SAME_POSITION_TOLERANCE_US тени):
 * лишняя перемотка — это лишний декод кадров.
 */
const SAME_POSITION_TOLERANCE_US = 1_000;

const MICROS_PER_SECOND = 1_000_000;

/** `HTMLMediaElement.HAVE_NOTHING`: метаданных нет. */
const HAVE_NOTHING = 0;
/** `HTMLMediaElement.NETWORK_IDLE`: браузер сейчас ничего не качает. */
const NETWORK_IDLE = 1;

/** Чем кончился возврат. */
export type FileReturnEnd =
  /** Элемент загрузил файл и стоит на T (эфир — там, где браузер вошёл в эфир). */
  | "restored"
  /** Браузер отложил загрузку (`preload="none"`): T — позиция начала воспроизведения элемента (у эфира — ничего). */
  | "start_position_set"
  /** Файл не загрузился (`error`): страница сама увидит ошибку элемента. */
  | "load_failed"
  /** Не уложились в FILE_RETURN_TIMEOUT_MS. */
  | "timed_out";

/** Отложенный вызов окна; возвращает «отменить». */
export type TimeoutScheduler = (callback: () => void, delayMs: number) => () => void;

export interface FileReturnParameters {
  readonly element: HTMLMediaElement;
  readonly originals: ElementOriginals;
  /** Куда встать после загрузки. */
  readonly position: ReturnPosition;
  readonly scheduleTimeout: TimeoutScheduler;
  readonly onFinished: (end: FileReturnEnd) => void;
}

type FileReturnStage = "loading" | "seeking" | "finished";

export class FileReturn {
  readonly #parameters: FileReturnParameters;
  readonly #cancelTimeout: () => void;
  #stage: FileReturnStage = "loading";
  /** Пришёл `loadstart` загрузки возвращённого файла (раньше него `suspend` — чужой, старый). */
  #loadStarted = false;

  constructor(parameters: FileReturnParameters) {
    this.#parameters = parameters;
    this.#cancelTimeout = parameters.scheduleTimeout(() => this.#finish("timed_out"), FILE_RETURN_TIMEOUT_MS);
  }

  get isFinished(): boolean {
    return this.#stage === "finished";
  }

  /** Доверенное событие настоящего элемента, пока возврат идёт. */
  onRealEvent(type: string): void {
    if (this.#stage === "finished") return;
    if (type === "error") {
      this.#finish("load_failed");
      return;
    }
    if (this.#stage === "loading" && type === "loadstart") {
      this.#loadStarted = true;
      return;
    }
    if (this.#stage === "loading" && type === "loadedmetadata") {
      this.#placeAtTarget();
      return;
    }
    if (this.#stage === "loading" && type === "suspend" && this.#loadStarted && this.#browserDeferredTheFetch()) {
      this.#placeBeforeMetadata();
      return;
    }
    if (this.#stage === "seeking" && type === "seeked") this.#finish("restored");
  }

  /** Возврат больше не нужен (страница сама сменила источник): без итога, таймер снят. */
  abandon(): void {
    if (this.#stage === "finished") return;
    this.#stage = "finished";
    this.#cancelTimeout();
  }

  #placeAtTarget(): void {
    const { element, originals, position } = this.#parameters;
    // Эфир: браузер уже сам встал в эфир, T не пишется (см. шапку).
    if (position.kind === "where_browser_joins_live") {
      this.#finish("restored");
      return;
    }
    const targetUs = position.targetUs;
    const realUs = Number(readOriginal(originals.media.currentTime, element)) * MICROS_PER_SECOND;
    if (Math.abs(realUs - targetUs) <= SAME_POSITION_TOLERANCE_US) {
      this.#finish("restored");
      return;
    }
    this.#stage = "seeking";
    writeOriginal(originals.media.currentTime, element, targetUs / MICROS_PER_SECOND);
  }

  /** `suspend` без метаданных и без сети: браузер ждёт просьбы пользователя (`preload="none"`). */
  #browserDeferredTheFetch(): boolean {
    const { element, originals } = this.#parameters;
    const readyState = Number(readOriginal(originals.media.readyState, element));
    const networkState = Number(readOriginal(originals.media.networkState, element));
    return readyState === HAVE_NOTHING && networkState === NETWORK_IDLE;
  }

  /** До метаданных запись `currentTime` — позиция начала воспроизведения (без перемотки и событий). */
  #placeBeforeMetadata(): void {
    const { element, originals, position } = this.#parameters;
    if (position.kind === "at") writeOriginal(originals.media.currentTime, element, position.targetUs / MICROS_PER_SECOND);
    this.#finish("start_position_set");
  }

  #finish(end: FileReturnEnd): void {
    if (this.#stage === "finished") return;
    this.#stage = "finished";
    this.#cancelTimeout();
    this.#parameters.onFinished(end);
  }
}
