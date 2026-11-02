// Внутренние сообщения между частями экстеншена (фон <-> попап).
import type { BridgeStatus } from "./bridge-status.ts";

// Имя порта, который попап открывает при показе. Открытый порт = «связь с плеером нужна»;
// закрытие попапа закрывает порт и снимает нужду.
export const BRIDGE_STATUS_PORT_NAME = "bridge-status";

// Фон -> попап: текущий статус (сразу при подключении и при каждой смене).
export type StatusUpdateMessage = {
  readonly kind: "status_update";
  readonly status: BridgeStatus;
};

const PLAYER_UNAVAILABLE_REASONS = ["player_unavailable", "player_not_responding", "player_shutting_down", "no_reply"];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isNonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
}

// Строгая проверка формы статуса: лишние поля и неверные типы — отказ (мусор не доходит до UI).
export function isBridgeStatus(value: unknown): value is BridgeStatus {
  if (!isRecord(value)) return false;
  const keys = Object.keys(value);
  const hasOnlyKeys = (allowed: readonly string[]): boolean => keys.length === allowed.length && allowed.every((key) => key in value);
  switch (value["state"]) {
    case "unknown":
    case "checking":
    case "host_missing":
      return hasOnlyKeys(["state"]);
    case "player_unavailable":
      return hasOnlyKeys(["state", "reason"]) && PLAYER_UNAVAILABLE_REASONS.includes(value["reason"] as string);
    case "incompatible":
      return hasOnlyKeys(["state", "update"]) && (value["update"] === "extension" || value["update"] === "player");
    case "connected": {
      const protocol = value["agreedProtocol"];
      return (
        hasOnlyKeys(["state", "playerVersion", "agreedProtocol"]) &&
        typeof value["playerVersion"] === "string" &&
        isRecord(protocol) &&
        Object.keys(protocol).length === 2 &&
        isNonNegativeInteger(protocol["major"]) &&
        isNonNegativeInteger(protocol["minor"])
      );
    }
    default:
      return false;
  }
}

export function isStatusUpdateMessage(value: unknown): value is StatusUpdateMessage {
  return (
    isRecord(value) &&
    Object.keys(value).length === 2 &&
    value["kind"] === "status_update" &&
    isBridgeStatus(value["status"])
  );
}
