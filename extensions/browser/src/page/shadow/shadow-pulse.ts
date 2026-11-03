// Пульс «тень жива» (решение 180, Д-2): тень раз в `SHADOW_PULSE_INTERVAL_MS` шлёт `shadow_alive`.
//
// Зачем: если `handoff_ended` (или любое другое событие) потерялось по дороге, фон, плеер и
// страница навсегда разошлись бы во мнении «передача жива». Пульс превращает потерю в тишину,
// а тишину дольше `SHADOW_SILENCE_LIMIT_MS` фон честно кончает (решение 180).
//
// Пульс идёт на тиках часов плеера, а не таймером: таймеры скрытой вкладки Chromium будит раз в
// минуту, а часы приходят каждые ~250 мс и на паузе. Нет тиков — нет и пульса: значит, сломан
// путь фон → страница, и фону тоже пора кончать.

import { SHADOW_PULSE_INTERVAL_MS } from "../../shared/handoff-contract.ts";

export class ShadowPulse {
  /** Когда ушёл последний пульс (`null` — ещё ни одного). */
  #lastPulseAtMs: number | null = null;

  /** Тик часов: `true` — пора подать пульс (первый — на первом же тике). */
  onClockTick(nowMs: number): boolean {
    if (this.#lastPulseAtMs !== null && nowMs - this.#lastPulseAtMs < SHADOW_PULSE_INTERVAL_MS) return false;
    this.#lastPulseAtMs = nowMs;
    return true;
  }
}
