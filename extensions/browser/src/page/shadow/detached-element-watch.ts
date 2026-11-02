// Страница убрала переданный `<video>` из документа (закрыла плеер, перешла на другой ролик SPA):
// тень должна сняться с `handoff_ended{element_released}`, иначе плеер играл бы видео, которого
// на странице уже нет.
//
// Дёшево и без MutationObserver: проверка `isConnected` (оригинальным геттером) на тиках часов
// плеера, которые и так приходят. Перенос элемента в DOM в той же задаче тик не увидит вовсе, а
// перенос через несколько задач (плеер переставил себя в контейнер полного экрана) покрывает
// допуск: снимаем, только если элемент отсутствует дольше него.

/**
 * Сколько элемент может отсутствовать в документе, прежде чем считать его брошенным. Секунда с
 * запасом покрывает перестановку плеера через несколько задач (анимации, `requestAnimationFrame`),
 * а пользователь за это время не успевает заметить, что видео играет «ни из чего».
 */
export const DETACHED_ELEMENT_GRACE_MS = 1_000;

export class DetachedElementWatch {
  /** Когда тик впервые увидел элемент вне документа (`null` — элемент на месте). */
  #detachedSinceMs: number | null = null;

  /** Тик часов: `true` — элемент вне документа дольше допуска, тень пора снимать. */
  onClockTick(isConnected: boolean, nowMs: number): boolean {
    if (isConnected) {
      this.#detachedSinceMs = null;
      return false;
    }
    if (this.#detachedSinceMs === null) {
      this.#detachedSinceMs = nowMs;
      return false;
    }
    return nowMs - this.#detachedSinceMs >= DETACHED_ELEMENT_GRACE_MS;
  }
}
