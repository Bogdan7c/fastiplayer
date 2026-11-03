// Запись операций MSE страницы (главный поток и выделенный воркер): обёртки поверх хука моста.
//
// Ставятся ПОСЛЕ `installMseHook`: поздняя обёртка снаружи, поэтому вызов страницы сначала идёт в
// оригинал через хук моста (трекер уже всё учёл), затем сюда. Обёртки для `mode`,
// `appendWindowStart/End`, `removeSourceBuffer`, `setLiveSeekableRange`, событий `MediaSource` и
// `updateend` существуют ТОЛЬКО в записывающей сборке: продуктовое поведение не меняется.
//
// Что пишется — см. `format.ts` и бриф сессии 09b2: каждый вызов страницы над MediaSource /
// SourceBuffer, события `sourceopen/ended/close` и `updateend`, снимки `buffered`, байты `append` с
// копией в момент вызова, сбой `appendBuffer` (`append_error`).

import { viewOfAppendSource, type BinaryIntrinsics } from "../binary-view.ts";
import { isObject, observeSafely, wrapGetter, wrapMethod, wrapSetter, type OddityReporter } from "../prototype-wrap.ts";
import { jsonNumber } from "./format.ts";
import type { RecordedBuffer, StreamRecorder } from "./recorder.ts";
import { wrapMethodWithOutcome } from "./wrap-extras.ts";

/** Окно (или глобал воркера) с MSE; всё необязательное, чего нет — пропускается. */
export interface MseRecordingScope {
  readonly MediaSource?: { readonly prototype: object };
  readonly SourceBuffer?: { readonly prototype: object };
  readonly URL?: object;
  readonly EventTarget?: { readonly prototype: object };
  readonly setInterval?: (callback: () => void, delayMs: number) => unknown;
}

/** Как найти ключ и индекс, под которыми мост учитывает объект (трекер хука). */
export interface TrackerIdentity {
  keyOf(mediaSource: object): string | null;
  indexOf(sourceBuffer: object): number | null;
}

/** Зависимости установки. */
export interface MseRecordingDependencies {
  readonly recorder: StreamRecorder;
  readonly identity: TrackerIdentity;
  readonly binary: BinaryIntrinsics;
  readonly isMediaSource: (candidate: unknown) => boolean;
  readonly reportOddity: OddityReporter;
}

/** Как часто снимается `buffered` (мс). */
const BUFFERED_SNAPSHOT_INTERVAL_MS = 1000;

type EventListenerAdder = (target: object, type: string, listener: () => void) => void;

/** Снять `addEventListener` прототипа EventTarget: свои слушатели ставим только им. */
function captureListenerAdder(scope: MseRecordingScope, reportOddity: OddityReporter): EventListenerAdder | null {
  const add: unknown = scope.EventTarget === undefined ? undefined : Object.getOwnPropertyDescriptor(scope.EventTarget.prototype, "addEventListener")?.value;
  if (typeof add !== "function") return null;
  return (target, type, listener) => {
    try {
      Reflect.apply(add, target, [type, listener]);
    } catch {
      // Объект не EventTarget (так не бывает у настоящих MediaSource/SourceBuffer): события не будет,
      // но остальные записи не должны от этого пропасть.
      reportOddity(`stream recorder: cannot listen to ${type}`);
    }
  };
}

