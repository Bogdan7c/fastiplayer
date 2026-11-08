// Сборка настоящего окружения тени на document_start и шов «запустить тень» для контроллера.
//
// Вынесено из `main.ts`, чтобы тесты MAIN-мира собирали тень ТЕМ ЖЕ кодом, что и продукт.
//
// На document_start (до скриптов страницы), всё синхронно:
//  1. снять оригиналы свойств элемента;
//  2. поставить переопределения прототипов (решение 181, Д-9). Вне передачи они строго прозрачны;
//     ставить их сразу нужно потому, что страница, сохранившая дескрипторы `currentTime`/`play`
//     до передачи, иначе обходила бы тень (читала настоящее время, будила настоящий элемент);
//  3. поставить стража mediaSession (иначе потом не восстановить обработчики страницы).

import type { HeldShadowStarter, PlayWatchSeam, RealMediaEventType, ShadowStarter } from "./handoff-seam.ts";
import { installMediaSessionGuard } from "./media-session-guard.ts";
import type { TimeoutScheduler } from "./shadow/file-return.ts";
import { callOriginal, readOriginal, writeOriginal } from "./shadow/originals.ts";
import { captureElementOriginals, installShadowOverrides, startHeldShadow, startShadow, type ShadowEnvironment } from "./shadow/index.ts";

/** Окружение тени без ответа реестра: его подставляет контроллер при каждом запуске тени. */
export type ShadowEnvironmentBase = Omit<ShadowEnvironment, "isMediaSourceObjectUrl">;

/** Окно, против которого собирается тень (в тестах — фейк с тем же составом). */
export type ShadowWindow = Window & { readonly performance: { now(): number } };

/**
 * Окружение тени. Если снять не вышло, хук MSE всё равно ставится (иначе «память без копий»
 * пропала бы навсегда), а передача честно откажет: `startShadow` бросит, контроллер ответит
 * `handoff_ended`.
 */
export function captureShadowEnvironment(win: ShadowWindow): ShadowEnvironmentBase | null {
  try {
    const originals = captureElementOriginals(win);
    installShadowOverrides(originals);
    return {
      win,
      originals,
      mediaSession: installMediaSessionGuard(win),
      now: win.performance.now.bind(win.performance),
      scheduleTimeout: captureTimeoutScheduler(win),
    };
  } catch {
    return null;
  }
}

/**
 * Таймер окна, снятый на document_start: страница позже может подменить `setTimeout`. Нужен только
 * страховке возврата файла (`FILE_RETURN_TIMEOUT_MS`), обычный путь идёт по событиям элемента.
 */
function captureTimeoutScheduler(win: ShadowWindow): TimeoutScheduler {
  const setTimer = win.setTimeout.bind(win);
  const clearTimer = win.clearTimeout.bind(win);
  return (callback, delayMs) => {
    const handle = setTimer(callback, delayMs);
    return () => clearTimer(handle);
  };
}

/** Шов для контроллера: запуск тени в снятом окружении (или честный отказ, если его нет). */
export function shadowStarterFor(environment: ShadowEnvironmentBase | null): ShadowStarter {
  return (element, emit, options) => {
    if (environment === null) throw new Error("shadow environment is unavailable in this document");
    return startShadow(element, emit, { ...environment, isMediaSourceObjectUrl: options.isMediaSourceObjectUrl }, options.source);
  };
}

/** Шов сторожа: тень на задержанном видео (или честный отказ, если окружения нет). */
export function heldShadowStarterFor(environment: ShadowEnvironmentBase | null): HeldShadowStarter {
  return (element, emit, options) => {
    if (environment === null) throw new Error("shadow environment is unavailable in this document");
    return startHeldShadow(
      element,
      emit,
      { ...environment, isMediaSourceObjectUrl: options.isMediaSourceObjectUrl },
      options.source,
      options.initialPlayback ?? "as_real_element",
    );
  };
}

/**
 * События настоящего элемента, по которым видно, что он уже хочет играть. `seeked` нужен взводу возврата
 * страницы (11b2): тень встаёт, когда страница честно перемоталась на секунду плеера.
 */
const WANTS_TO_PLAY_EVENT_TYPES: readonly RealMediaEventType[] = ["play", "loadedmetadata", "seeked"];

/**
 * Шов сторожа следующего видео с прототипами и событиями. Без окружения тени сторож работать не может:
 * шов остаётся пустым, `play()` прозрачен, а передача откажет сама.
 */
export function playWatchSeamFor(environment: ShadowEnvironmentBase | null): PlayWatchSeam {
  if (environment === null) {
    return {
      watchPlayCalls: () => {},
      isShadowed: () => false,
      onRealMediaEvent: () => {},
      seekRealElement: () => {},
      isSeekingReal: () => false,
      PagePromise: Promise,
    };
  }
  const { originals } = environment;
  const registry = installShadowOverrides(originals);
  return {
    watchPlayCalls: (watcher) => registry.watchPlayCalls(watcher),
    isShadowed: (element) => registry.isShadowed(element),
    PagePromise: originals.PromiseConstructor as PromiseConstructor,
    seekRealElement: (element, seconds) => writeOriginal(originals.media.currentTime, element, seconds),
    isSeekingReal: (element) => originals.media.seeking !== undefined && readOriginal(originals.media.seeking, element) === true,
    onRealMediaEvent: (target, listener) => {
      for (const type of WANTS_TO_PLAY_EVENT_TYPES) {
        const onEvent = (event: Event): void => {
          // Синтетические события ничего не значат: их рассылает и сама тень. `isTrusted` страница не подделает.
          if (!event.isTrusted) return;
          const element = callOriginal(originals.events.eventTarget, event);
          // Вид события известен по слушателю (а не читается из события): подмена `Event.type` страницей не влияет.
          if (typeof element === "object" && element !== null) listener(element, type);
        };
        callOriginal(originals.events.addEventListener, target, [type, onEvent, true]);
      }
    },
  };
}
