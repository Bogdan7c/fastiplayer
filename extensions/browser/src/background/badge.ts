// Значок на иконке экстеншена: «!» для проблем со связью, пусто для connected/unknown.
import type { BridgeStatus } from "../shared/bridge-status.ts";

export type BadgeAppearance = { readonly text: string; readonly color: string };

const WARNING_COLOR = "#d93025";
const CHECKING_COLOR = "#f9ab00";

// Чистая функция «статус -> значок» (тестируется без браузера).
export function badgeForStatus(status: BridgeStatus): BadgeAppearance {
  switch (status.state) {
    case "connected":
    case "unknown":
      return { text: "", color: WARNING_COLOR };
    case "checking":
      // Идёт проверка: не пугаем человека «!», показываем нейтральную точку.
      return { text: "…", color: CHECKING_COLOR };
    case "host_missing":
    case "player_unavailable":
    case "incompatible":
      return { text: "!", color: WARNING_COLOR };
  }
}

// Тонкая обёртка над chrome.action: ошибки API не глотаем, а пишем в консоль фона.
export function applyBadge(status: BridgeStatus): void {
  const appearance = badgeForStatus(status);
  chrome.action.setBadgeText({ text: appearance.text }).catch((error: unknown) => console.warn("setBadgeText failed", error));
  chrome.action
    .setBadgeBackgroundColor({ color: appearance.color })
    .catch((error: unknown) => console.warn("setBadgeBackgroundColor failed", error));
}
