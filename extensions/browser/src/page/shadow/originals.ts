// Оригиналы свойств и методов видеоэлемента, снятые на document_start (до скриптов страницы).
//
// Тень управляет НАСТОЯЩИМ элементом (пауза, заглушка, подтягивание позиции, возврат) только
// через эти оригиналы: страница или блокировщик могут позже подменить прототипы, и тогда вызов
// «через свойство» попал бы в чужую подмену или в нашу же тень. Зовём через `applyOriginal`.

import { applyOriginal } from "./intrinsics.ts";

/** Функция-оригинал: зовётся только через `callOriginal` с явным `this`. */
export type OriginalFunction = (...args: never[]) => unknown;

/** Пара «геттер/сеттер» свойства-аксессора прототипа. */
export interface OriginalAccessor {
  readonly get: OriginalFunction;
  /** Нет у свойств только для чтения (`paused`, `readyState`, …). */
  readonly set: OriginalFunction | undefined;
}

/** Что тень берёт из глобала окна (в тестах — фейковое окно с тем же составом). */
export interface PageGlobals {
  readonly HTMLMediaElement: { readonly prototype: object };
  readonly HTMLVideoElement?: { readonly prototype: object };
  readonly EventTarget: { readonly prototype: object };
  readonly Event: { readonly prototype: object; new (type: string, init?: EventInit): Event };
  readonly Node: { readonly prototype: object };
  readonly Element?: { readonly prototype: object };
  readonly Promise: PromiseConstructor;
  readonly DOMException: { new (message?: string, name?: string): DOMException };
  readonly TypeError: TypeErrorConstructor;
  readonly MediaSource?: { readonly prototype: object };
  readonly MediaSourceHandle?: { readonly prototype: object };
  /** Пустой MediaStream снимает файл с настоящего элемента (решение 186); нет — источник остаётся. */
  readonly MediaStream?: new () => object;
  readonly TimeRanges?: { readonly prototype: object };
  readonly queueMicrotask: (callback: () => void) => void;
}

/** Оригиналы `HTMLMediaElement.prototype`, нужные тени. */
export interface MediaElementOriginals {
  readonly currentTime: OriginalAccessor;
  readonly paused: OriginalAccessor;
  readonly playbackRate: OriginalAccessor;
  readonly muted: OriginalAccessor;
  readonly volume: OriginalAccessor;
  readonly readyState: OriginalAccessor;
  readonly duration: OriginalAccessor;
  readonly src: OriginalAccessor;
  readonly srcObject: OriginalAccessor | undefined;
  readonly networkState: OriginalAccessor;
  /** Нужны тени файла (сессия 09b): снимок перед снятием источника. */
  readonly currentSrc: OriginalAccessor | undefined;
  readonly seekable: OriginalAccessor | undefined;
  readonly play: OriginalFunction;
  readonly pause: OriginalFunction;
  readonly load: OriginalFunction;
}

/** Оригиналы `HTMLVideoElement.prototype`; чего нет у браузера — `undefined`. */
export interface VideoElementOriginals {
  readonly videoWidth: OriginalAccessor | undefined;
  readonly videoHeight: OriginalAccessor | undefined;
  readonly getVideoPlaybackQuality: OriginalFunction | undefined;
  readonly requestVideoFrameCallback: OriginalFunction | undefined;
  readonly cancelVideoFrameCallback: OriginalFunction | undefined;
  /** Только Chromium. */
  readonly webkitDecodedFrameCount: OriginalAccessor | undefined;
  /** Только Chromium. */
  readonly webkitDroppedFrameCount: OriginalAccessor | undefined;
}

/** Оригиналы событийной части DOM: свои слушатели и свои события тень ставит только ими. */
export interface EventOriginals {
  readonly addEventListener: OriginalFunction;
  readonly removeEventListener: OriginalFunction;
  readonly dispatchEvent: OriginalFunction;
  /** Геттер `Event.prototype.target`. */
  readonly eventTarget: OriginalFunction;
  readonly stopImmediatePropagation: OriginalFunction;
  readonly getRootNode: OriginalFunction;
  readonly EventConstructor: PageGlobals["Event"];
}

