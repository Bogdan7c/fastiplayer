// Сборка режима 2 «автозапуск» (сессия 12a) на настоящих API браузера. Только переходники к `chrome.*`:
// вся логика — в соседних модулях и проверяется на подделках. Слушатели ставятся СИНХРОННО на верхнем уровне
// фона: браузер будит спящий фон именно по уже зарегистрированному слушателю.
//
// Здесь сходятся два потока донесений, которые раньше шли прямо в передачу вкладки:
// - донесение сторожа (`next_video_held`) -> маршрутизатор режима 2 (`next_video` дальше идёт в передачу без изменений);
// - донесение о рекламе -> и сеансу вкладки, и пробе рекламы (проба нужна вкладкам без сеанса).
// И третий, свой (12b): выбор человека на предложении «Ask» -> `autoHandoff.viewerOfferChoice`.

import { browserApi } from "../../shared/browser-api.ts";
import { GRANT_PLAY_CREDIT_KIND, RELEASE_HELD_VIDEO_REQUEST_KIND, type GrantPlayCreditRequest, type ReleaseHeldVideoRequest } from "../../shared/auto-handoff-contract.ts";
import { readAutoHandoffSettings, writeAutoHandoffSettings } from "../../shared/auto-handoff-settings.ts";
import { WITHDRAW_VIEWER_OFFER_REQUEST_KIND, type WithdrawViewerOfferRequest } from "../../shared/viewer-offer-contract.ts";
import type { EntryReporter } from "../entries/entry-reporter.ts";
import { adMirrorBrowser } from "../handoff/browser-wiring.ts";
import { createAdWatchReportListener } from "../handoff/ad-watch-listener.ts";
import { createHeldVideoListener } from "../handoff/hand-off-command.ts";
import type { TabHandoffs } from "../handoff/tab-handoffs.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { createAdProbe } from "./ad-probe.ts";
import { createAutoHandoff } from "./auto-handoff.ts";
import { createViewerOfferChoiceListener } from "./viewer-offer-listener.ts";

/** Только для разработчика: проглоченный сбой не должен пропасть без следа. */
const note = (message: string, error: unknown): void => console.debug(`[fastiplayer] ${message}:`, error);

const storeOfBrowser = {
  get: (keys: readonly string[]) => browserApi.storage.local.get([...keys]),
  set: (items: Record<string, unknown>) => browserApi.storage.local.set(items),
};

/** Поставить режим 2. Вызывается один раз на верхнем уровне фона, после `installTabHandoffs`. */
export function installAutoHandoff(handoffs: TabHandoffs, scheduler: Scheduler, reporter: EntryReporter): void {
  const adProbe = createAdProbe({ scheduler, armAdWatch: (tabId, leaseMs) => adMirrorBrowser.armAdWatch(tabId, leaseMs) });
  const autoHandoff = createAutoHandoff({
    handoffs,
    readSettings: () => readAutoHandoffSettings(storeOfBrowser),
    writeSettings: (settings) => writeAutoHandoffSettings(storeOfBrowser, settings),
    describeTab: (tabId) => browserApi.tabs.get(tabId).then((tab) => ({ url: tab.url }), () => undefined),
    adProbe,
    grantPlayCredit: (tabId, leaseMs) => {
      const request: GrantPlayCreditRequest = { kind: GRANT_PLAY_CREDIT_KIND, lease_ms: leaseMs };
      // Фрейма без нашего скрипта или закрытой вкладки достаточно для отказа: кредит просто не выдан.
      browserApi.tabs.sendMessage(tabId, request).catch((error: unknown) => note("the play credit was not delivered", error));
    },
    releaseHeldVideo: (tabId, frameId, element) => {
      const request: ReleaseHeldVideoRequest = { kind: RELEASE_HELD_VIDEO_REQUEST_KIND, element };
      // Не дошло — страховка изолированного мира сама отпустит видео (`NEW_SESSION_ADOPT_INSURANCE_MS`).
      browserApi.tabs.sendMessage(tabId, request, { frameId }).catch((error: unknown) => note("the held video release was not delivered", error));
    },
    withdrawViewerOffer: (tabId, frameId, element) => {
      const request: WithdrawViewerOfferRequest = { kind: WITHDRAW_VIEWER_OFFER_REQUEST_KIND, element };
      // Не дошло — фрейма уже нет (страница ушла): снимать нечего.
      browserApi.tabs.sendMessage(tabId, request, { frameId }).catch((error: unknown) => note("the viewer offer withdrawal was not delivered", error));
    },
    reporter,
    note,
  });
  browserApi.runtime.onMessage.addListener(
    createHeldVideoListener(browserApi.runtime.id, (tabId, frameId, report) => autoHandoff.heldVideo(tabId, frameId, report), note),
  );
  browserApi.runtime.onMessage.addListener(
    createViewerOfferChoiceListener(browserApi.runtime.id, (tabId, frameId, message) => autoHandoff.viewerOfferChoice(tabId, frameId, message)),
  );
  browserApi.tabs.onRemoved.addListener((tabId) => autoHandoff.tabRemoved(tabId));
  browserApi.runtime.onMessage.addListener(
    createAdWatchReportListener(browserApi.runtime.id, (tabId, frameId, ad) => {
      handoffs.adWatchReport(tabId, frameId, ad);
      adProbe.acceptReport(tabId, frameId, ad);
    }),
  );
}