/** Поставить запись MSE. `false` — в окне нет MSE (ничего не тронуто). */
export function installMseRecording(scope: MseRecordingScope, dependencies: MseRecordingDependencies): boolean {
  const mediaSourcePrototype = scope.MediaSource?.prototype;
  const sourceBufferPrototype = scope.SourceBuffer?.prototype;
  if (mediaSourcePrototype === undefined || sourceBufferPrototype === undefined) return false;
  const { recorder, identity, binary, reportOddity } = dependencies;
  const addListener = captureListenerAdder(scope, reportOddity);
  const bufferedGetter = Object.getOwnPropertyDescriptor(sourceBufferPrototype, "buffered")?.get;
  const watcher = new BufferedWatcher(recorder, bufferedGetter);

  /** Идентификатор MediaSource; при первой встрече пишет `media_source_created` и вешает слушатели событий. */
  const mediaSourceId = (mediaSource: object): string => {
    const issued = recorder.mediaSourceIdOf(mediaSource);
    if (!issued.isNew) return issued.id;
    recorder.record("media_source_created", { ms: issued.id });
    if (addListener !== null) {
      for (const [eventType, op] of [["sourceopen", "source_open"], ["sourceended", "source_ended"], ["sourceclose", "source_close"]] as const) {
        addListener(mediaSource, eventType, () => observeSafely(reportOddity, eventType, () => recorder.record(op, { ms: issued.id })));
      }
    }
    return issued.id;
  };

  /** Идентификаторы SourceBuffer; первый раз вешает `updateend` и ставит на периодический снимок `buffered`. */
  const bufferOf = (sourceBuffer: object, owner: object | null): RecordedBuffer | undefined => {
    const known = recorder.knownSourceBuffer(sourceBuffer);
    if (known !== undefined) return known;
    if (owner === null) return undefined;
    const issued = recorder.sourceBufferIdOf(sourceBuffer, mediaSourceId(owner));
    if (issued === undefined) return undefined;
    const { buffer } = issued;
    if (addListener !== null) {
      addListener(sourceBuffer, "updateend", () =>
        observeSafely(reportOddity, "updateend", () => {
          recorder.record("update_end", { ms: buffer.ms, sb: buffer.sb });
          watcher.snapshot(sourceBuffer, buffer);
        }),
      );
    }
    watcher.watch(sourceBuffer, buffer);
    return buffer;
  };

  installMediaSourceConstructor(scope, mediaSourceId, reportOddity);
  installUrlRecording(scope, recorder, dependencies.isMediaSource, mediaSourceId, reportOddity);

  // ---- MediaSource
  wrapMethod(mediaSourcePrototype, "addSourceBuffer", reportOddity, (mediaSource, args, sourceBuffer) => {
    if (!isObject(mediaSource) || !isObject(sourceBuffer)) return;
    const buffer = bufferOf(sourceBuffer, mediaSource);
    if (buffer === undefined) return;
    recorder.record("add_source_buffer", { ms: buffer.ms, sb: buffer.sb, mime: String(args[0]) });
    // Хук моста уже учёл буфер (мы снаружи него): связываем его ключ учёта с нашими идентификаторами.
    const key = identity.keyOf(mediaSource);
    const index = identity.indexOf(sourceBuffer);
    if (key !== null && index !== null) recorder.bindTrackerBuffer(key, index, buffer);
  });
  wrapMethod(mediaSourcePrototype, "removeSourceBuffer", reportOddity, (mediaSource, args) => {
    const sourceBuffer = args[0];
    if (!isObject(mediaSource) || !isObject(sourceBuffer)) return;
    const buffer = bufferOf(sourceBuffer, mediaSource);
    if (buffer !== undefined) recorder.record("remove_source_buffer", { ms: buffer.ms, sb: buffer.sb });
  });
  wrapMethod(mediaSourcePrototype, "endOfStream", reportOddity, (mediaSource, args) => {
    if (!isObject(mediaSource)) return;
    const ms = mediaSourceId(mediaSource);
    recorder.record("end_of_stream", args[0] === undefined ? { ms } : { ms, error: String(args[0]) });
  });
  wrapSetter(mediaSourcePrototype, "duration", reportOddity, (mediaSource, seconds) => {
    if (isObject(mediaSource) && typeof seconds === "number") recorder.record("duration", { ms: mediaSourceId(mediaSource), value: jsonNumber(seconds) });
  });
  wrapMethod(mediaSourcePrototype, "setLiveSeekableRange", reportOddity, (mediaSource, args) => {
    if (isObject(mediaSource)) recorder.record("live_seekable_range", { ms: mediaSourceId(mediaSource), start: jsonNumber(Number(args[0])), end: jsonNumber(Number(args[1])) });
  });
  wrapMethod(mediaSourcePrototype, "clearLiveSeekableRange", reportOddity, (mediaSource) => {
    if (isObject(mediaSource)) recorder.record("live_seekable_range_cleared", { ms: mediaSourceId(mediaSource) });
  });
  // `handle` есть только в воркере (Chromium): запись раз на MediaSource, когда сайт взял handle.
  const handled = new WeakSet<object>();
  wrapGetter(mediaSourcePrototype, "handle", reportOddity, (mediaSource) => {
    if (!isObject(mediaSource) || handled.has(mediaSource)) return;
    handled.add(mediaSource);
    recorder.record("media_source_handle", { ms: mediaSourceId(mediaSource) });
  });

  // ---- SourceBuffer
  wrapMethodWithOutcome(sourceBufferPrototype, "appendBuffer", reportOddity, {
    onReturned: (sourceBuffer, args) => {
      const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
      // Вид на память страницы: копию на склад берёт `recordAppend` прямо сейчас.
      const view = viewOfAppendSource(binary, args[0]);
      if (buffer !== undefined && view !== null) recorder.recordAppend(buffer, view);
    },
    onThrown: (sourceBuffer, _args, error) => {
      const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
      if (buffer !== undefined) recorder.record("append_error", { ms: buffer.ms, sb: buffer.sb, error: describeError(error) });
    },
  });
  wrapMethod(sourceBufferPrototype, "remove", reportOddity, (sourceBuffer, args) => {
    const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
    if (buffer !== undefined) recorder.record("remove", { ms: buffer.ms, sb: buffer.sb, start: jsonNumber(Number(args[0])), end: jsonNumber(Number(args[1])) });
  });
  wrapMethod(sourceBufferPrototype, "abort", reportOddity, (sourceBuffer) => {
    const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
    if (buffer !== undefined) recorder.record("abort", { ms: buffer.ms, sb: buffer.sb });
  });
  wrapMethod(sourceBufferPrototype, "changeType", reportOddity, (sourceBuffer, args) => {
    const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
    if (buffer !== undefined) recorder.record("change_type", { ms: buffer.ms, sb: buffer.sb, mime: String(args[0]) });
  });
  const numericSetters = [
    ["timestampOffset", "timestamp_offset"],
    ["appendWindowStart", "append_window_start"],
    ["appendWindowEnd", "append_window_end"],
  ] as const;
  for (const [property, op] of numericSetters) {
    wrapSetter(sourceBufferPrototype, property, reportOddity, (sourceBuffer, value) => {
      const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
      if (buffer !== undefined && typeof value === "number") recorder.record(op, { ms: buffer.ms, sb: buffer.sb, value: jsonNumber(value) });
    });
  }
  wrapSetter(sourceBufferPrototype, "mode", reportOddity, (sourceBuffer, value) => {
    const buffer = isObject(sourceBuffer) ? bufferOf(sourceBuffer, null) : undefined;
    if (buffer !== undefined && typeof value === "string") recorder.record("mode", { ms: buffer.ms, sb: buffer.sb, value });
  });

  watcher.start(scope);
  return true;
}

