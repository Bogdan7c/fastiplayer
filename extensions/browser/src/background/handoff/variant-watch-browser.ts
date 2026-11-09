// Браузерная сторона меню вариантов страницы (сессия 14b): аренда наблюдения всем фреймам вкладки, команда —
// одному фрейму. Приём донесений — `variant-watch-listener.ts` (без `browserApi`, его грузят тесты).

import { browserApi } from "../../shared/browser-api.ts";
import {
  ARM_VARIANT_WATCH_KIND,
  VARIANT_COMMAND_KIND,
  type ArmVariantWatchRequest,
  type VariantCommandRequest,
} from "../../shared/variant-watch-contract.ts";
import { parseVariantActionOutcome } from "../../shared/variant-watch-validation.ts";
import type { VariantMirrorBrowser } from "./variant-mirror.ts";

export const variantMirrorBrowser: VariantMirrorBrowser = {
  armVariantWatch(tabId, leaseMs) {
    const request: ArmVariantWatchRequest = { kind: ARM_VARIANT_WATCH_KIND, lease_ms: leaseMs };
    // Без `frameId` — всем фреймам. Фрейма без нашего скрипта или закрытой вкладки достаточно для отказа:
    // следующее продление попробует снова.
    browserApi.tabs.sendMessage(tabId, request).catch(() => {});
  },
  async sendVariantCommand(tabId, frameId, command) {
    const request: VariantCommandRequest = { kind: VARIANT_COMMAND_KIND, command };
    const reply: unknown = await browserApi.tabs.sendMessage(tabId, request, { frameId });
    const outcome = parseVariantActionOutcome(reply);
    if (outcome === undefined) throw new Error("the frame gave an unknown answer to variant_command");
    return outcome;
  },
};
