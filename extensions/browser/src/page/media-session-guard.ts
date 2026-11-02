// Страж `navigator.mediaSession` страницы (поправка 02, пункт 8 задания 09a).
//
// Пока видео передано в Fastiplayer, системные медиакнопки и карточка «что играет» должны
// принадлежать плееру, а не странице: иначе пауза с клавиатуры ушла бы в тень браузера.
// Поэтому на время передачи у браузера гасится всё, что поставила страница, а при возврате
// возвращается ровно то, что она поставила (в том числе за время передачи).
//
// Страж ставится на document_start, раньше скриптов страницы: он обязан знать, что страница
// поставила ДО передачи. Обёртки прозрачны — сначала оригинал, запоминается только успешное.
// Пока страж приостановлен, вызовы страницы до системы НЕ доходят ни на миг: значения
// проверяются как у браузера (ошибки — её, того же типа) и только запоминаются.
//
// Ограничение (осознанное): обёртки висят на `MediaSession.prototype` с document_start на всех
// страницах, а не только во время передачи. Иначе нельзя: обработчики действий, поставленные
// страницей до передачи, у браузера не прочитать — их можно только запомнить в момент вызова, а
// без них при возврате медиакнопки страницы перестали бы работать. Цена — наши обёртки видны
// странице (`toString`), как и обёртки хука MSE.

import { applyOriginal, hasPrototype } from "./shadow/intrinsics.ts";
import { shadowProblemReporter } from "./shadow/once-reporter.ts";

/** Шов с тенью: на время передачи погасить mediaSession страницы, при возврате вернуть. */
export interface MediaSessionGuard {
  /** Погасить у браузера (metadata=null, состояние `none`, обработчики сняты). Повтор — ничего. */
  suspend(): void;
  /** Вернуть браузеру то, что поставила страница. Без `suspend` — ничего. */
  restore(): void;
}

/** Значение `playbackState`, когда страница «ничего не говорит» о воспроизведении. */
const NEUTRAL_PLAYBACK_STATE = "none";

/** Допустимые значения перечисления `MediaSessionPlaybackState` (недопустимые браузер молча игнорирует). */
const PLAYBACK_STATES: readonly string[] = ["none", "paused", "playing"];

type OriginalFunction = (...args: unknown[]) => unknown;

/** Оригиналы `MediaSession.prototype`, снятые до скриптов страницы. */
interface MediaSessionOriginals {
  readonly metadataGet: OriginalFunction;
  readonly metadataSet: OriginalFunction;
  readonly playbackStateGet: OriginalFunction;
  readonly playbackStateSet: OriginalFunction;
  readonly setActionHandler: OriginalFunction;
  /** Нет в старых браузерах. */
  readonly setPositionState: OriginalFunction | undefined;
  /** `MediaMetadata.prototype`: проверка значения `metadata`, пока погашено (без мигания у системы). */
  readonly metadataPrototype: object | undefined;
  /** TypeError мира страницы — наши ошибки проверки должны быть «её». */
  readonly TypeErrorConstructor: TypeErrorConstructor;
}

/** То, что страница поставила последним (и что вернём при `restore`). */
interface PageMediaSessionState {
  metadata: unknown;
  playbackState: string;
  readonly actionHandlers: Map<string, unknown>;
  /** `undefined` — страница позицию не ставила или очистила. */
  positionState: unknown;
}

const NO_OP_GUARD: MediaSessionGuard = { suspend() {}, restore() {} };

