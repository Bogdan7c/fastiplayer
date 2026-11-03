// Поддельные счётчики кадров тени: `getVideoPlaybackQuality`, `webkit*FrameCount` (Chromium)
// и `requestVideoFrameCallback`.
//
// Зачем: настоящий элемент в тени стоит и кадров не показывает. Плееры страниц смотрят на эти
// счётчики (доля выброшенных кадров для выбора качества, «видео идёт?» для сторожей зависания),
// и замершие счётчики выглядели бы как поломка. Тень считает кадры от проигранного медиавремени.

import { callOriginal, type ElementOriginals, readOriginal } from "./originals.ts";

/**
 * Кадров в секунду для поддельных счётчиков. Настоящую частоту MAIN-мир не знает (её знает
 * разбор потока в плеере), а страницам важны рост счётчика и доля выброшенных (у тени — 0),
 * а не точное число. 30 к/с — самая частая частота роликов.
 */
export const ASSUMED_FRAMES_PER_SECOND = 30;

/**
 * Номера `requestVideoFrameCallback` тени начинаются отсюда, чтобы не совпасть с номерами,
 * которые браузер выдал настоящему элементу до передачи (они идут с 1). Тогда
 * `cancelVideoFrameCallback` однозначно понимает, чей номер отменяют.
 */
export const SHADOW_FRAME_CALLBACK_FIRST_HANDLE = 1_000_000_000;

const MICROS_PER_SECOND = 1_000_000;

/** Счётчики настоящего элемента и прототип его отчёта (поддельный отчёт тени проходит `instanceof`). */
export interface RealFrameQuality {
  readonly baseline: FrameCountBaseline;
  readonly prototype: object;
}

/** Прочитать счётчики настоящего элемента оригиналами (на старте тени). */
export function readRealFrameQuality(element: HTMLMediaElement, originals: ElementOriginals): RealFrameQuality {
  const getQuality = originals.video.getVideoPlaybackQuality;
  if (getQuality !== undefined) {
    const quality = callOriginal(getQuality, element) as VideoPlaybackQuality;
    return {
      baseline: {
        totalFrames: quality.totalVideoFrames,
        droppedFrames: quality.droppedVideoFrames,
        corruptedFrames: quality.corruptedVideoFrames,
      },
      prototype: Object.getPrototypeOf(quality) as object,
    };
  }
  const decoded = originals.video.webkitDecodedFrameCount;
  const dropped = originals.video.webkitDroppedFrameCount;
  return {
    baseline: {
      totalFrames: decoded === undefined ? 0 : Number(readOriginal(decoded, element)),
      droppedFrames: dropped === undefined ? 0 : Number(readOriginal(dropped, element)),
      corruptedFrames: 0,
    },
    prototype: Object.prototype,
  };
}

/** Счётчики настоящего элемента на момент передачи: тень продолжает их, а не начинает с нуля. */
export interface FrameCountBaseline {
  readonly totalFrames: number;
  readonly droppedFrames: number;
  readonly corruptedFrames: number;
}

/** Сколько кадров «показала» тень всего, с учётом настоящих до передачи. */
export function shadowTotalFrames(baseline: FrameCountBaseline, playedUs: number): number {
  return baseline.totalFrames + Math.floor((playedUs * ASSUMED_FRAMES_PER_SECOND) / MICROS_PER_SECOND);
}

/** Поля `VideoFrameCallbackMetadata`, которые тень может честно заполнить. */
export interface ShadowFrameMetadata {
  readonly presentationTime: number;
  readonly expectedDisplayTime: number;
  readonly width: number;
  readonly height: number;
  readonly mediaTime: number;
  readonly presentedFrames: number;
  readonly processingDuration: number;
}

/** Колбэк кадра, как его передаёт страница. */
export type FrameCallback = (now: number, metadata: ShadowFrameMetadata) => void;

/**
 * Очередь `requestVideoFrameCallback` тени. Как у браузера, колбэк одноразовый: страница,
 * которой нужен каждый кадр, просит снова изнутри колбэка (такие просьбы ждут следующего кадра).
 */
export class ShadowFrameCallbacks {
  readonly #pending = new Map<number, FrameCallback>();
  #nextHandle = SHADOW_FRAME_CALLBACK_FIRST_HANDLE;

  request(callback: FrameCallback): number {
    const handle = this.#nextHandle;
    this.#nextHandle += 1;
    this.#pending.set(handle, callback);
    return handle;
  }

  /** Отменить свой колбэк; `false` — номер не наш (его выдал браузер настоящему элементу). */
  cancel(handle: number): boolean {
    return this.#pending.delete(handle);
  }

  /**
   * «Кадр показан»: отдать все ожидающие колбэки. Ошибка одного колбэка не мешает другим
   * и уходит в `reportError` — как у браузера, где исключение колбэка лишь печатается.
   */
  presentFrame(now: number, metadata: ShadowFrameMetadata, reportError: (problem: unknown) => void): void {
    if (this.#pending.size === 0) return;
    const due = [...this.#pending.values()];
    this.#pending.clear();
    for (const callback of due) {
      try {
        callback(now, metadata);
      } catch (problem) {
        reportError(problem);
      }
    }
  }

  /** Снятие тени: ожидающие колбэки уходят настоящему элементу, чтобы цикл страницы не оборвался. */
  drainInto(requestOnRealElement: (callback: FrameCallback) => void): void {
    const due = [...this.#pending.values()];
    this.#pending.clear();
    for (const callback of due) requestOnRealElement(callback);
  }
}
