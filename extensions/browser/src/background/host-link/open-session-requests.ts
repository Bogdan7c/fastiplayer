// Таблица ожидающих запросов `open_session`: сопоставление ответов по request_id, таймауты, явные отказы.
// Владеет только таблицей и таймерами запросов; порт и статус — у host-link.ts.
import type { BridgeStatus } from "../../shared/bridge-status.ts";
import type { ExtensionOpenSession, PlayerRefused, PlayerSessionGranted, ProtocolError, ProtocolVersion } from "../../protocol/index.ts";
import { OPEN_SESSION_REPLY_TIMEOUT_MS } from "./constants.ts";
import type { Scheduler } from "./native-port.ts";
import { createTimerSlot, type TimerSlot } from "./timer-slot.ts";

// Всё, что вызывающий задаёт сам; `type` и `request_id` проставляет связь.
export type OpenSessionRequest = Omit<ExtensionOpenSession, "type" | "request_id">;

// Чем закончился запрос. Явно различаем причины, чтобы сессия 10 могла реагировать по-разному.
export type OpenSessionOutcome =
  // `agreedProtocol` - версия из `hello_accepted` соединения, по которому выдан сеанс: сеанс не шлёт сообщений новее неё.
  | { readonly kind: "granted"; readonly grant: PlayerSessionGranted; readonly agreedProtocol: ProtocolVersion }
  | { readonly kind: "refused"; readonly refusal: PlayerRefused } // ответ плеера/посредника
  | { readonly kind: "no_reply" } // порт упал, таймаут или мусор после отправки
  | { readonly kind: "not_connected"; readonly status: BridgeStatus } // до отправки связь не поднялась
  | { readonly kind: "invalid_request"; readonly error: ProtocolError }; // запрос не прошёл строгую проверку протокола

export interface PendingRequest {
  readonly requestId: number;
  readonly sessionKey: string;
  // Готовый JSON запроса (проверен кодеком протокола заранее).
  readonly messageText: string;
  // Уже отправлен в текущий порт и ждёт ответа?
  sent: boolean;
  // Повтор после hello_required разрешён один раз (решение 153): иначе петля.
  helloRetried: boolean;
}

interface Entry {
  readonly request: PendingRequest;
  readonly resolve: (outcome: OpenSessionOutcome) => void;
  readonly timeout: TimerSlot;
}

export interface OpenSessionRequests {
  add(request: PendingRequest, resolve: (outcome: OpenSessionOutcome) => void): void;
  get(requestId: number): PendingRequest | undefined;
  // Запускает таймаут ответа (вызывать при отправке).
  markSent(requestId: number): void;
  // Возвращает запрос в «не отправлен» (после hello_required): таймаут снят, ждёт нового hello.
  markUnsent(requestId: number): void;
  unsent(): PendingRequest[];
  // Есть ли запрос, отправленный в порт и ещё ждущий ответа.
  hasSent(): boolean;
  settle(requestId: number, outcome: OpenSessionOutcome): void;
  // Связь потеряна: неотправленные получают not_connected, отправленные — no_reply.
  settleAllOnLinkLoss(statusAfterLoss: BridgeStatus): void;
  size(): number;
}

export function createOpenSessionRequests(scheduler: Scheduler, onSettled: () => void): OpenSessionRequests {
  const entries = new Map<number, Entry>();

  const settle = (requestId: number, outcome: OpenSessionOutcome): void => {
    const entry = entries.get(requestId);
    if (entry === undefined) return;
    // Сначала убираем запись и таймер, потом разрешаем промис: ни один запрос не остаётся «висеть».
    entries.delete(requestId);
    entry.timeout.cancel();
    entry.resolve(outcome);
    onSettled();
  };

  return {
    add: (request, resolve) => void entries.set(request.requestId, { request, resolve, timeout: createTimerSlot(scheduler) }),
    get: (requestId) => entries.get(requestId)?.request,
    markSent(requestId) {
      const entry = entries.get(requestId);
      if (entry === undefined) return;
      entry.request.sent = true;
      entry.timeout.start(OPEN_SESSION_REPLY_TIMEOUT_MS, () => settle(requestId, { kind: "no_reply" }));
    },
    markUnsent(requestId) {
      const entry = entries.get(requestId);
      if (entry === undefined) return;
      entry.request.sent = false;
      entry.timeout.cancel();
    },
    unsent: () => [...entries.values()].map((entry) => entry.request).filter((request) => !request.sent),
    hasSent: () => [...entries.values()].some((entry) => entry.request.sent),
    settle,
    settleAllOnLinkLoss(statusAfterLoss) {
      for (const [requestId, entry] of [...entries]) {
        settle(requestId, entry.request.sent ? { kind: "no_reply" } : { kind: "not_connected", status: statusAfterLoss });
      }
    },
    size: () => entries.size,
  };
}
