// Чистая функция «статус -> текст попапа» (английский, пользовательский). DOM тут не трогаем.
import type { BridgeStatus } from "../shared/bridge-status.ts";

export const HELP_URL = "https://github.com/Bogdan7c/fastiplayer/blob/main/docs/browser-bridge/README.md";

export type PopupView = {
  readonly headline: string;
  readonly details: readonly string[]; // абзацы пояснений
  readonly showHelpLink: boolean;
};

const HOW_TO_CONNECT: readonly string[] = [
  "Open Fastiplayer, go to Settings › Browser integration and press Connect.",
  "If your browser is installed from Flatpak, also press Allow there, then restart the browser.",
];

export function describeStatus(status: BridgeStatus): PopupView {
  switch (status.state) {
    case "unknown":
    case "checking":
      return { headline: "Checking connection…", details: [], showHelpLink: false };
    case "connected":
      return { headline: `Connected to Fastiplayer ${status.playerVersion}`, details: [], showHelpLink: false };
    case "host_missing":
      return { headline: "Fastiplayer is not connected", details: HOW_TO_CONNECT, showHelpLink: true };
    case "player_unavailable":
      return {
        headline: "Fastiplayer is not connected",
        details: [
          status.reason === "player_not_responding"
            ? "Fastiplayer did not respond. Try restarting it."
            : status.reason === "player_shutting_down"
              ? "Fastiplayer is shutting down. Start it again to continue."
              : "Fastiplayer could not be started.",
          ...HOW_TO_CONNECT,
        ],
        showHelpLink: true,
      };
    case "incompatible":
      return {
        headline: "Fastiplayer and the extension need to match",
        details: [
          status.update === "extension"
            ? "Please update the Fastiplayer Bridge extension."
            : "Please update Fastiplayer.",
        ],
        showHelpLink: true,
      };
  }
}
