// Сборка наблюдения за вариантами страницы на настоящих API браузера (изолированный мир, все фреймы).
// Логика — в `variant-watch.ts` и адаптерах `src/adapters/sites/*/variants.ts`; здесь только переходники.
//
// До аренды от фона этот модуль не делает ничего: ни наблюдателей, ни таймеров, ни сообщений.

import { createDomVariantPageView } from "../adapters/capabilities/variants/dom-view.ts";
import { createAllVariantAdapters } from "../adapters/registry/isolated-world.ts";
import { describeSiteAdapter } from "../adapters/site-identity.ts";
import { browserApi } from "../shared/browser-api.ts";
import { parseArmVariantWatchRequest, parseVariantCommandRequest } from "../shared/variant-watch-validation.ts";
import type { AdapterHandClient } from "./adapter-hand-client.ts";
import { createVariantWatch, type VariantMutationWatcher } from "./variant-watch.ts";

/** Какие изменения разметки будят чтение: дерево, текст и атрибуты, которыми сайты отмечают выбранный пункт. */
const OBSERVED_ATTRIBUTES = ["class", "style", "hidden", "aria-pressed", "aria-checked", "aria-selected"];

function createDomWatcher(onMutation: () => void): VariantMutationWatcher {
  const observer = new MutationObserver(() => onMutation());
  return {
    observe: (root) =>
      observer.observe(root as unknown as Node, {
        childList: true,
        subtree: true,
        characterData: true,
        attributes: true,
        attributeFilter: OBSERVED_ATTRIBUTES,
      }),
    disconnect: () => observer.disconnect(),
  };
}

/** Подключить наблюдение к этому документу; зовётся один раз при загрузке контент-скрипта. */
export function installVariantWatch(hand: AdapterHandClient): void {
  const view = createDomVariantPageView(document, window);
  const watch = createVariantWatch({
    adapters: createAllVariantAdapters((identity) => ({ dom: view, player: hand.handFor(identity.id) })),
    place: view,
    createWatcher: createDomWatcher,
    timers: {
      setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
      clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    },
    now: () => performance.now(),
    // Фон мог уснуть или умереть: отказ доставки не страшен, следующее продление аренды повторит донесение.
    sendReport: (report) => void Promise.resolve(browserApi.runtime.sendMessage(report)).catch(() => {}),
    // Журнал разработчика: паспорт адаптера (решение 283) без адресов и названий страницы.
    noteAdapterFailure: (identity, error) => console.debug(`[fastiplayer] variant adapter ${describeSiteAdapter(identity)} failed:`, error),
  });

  // Только наш фон: у него нет `tab`. Ответ на команду асинхронный — `true` держит канал открытым.
  browserApi.runtime.onMessage.addListener((message: unknown, sender, sendResponse) => {
    if (sender.id !== browserApi.runtime.id || sender.tab !== undefined) return false;
    const arm = parseArmVariantWatchRequest(message);
    if (arm !== undefined) {
      watch.arm(arm.lease_ms);
      return false;
    }
    const request = parseVariantCommandRequest(message);
    if (request === undefined) return false;
    void watch.command(request.command).then(sendResponse, () => sendResponse("failed"));
    return true;
  });
}