/** Поставить стража на document_start. Нет `navigator.mediaSession` — пустышка. */
export function installMediaSessionGuard(win: Window): MediaSessionGuard {
  const mediaSession = (win.navigator as { readonly mediaSession?: object }).mediaSession;
  if (mediaSession === undefined || mediaSession === null) return NO_OP_GUARD;
  const prototype = Object.getPrototypeOf(mediaSession) as object | null;
  if (prototype === null) return NO_OP_GUARD;
  const originals = captureOriginals(prototype, win);
  if (originals === undefined) return NO_OP_GUARD;

  const pageState: PageMediaSessionState = {
    metadata: applyOriginal(originals.metadataGet, mediaSession, []),
    playbackState: String(applyOriginal(originals.playbackStateGet, mediaSession, [])),
    actionHandlers: new Map(),
    positionState: undefined,
  };
  let suspended = false;
  installWrappers(prototype, originals, pageState, () => suspended);

  return {
    suspend() {
      if (suspended) return;
      suspended = true;
      clearBrowserSession(mediaSession, originals, pageState);
    },
    restore() {
      if (!suspended) return;
      suspended = false;
      restoreBrowserSession(mediaSession, originals, pageState);
    },
  };
}

function captureOriginals(prototype: object, win: Window): MediaSessionOriginals | undefined {
  const globals = win as unknown as {
    readonly MediaMetadata?: { readonly prototype: object };
    readonly TypeError: TypeErrorConstructor;
  };
  const metadata = Object.getOwnPropertyDescriptor(prototype, "metadata");
  const playbackState = Object.getOwnPropertyDescriptor(prototype, "playbackState");
  const setActionHandler = Object.getOwnPropertyDescriptor(prototype, "setActionHandler")?.value as unknown;
  const setPositionState = Object.getOwnPropertyDescriptor(prototype, "setPositionState")?.value as unknown;
  if (metadata?.get === undefined || metadata.set === undefined) return undefined;
  if (playbackState?.get === undefined || playbackState.set === undefined) return undefined;
  if (typeof setActionHandler !== "function") return undefined;
  return {
    metadataGet: metadata.get as OriginalFunction,
    metadataSet: metadata.set as OriginalFunction,
    playbackStateGet: playbackState.get as OriginalFunction,
    playbackStateSet: playbackState.set as OriginalFunction,
    setActionHandler: setActionHandler as OriginalFunction,
    setPositionState: typeof setPositionState === "function" ? (setPositionState as OriginalFunction) : undefined,
    metadataPrototype: globals.MediaMetadata?.prototype,
    TypeErrorConstructor: globals.TypeError,
  };
}

