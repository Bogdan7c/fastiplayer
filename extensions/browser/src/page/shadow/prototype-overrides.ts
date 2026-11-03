// Переопределения свойств и методов видеоэлемента на прототипах — только для элементов в тени.
//
// Ставятся на document_start (решение 181, Д-9; `shadow-assembly.ts`), а не при первой передаче:
// страница, сохранившая дескрипторы `currentTime`/`play` до передачи, иначе обходила бы тень.
// Каждое переопределение смотрит в таблицу теней: элемент в тени — отвечает тень; любой другой
// элемент (и тот же после снятия тени) — СТРОГО прозрачный вызов того, что стояло на прототипе в
// момент установки (значение, исключение, `this`, обещание `play()` — те же, что у оригинала).
// Это родной дескриптор браузера или подмена, поставленная ДО нас (другой экстеншен): она
// продолжает работать для всех элементов, кроме переданного. Подмена, поставленная ПОСЛЕ нас,
// оборачивает наше переопределение и видит вызовы всех элементов, а переданный обслуживает тень.
// Цена — заметность: `toString` переопределений не «[native code]» (принято владельцем).

import { applyOriginal, SealedWeakMap } from "./intrinsics.ts";
import type { ElementOriginals } from "./originals.ts";

/**
 * Что тень отвечает странице за переданный элемент. Сеттеры и методы получают «сырые»
 * аргументы страницы: проверка и ошибки — как у браузера — на стороне тени.
 */
export interface ShadowBehavior {
  currentTime(): number;
  setCurrentTime(value: unknown): void;
  paused(): boolean;
  ended(): boolean;
  seeking(): boolean;
  playbackRate(): number;
  setPlaybackRate(value: unknown): void;
  readyState(): number;
  muted(): boolean;
  setMuted(value: unknown): void;
  volume(): number;
  setVolume(value: unknown): void;
  play(): Promise<void>;
  pause(): void;
  /** `load()` страницы: оригинал, затем тень повторяет «алгоритм загрузки». */
  load(): void;
  /** Запись `src`/`srcObject` страницей: оригинал, затем тень повторяет «алгоритм загрузки». */
  setSource(property: SourceProperty, value: unknown): void;
  fastSeek(value: unknown): void;
  getVideoPlaybackQuality(): unknown;
  requestVideoFrameCallback(callback: unknown): number;
  cancelVideoFrameCallback(handle: unknown): void;
  webkitDecodedFrameCount(): number;
  webkitDroppedFrameCount(): number;
}

/** Свойства, запись которых запускает «алгоритм загрузки» элемента. */
export type SourceProperty = "src" | "srcObject";

/** Таблица «элемент → тень» одного документа. */
export interface ShadowRegistry {
  attach(element: HTMLMediaElement, behavior: ShadowBehavior): void;
  detach(element: HTMLMediaElement): void;
  isShadowed(element: HTMLMediaElement): boolean;
}

/**
 * Свойство-аксессор: что тень отвечает на чтение и (если свойство записываемое) на запись.
 * Без `read` чтение и у элемента в тени идёт в оригинал (`src` — настоящий).
 */
interface AccessorOverride {
  readonly name: string;
  readonly read?: (behavior: ShadowBehavior) => unknown;
  readonly write?: (behavior: ShadowBehavior, value: unknown) => void;
}

/** Метод: что тень делает вместо него. */
interface MethodOverride {
  readonly name: string;
  readonly invoke: (behavior: ShadowBehavior, args: readonly unknown[]) => unknown;
}

const MEDIA_ACCESSORS: readonly AccessorOverride[] = [
  { name: "currentTime", read: (b) => b.currentTime(), write: (b, v) => b.setCurrentTime(v) },
  { name: "paused", read: (b) => b.paused() },
  { name: "ended", read: (b) => b.ended() },
  { name: "seeking", read: (b) => b.seeking() },
  { name: "playbackRate", read: (b) => b.playbackRate(), write: (b, v) => b.setPlaybackRate(v) },
  { name: "readyState", read: (b) => b.readyState() },
  { name: "muted", read: (b) => b.muted(), write: (b, v) => b.setMuted(v) },
  { name: "volume", read: (b) => b.volume(), write: (b, v) => b.setVolume(v) },
  { name: "src", write: (b, v) => b.setSource("src", v) },
  { name: "srcObject", write: (b, v) => b.setSource("srcObject", v) },
];

const MEDIA_METHODS: readonly MethodOverride[] = [
  { name: "play", invoke: (b) => b.play() },
  { name: "pause", invoke: (b) => b.pause() },
  { name: "load", invoke: (b) => b.load() },
  // Есть только в Firefox/Safari; где метода нет, переопределение не ставится.
  { name: "fastSeek", invoke: (b, args) => b.fastSeek(args[0]) },
];

