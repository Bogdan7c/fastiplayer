// Сторож тишины тени (решение владельца 180, дыра Д-2 сессии 09a2; правило «прожитых секунд» —
// решение 330, сессия 16d2).
//
// Инвариант «фон и страница согласны, что тень стоит» живёт у фона. Единственный признак жизни
// ТЕНИ — её пульс `shadow_alive`: байты и события MSE шлёт перехват страницы, и они идут даже
// тогда, когда тень уже снята, а её `handoff_ended` потерялся. Поэтому сторожа сбрасывает только
// пульс. Тишина дольше `SHADOW_SILENCE_LIMIT_MS` = страница и фон разошлись (потерянное событие,
// зависшая страница, сломанный канал) — владелец сеанса честно его кончает.
//
// Тишина считается ПРОЖИТЫМИ секундами, а не по часам. Сон машины (16d2, T480s): пока ядро
// будит устройства, монотонные часы идут (~48 с), а браузер ещё заморожен. Один таймер на 10 с
// после оттаивания срабатывал сразу, раньше первого пульса, и живой сеанс кончался 4003
// «Stream error». Теперь фон тикает раз в секунду и считает тики без пульса: просроченный за
// время заморозки тик срабатывает один раз и добавляет одну секунду, а не все проспанные.
// Зависшую страницу при живом фоне сторож по-прежнему ловит за `SHADOW_SILENCE_LIMIT_MS`.
//
// Время — через `Scheduler` фона (как остальные таймеры сеанса): в тестах это фейковые часы.

import { SHADOW_SILENCE_LIMIT_MS } from "../../shared/handoff-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createTimerSlot } from "../host-link/timer-slot.ts";

/** Шаг счёта тишины: одна прожитая фоном секунда. */
const SILENCE_TICK_MS = 1_000;

/** Сколько прожитых тихих секунд подряд — уже конец сеанса. */
const SILENT_TICKS_LIMIT = Math.ceil(SHADOW_SILENCE_LIMIT_MS / SILENCE_TICK_MS);

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
  const tickTimer = createTimerSlot(scheduler);
  let watching = false;
  /** Сколько тиков подряд прошло без пульса. */
  let silentTicks = 0;

  // Счёт заново с этой секунды. Тик перезапускается от пульса, поэтому в обычной жизни конец
  // наступает ровно через предел после последнего пульса — как у прежнего единого таймера.
  function restartCount(): void {
    silentTicks = 0;
    tickTimer.start(SILENCE_TICK_MS, onTick);
  }

  // Одна прожитая тихая секунда. Сколько бы времени ни прошло по часам с прошлого тика (сон,
  // заморозка вкладки), это одна секунда: проспанное время тишиной страницы не считается.
  function onTick(): void {
    silentTicks += 1;
    if (silentTicks < SILENT_TICKS_LIMIT) {
      tickTimer.start(SILENCE_TICK_MS, onTick);
      return;
    }
    watching = false;
    onSilence();
  }

  return {
    start() {
      watching = true;
      restartCount();
    },
    notePulse() {
      if (watching) restartCount();
    },
    stop() {
      watching = false;
      tickTimer.cancel();
    },
  };
}