/** Всё, что тень сняла с окна на document_start. Состав решает тень (шов A1↔A2). */
export interface ElementOriginals {
  /** Прототипы, на которые тень на document_start ставит свои переопределения (решение 181). */
  readonly mediaPrototype: object;
  readonly videoPrototype: object | undefined;
  readonly media: MediaElementOriginals;
  readonly video: VideoElementOriginals;
  readonly events: EventOriginals;
  /** Геттер `Node.prototype.isConnected`: убран ли переданный элемент из документа (нет в очень старых браузерах). */
  readonly isConnected: OriginalAccessor | undefined;
  /**
   * `Element.prototype.getAttribute`: есть ли у элемента атрибут `src` и пуст ли он. Геттер `src`
   * этого не скажет: для `src=""` он отдаёт адрес документа (URL-отражение атрибута).
   */
  readonly getAttribute: OriginalFunction | undefined;
  /**
   * Дочерние элементы и их имя (`Element.prototype.children` / `localName`): источник из дочерних
   * `<source>` виден только по ним (у элемента тогда нет атрибута `src`).
   */
  readonly elementChildren: OriginalAccessor | undefined;
  readonly localName: OriginalAccessor | undefined;
  /** Конструктор MediaStream мира страницы на document_start (`undefined` — браузер его не знает). */
  readonly MediaStreamConstructor: (new () => object) | undefined;
  /** Прототип TimeRanges: `buffered` тени файла проходит `instanceof TimeRanges`. */
  readonly timeRangesPrototype: object | undefined;
  /** Конструкторы мира страницы: обещания `play()` и ошибки должны быть «её», а не наши. */
  readonly PromiseConstructor: PromiseConstructor;
  readonly DOMExceptionConstructor: PageGlobals["DOMException"];
  readonly TypeErrorConstructor: TypeErrorConstructor;
  /** Прототипы источников MSE — чтобы отличить «новый MediaSource» от «элемент брошен». */
  readonly mediaSourcePrototypes: readonly object[];
  readonly queueMicrotask: (callback: () => void) => void;
}

/** Вызвать оригинал с явным `this` (страница могла подменить сам метод на прототипе). */
export function callOriginal(fn: OriginalFunction, receiver: unknown, args: readonly unknown[] = []): unknown {
  return applyOriginal(fn as (...args: unknown[]) => unknown, receiver, args);
}

/** Прочитать свойство настоящего элемента оригинальным геттером. */
export function readOriginal(accessor: OriginalAccessor, receiver: unknown): unknown {
  return callOriginal(accessor.get, receiver);
}

/** Записать свойство настоящего элемента оригинальным сеттером (ошибки — как у браузера). */
export function writeOriginal(accessor: OriginalAccessor, receiver: unknown, value: unknown): void {
  if (accessor.set === undefined) throw new TypeError("read-only original accessor");
  callOriginal(accessor.set, receiver, [value]);
}

/**
 * Снять оригиналы на document_start (до скриптов страницы). Зовётся один раз из page/main.ts.
 * Ничего не меняет на странице: только читает дескрипторы прототипов.
 */