/** Обёртки на прототипе: сначала оригинал (или проверка), потом запоминание. */
function installWrappers(
  prototype: object,
  originals: MediaSessionOriginals,
  pageState: PageMediaSessionState,
  isSuspended: () => boolean,
): void {
  const accessors = {
    get metadata(): unknown {
      // Пока погашено, страница видит своё значение, а не наш null: иначе она ставила бы его снова.
      return isSuspended() ? pageState.metadata : applyOriginal(originals.metadataGet, this, []);
    },
    set metadata(value: unknown) {
      const metadata = value === undefined ? null : value;
      if (!isSuspended()) {
        // Оригинал проверяет тип (MediaMetadata или null) и бросает TypeError, как у браузера.
        applyOriginal(originals.metadataSet, this, [metadata]);
      } else if (metadata !== null && !isMediaMetadata(metadata, originals)) {
        throw new originals.TypeErrorConstructor(
          "Failed to set the 'metadata' property on 'MediaSession': The provided value is not of type 'MediaMetadata'.",
        );
      }
      pageState.metadata = metadata;
    },
    get playbackState(): unknown {
      return isSuspended() ? pageState.playbackState : applyOriginal(originals.playbackStateGet, this, []);
    },
    set playbackState(value: unknown) {
      // Шаблонная строка, а не String(): для Symbol она бросает TypeError, как WebIDL.
      const state = `${value as string}`;
      // Недопустимое значение перечисления браузер молча игнорирует — так же и мы.
      if (!PLAYBACK_STATES.includes(state)) return;
      pageState.playbackState = state;
      if (!isSuspended()) applyOriginal(originals.playbackStateSet, this, [state]);
    },
  };
  defineAccessor(prototype, "metadata", accessors);
  defineAccessor(prototype, "playbackState", accessors);

  const methods = {
    setActionHandler(this: unknown, action: unknown, handler: unknown): unknown {
      if (isSuspended()) {
        if (handler !== null && handler !== undefined && typeof handler !== "function") {
          // Как WebIDL для nullable-колбэка: не функция и не null — TypeError.
          throw new originals.TypeErrorConstructor("Failed to execute 'setActionHandler' on 'MediaSession': The callback provided as parameter 2 is not a function.");
        }
        // Оригинал с null проверяет имя действия (TypeError на неизвестное) и держит его снятым.
        applyOriginal(originals.setActionHandler, this, [action, null]);
      } else {
        applyOriginal(originals.setActionHandler, this, [action, handler]);
      }
      const actionName = String(action);
      if (handler === null || handler === undefined) pageState.actionHandlers.delete(actionName);
      else pageState.actionHandlers.set(actionName, handler);
      return undefined;
    },
    setPositionState(this: unknown, ...args: unknown[]): unknown {
      const original = originals.setPositionState;
      if (original === undefined) return undefined;
      if (isSuspended()) {
        // До браузера не доходит даже на миг: проверка — своя, по правилам спецификации.
        pageState.positionState = normalizePositionState(args[0], originals.TypeErrorConstructor);
        return undefined;
      }
      // Проверку (TypeError на позицию за длительностью и т.п.) делает оригинал.
      applyOriginal(original, this, args);
      const positionState = args[0];
      const clears = positionState === undefined || positionState === null || isEmptyObject(positionState);
      pageState.positionState = clears ? undefined : positionState;
      return undefined;
    },
  };
  defineMethod(prototype, "setActionHandler", methods.setActionHandler, originals.setActionHandler);
  if (originals.setPositionState !== undefined) {
    defineMethod(prototype, "setPositionState", methods.setPositionState, originals.setPositionState);
  }
}

function clearBrowserSession(mediaSession: object, originals: MediaSessionOriginals, pageState: PageMediaSessionState): void {
  attempt("mediaSession suspend: metadata", () => applyOriginal(originals.metadataSet, mediaSession, [null]));
  attempt("mediaSession suspend: playbackState", () =>
    applyOriginal(originals.playbackStateSet, mediaSession, [NEUTRAL_PLAYBACK_STATE]),
  );
  for (const action of pageState.actionHandlers.keys()) {
    attempt("mediaSession suspend: action handler", () => applyOriginal(originals.setActionHandler, mediaSession, [action, null]));
  }
  const setPositionState = originals.setPositionState;
  if (setPositionState !== undefined && pageState.positionState !== undefined) {
    attempt("mediaSession suspend: position", () => applyOriginal(setPositionState, mediaSession, []));
  }
}

function restoreBrowserSession(mediaSession: object, originals: MediaSessionOriginals, pageState: PageMediaSessionState): void {
  attempt("mediaSession restore: metadata", () => applyOriginal(originals.metadataSet, mediaSession, [pageState.metadata]));
  attempt("mediaSession restore: playbackState", () =>
    applyOriginal(originals.playbackStateSet, mediaSession, [pageState.playbackState]),
  );
  for (const [action, handler] of pageState.actionHandlers) {
    attempt("mediaSession restore: action handler", () => applyOriginal(originals.setActionHandler, mediaSession, [action, handler]));
  }
  const setPositionState = originals.setPositionState;
  if (setPositionState !== undefined && pageState.positionState !== undefined) {
    // Позиция могла устареть (длительность сменилась) — тогда браузер откажет, это не повод ломать возврат.
    attempt("mediaSession restore: position", () => applyOriginal(setPositionState, mediaSession, [pageState.positionState]));
  }
}

