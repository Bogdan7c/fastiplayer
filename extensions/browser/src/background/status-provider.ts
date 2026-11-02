// Шов между фоном-оболочкой (пакет B) и связью с посредником (пакет C).
import type { BridgeStatus } from "../shared/bridge-status.ts";

export interface BridgeStatusProvider {
  // Последний известный статус (без обращения к плееру).
  current(): BridgeStatus;
  // Подписка на смены статуса; возвращает функцию отписки.
  subscribe(listener: (status: BridgeStatus) => void): () => void;
  // «Связь нужна» (попап открыт, видео передаётся). Пока нужда есть — провайдер проверяет связь
  // по-настоящему и делает повторы. Возвращает функцию освобождения (вызывать ровно один раз).
  acquireDemand(reason: string): () => void;
}

// Заглушка: всегда unknown. Пакет C подставит настоящий поставщик в main.ts.
export function createUnknownStatusProvider(): BridgeStatusProvider {
  return {
    current: () => ({ state: "unknown" }),
    subscribe: () => () => {},
    acquireDemand: () => () => {},
  };
}