export function captureElementOriginals(win: Window): ElementOriginals {
  const globals = win as unknown as PageGlobals;
  const mediaPrototype = globals.HTMLMediaElement.prototype;
  const videoPrototype = globals.HTMLVideoElement?.prototype;
  const eventTargetPrototype = globals.EventTarget.prototype;
  const eventPrototype = globals.Event.prototype;
  const mediaSourcePrototypes: object[] = [];
  if (globals.MediaSource !== undefined) mediaSourcePrototypes.push(globals.MediaSource.prototype);
  if (globals.MediaSourceHandle !== undefined) mediaSourcePrototypes.push(globals.MediaSourceHandle.prototype);

  return {
    mediaPrototype,
    videoPrototype,
    media: {
      currentTime: requireAccessor(mediaPrototype, "currentTime"),
      paused: requireAccessor(mediaPrototype, "paused"),
      playbackRate: requireAccessor(mediaPrototype, "playbackRate"),
      muted: requireAccessor(mediaPrototype, "muted"),
      volume: requireAccessor(mediaPrototype, "volume"),
      readyState: requireAccessor(mediaPrototype, "readyState"),
      duration: requireAccessor(mediaPrototype, "duration"),
      src: requireAccessor(mediaPrototype, "src"),
      srcObject: optionalAccessor(mediaPrototype, "srcObject"),
      networkState: requireAccessor(mediaPrototype, "networkState"),
      currentSrc: optionalAccessor(mediaPrototype, "currentSrc"),
      seekable: optionalAccessor(mediaPrototype, "seekable"),
      play: requireMethod(mediaPrototype, "play"),
      pause: requireMethod(mediaPrototype, "pause"),
      load: requireMethod(mediaPrototype, "load"),
    },
    video: {
      videoWidth: videoPrototype === undefined ? undefined : optionalAccessor(videoPrototype, "videoWidth"),
      videoHeight: videoPrototype === undefined ? undefined : optionalAccessor(videoPrototype, "videoHeight"),
      getVideoPlaybackQuality: optionalMethod(videoPrototype, "getVideoPlaybackQuality"),
      requestVideoFrameCallback: optionalMethod(videoPrototype, "requestVideoFrameCallback"),
      cancelVideoFrameCallback: optionalMethod(videoPrototype, "cancelVideoFrameCallback"),
      webkitDecodedFrameCount: videoPrototype === undefined ? undefined : optionalAccessor(videoPrototype, "webkitDecodedFrameCount"),
      webkitDroppedFrameCount: videoPrototype === undefined ? undefined : optionalAccessor(videoPrototype, "webkitDroppedFrameCount"),
    },
    events: {
      addEventListener: requireMethod(eventTargetPrototype, "addEventListener"),
      removeEventListener: requireMethod(eventTargetPrototype, "removeEventListener"),
      dispatchEvent: requireMethod(eventTargetPrototype, "dispatchEvent"),
      eventTarget: requireAccessor(eventPrototype, "target").get,
      stopImmediatePropagation: requireMethod(eventPrototype, "stopImmediatePropagation"),
      getRootNode: requireMethod(globals.Node.prototype, "getRootNode"),
      EventConstructor: globals.Event,
    },
    isConnected: optionalAccessor(globals.Node.prototype, "isConnected"),
    getAttribute: optionalMethod(globals.Element?.prototype, "getAttribute"),
    elementChildren: globals.Element === undefined ? undefined : optionalAccessor(globals.Element.prototype, "children"),
    localName: globals.Element === undefined ? undefined : optionalAccessor(globals.Element.prototype, "localName"),
    MediaStreamConstructor: globals.MediaStream,
    timeRangesPrototype: globals.TimeRanges?.prototype,
    PromiseConstructor: globals.Promise,
    DOMExceptionConstructor: globals.DOMException,
    TypeErrorConstructor: globals.TypeError,
    mediaSourcePrototypes,
    // Привязка к окну: queueMicrotask зовётся без `this` и иначе бросит «Illegal invocation».
    queueMicrotask: globals.queueMicrotask.bind(win),
  };
}

function optionalAccessor(prototype: object, name: string): OriginalAccessor | undefined {
  const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
  if (descriptor?.get === undefined) return undefined;
  return { get: descriptor.get as OriginalFunction, set: descriptor.set as OriginalFunction | undefined };
}

function requireAccessor(prototype: object, name: string): OriginalAccessor {
  const accessor = optionalAccessor(prototype, name);
  // Без базовых свойств HTMLMediaElement тень невозможна: это ошибка среды, а не страницы.
  if (accessor === undefined) throw new Error(`shadow: accessor ${name} is missing`);
  return accessor;
}

function optionalMethod(prototype: object | undefined, name: string): OriginalFunction | undefined {
  if (prototype === undefined) return undefined;
  const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
  return typeof descriptor?.value === "function" ? (descriptor.value as OriginalFunction) : undefined;
}

function requireMethod(prototype: object, name: string): OriginalFunction {
  const method = optionalMethod(prototype, name);
  if (method === undefined) throw new Error(`shadow: method ${name} is missing`);
  return method;
}