/** Сбой браузера при гашении/возврате не должен ломать передачу: одна строка отладки за жизнь. */
function attempt(context: string, action: () => unknown): void {
  try {
    action();
  } catch (problem) {
    shadowProblemReporter.report(context, problem);
  }
}

/** Пока погашено, проверяем тип сами; без `MediaMetadata` в окне доверяем странице. */
function isMediaMetadata(candidate: unknown, originals: MediaSessionOriginals): boolean {
  return originals.metadataPrototype === undefined || hasPrototype(candidate, originals.metadataPrototype);
}

/** Позиция, проверенная как в `MediaSession.setPositionState` (спецификация Media Session). */
interface NormalizedPositionState {
  readonly duration: number;
  readonly playbackRate: number;
  readonly position: number;
}

/** Скорость по умолчанию в словаре `MediaPositionState`. */
const DEFAULT_POSITION_RATE = 1;

/**
 * Проверка и приведение `MediaPositionState`, как это сделал бы браузер (пока погашено, оригинал
 * звать нельзя: он сразу показал бы позицию системе). `undefined` — страница позицию очистила.
 * Поля словаря читаются один раз, в порядке WebIDL (по алфавиту); ошибки — TypeError мира страницы.
 */
function normalizePositionState(state: unknown, TypeErrorConstructor: TypeErrorConstructor): NormalizedPositionState | undefined {
  const fail = (reason: string): never => {
    throw new TypeErrorConstructor(`Failed to execute 'setPositionState' on 'MediaSession': ${reason}`);
  };
  if (state === undefined || state === null) return undefined;
  if (typeof state !== "object" && typeof state !== "function") return fail("The provided value is not of type 'MediaPositionState'.");
  const fields = state as { readonly duration?: unknown; readonly playbackRate?: unknown; readonly position?: unknown };
  const rawDuration = fields.duration;
  const rawRate = fields.playbackRate;
  const rawPosition = fields.position;
  if (rawDuration === undefined && rawRate === undefined && rawPosition === undefined) return undefined;
  if (rawDuration === undefined) return fail("The duration must be provided.");
  const duration = Number(rawDuration);
  const playbackRate = rawRate === undefined ? DEFAULT_POSITION_RATE : Number(rawRate);
  const position = rawPosition === undefined ? 0 : Number(rawPosition);
  // `playbackRate` и `position` — restricted double: не-числа и бесконечности браузер отвергает.
  if (!Number.isFinite(playbackRate) || !Number.isFinite(position)) return fail("The provided double value is non-finite.");
  if (Number.isNaN(duration) || duration < 0) return fail("The provided duration cannot be less than zero.");
  if (position < 0) return fail("The provided position cannot be less than zero.");
  if (position > duration) return fail("The provided position cannot be greater than the duration.");
  if (playbackRate === 0) return fail("The provided playbackRate cannot be equal to zero.");
  return { duration, playbackRate, position };
}

function isEmptyObject(candidate: unknown): boolean {
  return typeof candidate === "object" && candidate !== null && Object.keys(candidate).length === 0;
}

function defineAccessor(prototype: object, name: string, source: object): void {
  const replacement = Object.getOwnPropertyDescriptor(source, name);
  const current = Object.getOwnPropertyDescriptor(prototype, name);
  const descriptor: PropertyDescriptor = { enumerable: current?.enumerable === true, configurable: true };
  if (replacement?.get !== undefined) descriptor.get = replacement.get;
  if (replacement?.set !== undefined) descriptor.set = replacement.set;
  Object.defineProperty(prototype, name, descriptor);
}

function defineMethod(prototype: object, name: string, replacement: OriginalFunction, original: OriginalFunction): void {
  const current = Object.getOwnPropertyDescriptor(prototype, name);
  Object.defineProperty(replacement, "length", { value: original.length });
  Object.defineProperty(prototype, name, {
    value: replacement,
    writable: current?.writable === true,
    enumerable: current?.enumerable === true,
    configurable: true,
  });
}
