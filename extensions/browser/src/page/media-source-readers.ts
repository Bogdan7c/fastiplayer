// Чтение состояния MediaSource оригинальным геттером `readyState`, снятым на document_start.
// Тот же геттер — проверка «это MediaSource?» без `instanceof`: на чужом объекте он бросает.

/** Окно с MediaSource (в тестах — фейк). */
export interface MediaSourceWindow {
  readonly MediaSource?: { readonly prototype: object };
}

export interface MediaSourceReaders {
  /** Объект — настоящий MediaSource. */
  isMediaSource(candidate: unknown): boolean;
  /** `readyState` ("closed" | "open" | "ended"); `null` — прочитать не вышло. */
  readyStateOf(mediaSource: object): string | null;
}

/** Снять геттер `readyState`. Без MediaSource в окне — читатели, которые ничего не узнают. */
export function captureMediaSourceReaders(win: MediaSourceWindow): MediaSourceReaders {
  const prototype = win.MediaSource?.prototype;
  const getter = prototype === undefined ? undefined : Object.getOwnPropertyDescriptor(prototype, "readyState")?.get;
  const read = (target: unknown): string | null => {
    if (getter === undefined) return null;
    try {
      const state: unknown = Reflect.apply(getter, target, []);
      return typeof state === "string" ? state : null;
    } catch {
      return null;
    }
  };
  return {
    isMediaSource: (candidate) => read(candidate) !== null,
    readyStateOf: (mediaSource) => read(mediaSource),
  };
}
