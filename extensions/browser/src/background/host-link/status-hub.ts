// Хранилище статуса и подписчиков: одинаковый статус подряд не рассылается.
import type { BridgeStatus } from "../../shared/bridge-status.ts";

export interface StatusHub {
  current(): BridgeStatus;
  subscribe(listener: (status: BridgeStatus) => void): () => void;
  // Возвращает true, если статус изменился и подписчики оповещены.
  publish(next: BridgeStatus): boolean;
}

// Статусы — плоские объекты с постоянным порядком полей (строим их сами), поэтому сравнение по JSON надёжно.
function isSameStatus(left: BridgeStatus, right: BridgeStatus): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function createStatusHub(initial: BridgeStatus): StatusHub {
  let status = initial;
  const listeners = new Set<(status: BridgeStatus) => void>();
  return {
    current: () => status,
    subscribe(listener) {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    publish(next) {
      if (isSameStatus(status, next)) return false;
      status = next;
      // Копия набора: подписчик может отписаться прямо в обработчике.
      for (const listener of [...listeners]) {
        try {
          listener(next);
        } catch (error) {
          // Падение одного подписчика (закрытый попап) не должно ломать остальных.
          console.warn("status listener failed", error);
        }
      }
      return true;
    },
  };
}
