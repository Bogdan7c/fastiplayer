// Сборка наблюдения за рекламой на настоящих API браузера (изолированный мир, все фреймы). Логика —
// в `ad-watch.ts` и читателях `src/adapters/ad-organs/`; здесь только переходники.
//
// До аренды от фона этот модуль не делает ничего: ни наблюдателей, ни таймеров, ни сообщений.

import { createAllAdOrgans } from "../adapters/ad-organs/index.ts";
import { createDomAdPageView } from "../adapters/ad-organs/dom-view.ts";
import { parseArmAdWatchRequest, parsePressAdSkipRequest } from "../shared/ad-watch-validation.ts";
import { browserApi } from "../shared/browser-api.ts";
import { createAdWatch, type AdMutationWatcher } from "./ad-watch.ts";

/** Какие изменения разметки будят чтение: дерево, текст и атрибуты, которыми сайты прячут и показывают кнопки. */
const OBSERVED_ATTRIBUTES = ["class", "style", "hidden"];

function createDomWatcher(onMutation: () => void): AdMutationWatcher {
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

/** Подключить слежение к этому документу; зовётся один раз при загрузке контент-скрипта. */
export function installAdWatch(): void {
  const view = createDomAdPageView(document, window);
  const watch = createAdWatch({
    organs: createAllAdOrgans(view),
    place: view,
    createWatcher: createDomWatcher,
    timers: {
      setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
      clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    },
    now: () => performance.now(),
    // Фон мог уснуть или умереть: отказ доставки не страшен, следующее продление аренды повторит донесение.
    sendReport: (report) => void Promise.resolve(browserApi.runtime.sendMessage(report)).catch(() => {}),
    reportReaderFailure: (error) => console.debug("[fastiplayer] an ad reader failed:", error),
  });

  // Только наш фон: у него нет `tab`. Чужие экстеншены и страницы сюда не пишут, но отправителя проверяем.
  browserApi.runtime.onMessage.addListener((message: unknown, sender, sendResponse) => {
    if (sender.id !== browserApi.runtime.id || sender.tab !== undefined) return false;
    const arm = parseArmAdWatchRequest(message);
    if (arm !== undefined) {
      watch.arm(arm.lease_ms);
      return false;
    }
    if (parsePressAdSkipRequest(message) !== undefined) {
      sendResponse(watch.pressSkip());
    }
    return false;
  });
}
