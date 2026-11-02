// Маршрутизация порта «bridge-status»: попап открыл порт — фон шлёт статус и держит «нужду» в связи.
import type { BridgeStatusProvider } from "./status-provider.ts";
import { BRIDGE_STATUS_PORT_NAME, type StatusUpdateMessage } from "../shared/internal-messages.ts";

// Минимум от runtime.Port, нужный роутеру (упрощает фейки в тестах).
export interface StatusPortLike {
  readonly name: string;
  postMessage(message: StatusUpdateMessage): void;
  readonly onDisconnect: { addListener(listener: () => void): void };
}

// Возвращает true, если порт наш и обработан; порты с другим именем игнорируются (false).
export function routeStatusPort(port: StatusPortLike, provider: BridgeStatusProvider): boolean {
  if (port.name !== BRIDGE_STATUS_PORT_NAME) return false;

  let disconnected = false;
  const send = (status: StatusUpdateMessage["status"]): void => {
    if (disconnected) return;
    try {
      port.postMessage({ kind: "status_update", status });
    } catch (error) {
      // Попап мог закрыться между событием и отправкой — это штатно, но не молчим.
      console.warn("status port postMessage failed", error);
    }
  };

  // Нужда в связи появляется ДО первого сообщения, чтобы провайдер сразу начал проверку.
  const releaseDemand = provider.acquireDemand("popup");
  const unsubscribe = provider.subscribe(send);
  send(provider.current());

  port.onDisconnect.addListener(() => {
    // Освобождение ровно один раз, даже если onDisconnect придёт повторно.
    if (disconnected) return;
    disconnected = true;
    unsubscribe();
    releaseDemand();
  });
  return true;
}