/** Исключение оригинала одной строкой: «Имя: сообщение». */
function describeError(error: unknown): string {
  if (!isObject(error)) return String(error);
  const { name, message } = error as { readonly name?: unknown; readonly message?: unknown };
  return `${typeof name === "string" ? name : "Error"}: ${typeof message === "string" ? message : ""}`;
}

/**
 * Конструктор `MediaSource` заменяется прокси: так `media_source_created` пишется в момент
 * `new MediaSource()`, а не при первом вызове метода. Прототип и статика (`isTypeSupported`,
 * `canConstructInDedicatedWorker`) остаются теми же. Не вышло подменить — MediaSource увидим по
 * первому вызову (запись останется верной, потеряется лишь точное время создания).
 */
function installMediaSourceConstructor(scope: MseRecordingScope, mediaSourceId: (mediaSource: object) => string, reportOddity: OddityReporter): void {
  const Original = scope.MediaSource;
  if (typeof Original !== "function") return;
  const descriptor = Object.getOwnPropertyDescriptor(scope, "MediaSource");
  if (descriptor === undefined || descriptor.configurable !== true) {
    reportOddity("MediaSource constructor cannot be replaced; creation time is recorded lazily");
    return;
  }
  const proxy: unknown = new Proxy(Original as new (...args: unknown[]) => object, {
    construct(target, args, newTarget) {
      const instance = Reflect.construct(target, args, newTarget === proxy ? target : newTarget);
      observeSafely(reportOddity, "MediaSource", () => mediaSourceId(instance));
      return instance;
    },
  });
  Object.defineProperty(scope, "MediaSource", { ...descriptor, ...("value" in descriptor ? { value: proxy } : {}) });
}