const VIDEO_ACCESSORS: readonly AccessorOverride[] = [
  { name: "webkitDecodedFrameCount", read: (b) => b.webkitDecodedFrameCount() },
  { name: "webkitDroppedFrameCount", read: (b) => b.webkitDroppedFrameCount() },
];

const VIDEO_METHODS: readonly MethodOverride[] = [
  { name: "getVideoPlaybackQuality", invoke: (b) => b.getVideoPlaybackQuality() },
  { name: "requestVideoFrameCallback", invoke: (b, args) => b.requestVideoFrameCallback(args[0]) },
  { name: "cancelVideoFrameCallback", invoke: (b, args) => b.cancelVideoFrameCallback(args[0]) },
];

/** Одна установка на документ: ключ — оригиналы окна (они снимаются один раз на документ). */
const installedRegistries = new SealedWeakMap<ElementOriginals, ShadowRegistry>();

/**
 * Поставить переопределения (если ещё не стоят) и вернуть таблицу теней документа. Идемпотентно:
 * `shadow-assembly.ts` зовёт на document_start, `startShadow` — за той же таблицей.
 */
export function installShadowOverrides(originals: ElementOriginals): ShadowRegistry {
  const existing = installedRegistries.get(originals);
  if (existing !== undefined) return existing;

  const shadows = new SealedWeakMap<object, ShadowBehavior>();
  const lookup = (receiver: unknown): ShadowBehavior | undefined => shadows.get(receiver);
  for (const override of MEDIA_ACCESSORS) overrideAccessor(originals.mediaPrototype, override, lookup);
  for (const override of MEDIA_METHODS) overrideMethod(originals.mediaPrototype, override, lookup);
  if (originals.videoPrototype !== undefined) {
    for (const override of VIDEO_ACCESSORS) overrideAccessor(originals.videoPrototype, override, lookup);
    for (const override of VIDEO_METHODS) overrideMethod(originals.videoPrototype, override, lookup);
  }

  const registry: ShadowRegistry = {
    attach: (element, behavior) => shadows.set(element, behavior),
    detach: (element) => shadows.delete(element),
    isShadowed: (element) => shadows.get(element) !== undefined,
  };
  installedRegistries.set(originals, registry);
  return registry;
}

type ShadowLookup = (receiver: unknown) => ShadowBehavior | undefined;

function overrideAccessor(prototype: object, override: AccessorOverride, lookup: ShadowLookup): void {
  const current = Object.getOwnPropertyDescriptor(prototype, override.name);
  // Свойства нет у этого браузера (webkit-счётчики в Firefox) или оно не аксессор — не трогаем.
  if (current?.get === undefined || current.configurable !== true) return;
  const passthroughGet = current.get;
  const passthroughSet = current.set;
  const { read, write } = override;
  // Объектный литерал даёт функциям «родные» имена (`get currentTime`), как у браузера.
  const replacement = {
    get [override.name](): unknown {
      const behavior = lookup(this);
      return behavior === undefined || read === undefined ? applyOriginal(passthroughGet, this, []) : read(behavior);
    },
    set [override.name](value: unknown) {
      const behavior = lookup(this);
      if (behavior === undefined || write === undefined) {
        if (passthroughSet === undefined) return;
        applyOriginal(passthroughSet, this, [value]);
        return;
      }
      write(behavior, value);
    },
  };
  const replacementDescriptor = Object.getOwnPropertyDescriptor(replacement, override.name);
  const descriptor: PropertyDescriptor = {
    enumerable: current.enumerable === true,
    configurable: true,
  };
  if (replacementDescriptor?.get !== undefined) descriptor.get = replacementDescriptor.get;
  // Свойство только для чтения остаётся только для чтения.
  if (passthroughSet !== undefined && replacementDescriptor?.set !== undefined) descriptor.set = replacementDescriptor.set;
  Object.defineProperty(prototype, override.name, descriptor);
}

function overrideMethod(prototype: object, override: MethodOverride, lookup: ShadowLookup): void {
  const current = Object.getOwnPropertyDescriptor(prototype, override.name);
  if (typeof current?.value !== "function" || current.configurable !== true) return;
  const passthrough = current.value as (...args: unknown[]) => unknown;
  const { invoke } = override;
  const replacement = {
    [override.name](this: unknown, ...args: unknown[]): unknown {
      const behavior = lookup(this);
      return behavior === undefined ? applyOriginal(passthrough, this, args) : invoke(behavior, args);
    },
  };
  const replacementFunction = replacement[override.name];
  // Длина как у оригинала: страница (или её проверка «родной ли метод») видит ту же сигнатуру.
  Object.defineProperty(replacementFunction, "length", { value: passthrough.length });
  Object.defineProperty(prototype, override.name, {
    value: replacementFunction,
    writable: current.writable === true,
    enumerable: current.enumerable === true,
    configurable: true,
  });
}
