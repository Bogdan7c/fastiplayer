// Сторож тишины тени (решение владельца 180, дыра Д-2 сессии 09a2).
//
// Инвариант «фон и страница согласны, что тень стоит» живёт у фона. Единственный признак жизни
// ТЕНИ — её пульс `shadow_alive`: байты и события MSE шлёт перехват страницы, и они идут даже
// тогда, когда тень уже снята, а её `handoff_ended` потерялся. Поэтому сторожа сбрасывает только
// пульс. Тишина дольше `SHADOW_SILENCE_LIMIT_MS` = страница и фон разошлись (потерянное событие,
// зависшая страница, сломанный канал) — владелец сеанса честно его кончает.
//
// Время — через `Scheduler` фона (как остальные таймеры сеанса): в тестах это фейковые часы.

import { SHADOW_SILENCE_LIMIT_MS } from "../../shared/handoff-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";

export interface ShadowSilenceWatch {
  /** Тень встала (вход в фазу `active`): отсчёт тишины начинается с этого момента. */
  start(): void;
  /** Пришёл пульс тени: отсчёт тишины заново. Вне слежения (до `start`, после `stop`) — ничего. */
  notePulse(): void;
  /** Слежение больше не нужно (тень снимаем сами или сеанс кончен). Повторный вызов безвреден. */
  stop(): void;
}

/**
 * @param onSilence — вызывается ровно один раз за слежение, когда тишина превысила предел;
 *   после него сторож остановлен.
 */
export function createShadowSilenceWatch(scheduler: Scheduler, onSilence: () => void): ShadowSilenceWatch {
  const silenceTimer = createTimerSlot(scheduler);
  let watching = false;

  // Один таймер «предел с последнего пульса», перезапускаемый каждым пульсом: пульс раз в секунду
  // — дёшево, а срабатывание точно в момент превышения, без периодического опроса.
  function restartCountdown(): void {
    silenceTimer.start(SHADOW_SILENCE_LIMIT_MS, () => {
      watching = false;
      onSilence();
    });
  }

  return {
    start() {
      watching = true;
      restartCountdown();
    },
    notePulse() {
      if (watching) restartCountdown();
    },
    stop() {
      watching = false;
      silenceTimer.cancel();
    },
  };
}