/** `URL.createObjectURL(mediaSource)` → `media_source_object_url` и запоминание адреса для `element_source`. */
function installUrlRecording(
  scope: MseRecordingScope,
  recorder: StreamRecorder,
  isMediaSource: (candidate: unknown) => boolean,
  mediaSourceId: (mediaSource: object) => string,
  reportOddity: OddityReporter,
): void {
  if (scope.URL === undefined) return;
  wrapMethod(scope.URL, "createObjectURL", reportOddity, (_url, args, result) => {
    const subject = args[0];
    if (!isObject(subject) || !isMediaSource(subject) || typeof result !== "string") return;
    const ms = mediaSourceId(subject);
    recorder.rememberObjectUrl(result, ms);
    recorder.record("media_source_object_url", { ms });
  });
}

/**
 * Снимки `buffered` настоящего SourceBuffer: после каждого `updateend` и раз в секунду, если
 * изменились. Читает оригинальным геттером; живые SourceBuffer держит слабыми ссылками.
 */
class BufferedWatcher {
  readonly #recorder: StreamRecorder;
  readonly #getter: ((this: object) => unknown) | undefined;
  readonly #watched: { readonly reference: WeakRef<object>; readonly buffer: RecordedBuffer; lastSnapshot: string }[] = [];

  constructor(recorder: StreamRecorder, getter: ((this: object) => unknown) | undefined) {
    this.#recorder = recorder;
    this.#getter = getter;
  }

  watch(sourceBuffer: object, buffer: RecordedBuffer): void {
    this.#watched.push({ reference: new WeakRef(sourceBuffer), buffer, lastSnapshot: "" });
  }

  /** Раз в секунду; таймер — оригинал, снятый до страницы. Нет таймера — только по `updateend`. */
  start(scope: MseRecordingScope): void {
    scope.setInterval?.(() => {
      for (const entry of [...this.#watched]) {
        const sourceBuffer = entry.reference.deref();
        if (sourceBuffer === undefined) this.#watched.splice(this.#watched.indexOf(entry), 1);
        else this.#emitIfChanged(sourceBuffer, entry);
      }
    }, BUFFERED_SNAPSHOT_INTERVAL_MS);
  }

  snapshot(sourceBuffer: object, buffer: RecordedBuffer): void {
    const entry = this.#watched.find((candidate) => candidate.buffer === buffer);
    if (entry !== undefined) this.#emitIfChanged(sourceBuffer, entry);
  }

  #emitIfChanged(sourceBuffer: object, entry: { readonly buffer: RecordedBuffer; lastSnapshot: string }): void {
    const ranges = this.#readRanges(sourceBuffer);
    if (ranges === null) return;
    const fingerprint = JSON.stringify(ranges);
    if (fingerprint === entry.lastSnapshot) return;
    entry.lastSnapshot = fingerprint;
    this.#recorder.record("buffered", { ms: entry.buffer.ms, sb: entry.buffer.sb, ranges });
  }

  /** Диапазоны `[[start, end], …]`; `null` — прочитать нельзя (буфер уже снят с MediaSource). */
  #readRanges(sourceBuffer: object): number[][] | null {
    if (this.#getter === undefined) return null;
    try {
      const timeRanges = Reflect.apply(this.#getter, sourceBuffer, []) as { readonly length: number; start(index: number): number; end(index: number): number };
      const ranges: number[][] = [];
      for (let index = 0; index < timeRanges.length; index += 1) ranges.push([timeRanges.start(index), timeRanges.end(index)]);
      return ranges;
    } catch {
      // SourceBuffer снят с MediaSource: чтение бросает InvalidStateError — это штатно, не странность.
      return null;
    }
  }
}
