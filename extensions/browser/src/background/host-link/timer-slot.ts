// «Слот» таймера: не больше одного живого таймера на назначение. Повторный start заменяет прежний,
// cancel гарантированно снимает — так после остановки связи не остаётся висящих таймеров.
import type { Scheduler, TimerHandle } from "./native-port.ts";

export interface TimerSlot {
  start(delayMs: number, callback: () => void): void;
  cancel(): void;
  isActive(): boolean;
}

export function createTimerSlot(scheduler: Scheduler): TimerSlot {
  let handle: TimerHandle | undefined;
  let active = false;
  const cancel = (): void => {
    if (!active) return;
    scheduler.clearTimeout(handle);
    handle = undefined;
    active = false;
  };
  return {
    start(delayMs, callback) {
      cancel();
      active = true;
      handle = scheduler.setTimeout(() => {
        // Сработавший таймер больше не активен — иначе cancel() позже снимал бы чужой handle.
        active = false;
        handle = undefined;
        callback();
      }, delayMs);
    },
    cancel,
    isActive: () => active,
  };
}
