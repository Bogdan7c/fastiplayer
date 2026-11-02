// Хук MSE главного потока (пункт 1 задания 09a): обёртки методов и сеттеров на прототипах
// `MediaSource` и `SourceBuffer`. Правила обёрток (оригинал первым, только успешные вызовы,
// никаких исключений из хука, сосуществование с чужими подменами) — в `prototype-wrap.ts`.
//
// Пока передачи нет, обёртка `appendBuffer` только создаёт вид на байты (без копии) и отдаёт его
// трекеру — дальше журнал запоминает ссылку, а сканер читает заголовки.

import { viewOfAppendSource, type BinaryIntrinsics } from "./binary-view.ts";
import type { MseObserver } from "./mse-tracker.ts";
import { isObject, wrapMethod, wrapSetter, type OddityReporter } from "./prototype-wrap.ts";

/** Окно с MSE (в тестах — фейк). Без `MediaSource` хук не ставится. */
export interface MseWindow {
  readonly MediaSource?: { readonly prototype: object };
  readonly SourceBuffer?: { readonly prototype: object };
}

/** Окружение установки хука. */
export interface MseHookEnvironment {
  readonly observer: MseObserver;
  readonly binary: BinaryIntrinsics;
  readonly reportOddity: OddityReporter;
}

/** Поставить хук MSE. `false` — в окне нет MSE (тогда ничего не трогаем). */
export function installMseHook(win: MseWindow, environment: MseHookEnvironment): boolean {
  const mediaSourcePrototype = win.MediaSource?.prototype;
  const sourceBufferPrototype = win.SourceBuffer?.prototype;
  if (mediaSourcePrototype === undefined || sourceBufferPrototype === undefined) return false;
  const { observer, reportOddity } = environment;
  // Геттер длительности снимаем до обёрток: после endOfStream() читаем, что поставил браузер.
  const durationGetter = Object.getOwnPropertyDescriptor(mediaSourcePrototype, "duration")?.get;
  const durationAfterCall = (mediaSource: object): number | null => {
    if (durationGetter === undefined) return null;
    const seconds: unknown = Reflect.apply(durationGetter, mediaSource, []);
    return typeof seconds === "number" ? seconds : null;
  };

  const installed = [
    wrapMethod(mediaSourcePrototype, "addSourceBuffer", reportOddity, (mediaSource, args, sourceBuffer) => {
      if (isObject(mediaSource) && isObject(sourceBuffer)) observer.sourceBufferAdded(mediaSource, sourceBuffer, args[0]);
    }),
    wrapMethod(mediaSourcePrototype, "endOfStream", reportOddity, (mediaSource, args) => {
      if (isObject(mediaSource)) observer.streamEnded(mediaSource, args[0], durationAfterCall(mediaSource));
    }),
    wrapSetter(mediaSourcePrototype, "duration", reportOddity, (mediaSource, seconds) => {
      if (isObject(mediaSource) && typeof seconds === "number") observer.durationChanged(mediaSource, seconds);
    }),
    wrapMethod(sourceBufferPrototype, "appendBuffer", reportOddity, (sourceBuffer, args) => {
      if (!isObject(sourceBuffer)) return;
      // Вид на память страницы, НЕ копия (решение 176).
      const view = viewOfAppendSource(environment.binary, args[0]);
      if (view !== null) observer.bytesAppended(sourceBuffer, view);
    }),
    wrapMethod(sourceBufferPrototype, "remove", reportOddity, (sourceBuffer, args) => {
      if (isObject(sourceBuffer)) observer.rangeRemoved(sourceBuffer, args[0], args[1]);
    }),
    wrapMethod(sourceBufferPrototype, "abort", reportOddity, (sourceBuffer) => {
      if (isObject(sourceBuffer)) observer.sourceBufferAborted(sourceBuffer);
    }),
    wrapMethod(sourceBufferPrototype, "changeType", reportOddity, (sourceBuffer, args) => {
      if (isObject(sourceBuffer)) observer.sourceBufferTypeChanged(sourceBuffer, args[0]);
    }),
    wrapSetter(sourceBufferPrototype, "timestampOffset", reportOddity, (sourceBuffer, seconds) => {
      if (isObject(sourceBuffer) && typeof seconds === "number") observer.timestampOffsetChanged(sourceBuffer, seconds);
    }),
  ];
  // changeType нет в старых браузерах — это не сбой, но о неполном хуке стоит знать.
  if (installed.some((done) => !done)) reportOddity("some mse methods are missing; hook is partial");
  return true;
}
