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

import type { ShadowStarter } from "./handoff-seam.ts";
import { installMediaSessionGuard } from "./media-session-guard.ts";
import { captureElementOriginals, installShadowOverrides, startShadow, type ShadowEnvironment } from "./shadow/index.ts";

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
    };
  } catch {
    return null;
  }
}

/** Шов для контроллера: запуск тени в снятом окружении (или честный отказ, если его нет). */
export function shadowStarterFor(environment: ShadowEnvironmentBase | null): ShadowStarter {
  return (element, emit, isMediaSourceObjectUrl) => {
    if (environment === null) throw new Error("shadow environment is unavailable in this document");
    return startShadow(element, emit, { ...environment, isMediaSourceObjectUrl });
  };
}
