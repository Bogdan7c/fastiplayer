// Скрипт MAIN-мира (общий мир со страницей), `document_start`, все фреймы. Точка входа IIFE `page.js`.
//
// Здесь только сборка настоящего окружения: оригиналы элемента и гашение mediaSession для тени
// (пакет A2) снимаются на старте, а всё остальное делает `startPageBridge` (bridge.ts).
// ВАЖНО: никаких глобалов и исключений в страницу — всё в замыкании и под try/catch.

import { startPageBridge } from "./bridge.ts";
import { installMediaSessionGuard } from "./media-session-guard.ts";
import { createOddityReporter, type DebugSink } from "./oddity-reporter.ts";
import { captureElementOriginals, startShadow, type ShadowEnvironment } from "./shadow/index.ts";

/** Окружение тени без ответа реестра: его подставляет контроллер при каждом запуске тени. */
type ShadowEnvironmentBase = Omit<ShadowEnvironment, "isMediaSourceObjectUrl">;

/**
 * Окружение тени. Снимается на document_start (до скриптов страницы). Если снять не вышло, хук MSE
 * всё равно ставится (иначе «память без копий» пропала бы навсегда), а передача честно
 * откажет: `startShadow` бросит, контроллер ответит `handoff_ended`.
 */
function captureShadowEnvironment(win: Window & typeof globalThis): ShadowEnvironmentBase | null {
  try {
    return {
      win,
      originals: captureElementOriginals(win),
      mediaSession: installMediaSessionGuard(win),
      now: win.performance.now.bind(win.performance),
    };
  } catch {
    return null;
  }
}

function main(win: Window & typeof globalThis, debugSink: DebugSink): void {
  const shadowEnvironment = captureShadowEnvironment(win);
  startPageBridge(win, {
    startShadow: (element, emit, isMediaSourceObjectUrl) => {
      if (shadowEnvironment === null) throw new Error("shadow environment is unavailable in this document");
      return startShadow(element, emit, { ...shadowEnvironment, isMediaSourceObjectUrl });
    },
    debugSink,
  });
}

// Консоль снимается первой, до всего, что может бросить: через неё же — след неудачного старта.
const consoleAtStart = window.console;
const debugAtStart = consoleAtStart.debug;
const debugSink: DebugSink = (line) => Reflect.apply(debugAtStart, consoleAtStart, [line]);
try {
  main(window, debugSink);
} catch {
  // Мост не встал — странице это не мешает; одна строка, чтобы при разборе было с чего начать.
  createOddityReporter(debugSink)("page bridge failed to start");
}
