// Попап: при открытии сам отдаёт видео активной вкладки (решение 209), показывает исход и список найденного,
// а статус связи с плеером остаётся отдельной секцией. Открытый порт «bridge-status» — это и есть сигнал
// «связь нужна»; закрытие попапа закрывает порт, но НЕ отменяет передачу: её держит фон.
import { autoHandoffSettingsFromChange } from "../shared/auto-handoff-settings.ts";
import { BRIDGE_STATUS_PORT_NAME, isStatusUpdateMessage } from "../shared/internal-messages.ts";
import { createBackgroundLink } from "./background-link.ts";
import { createPopupFlow } from "./popup-flow.ts";
import { mountSiteRuleRow } from "./site-rule-row.ts";
import { renderPopup } from "./popup-view.ts";
import { describeStatus, HELP_URL, type PopupView } from "./status-text.ts";

// Рисуем только через textContent / createElement — данные в innerHTML не попадают.
function renderStatus(root: HTMLElement, view: PopupView): void {
  const headline = document.createElement("h2");
  headline.textContent = view.headline;
  const children: HTMLElement[] = [headline];
  for (const detail of view.details) {
    const paragraph = document.createElement("p");
    paragraph.textContent = detail;
    children.push(paragraph);
  }
  if (view.showHelpLink) {
    const link = document.createElement("a");
    link.href = HELP_URL;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.textContent = "Help: connecting Fastiplayer to your browser";
    const wrapper = document.createElement("p");
    wrapper.append(link);
    children.push(wrapper);
  }
  root.replaceChildren(...children);
}

/** Ссылка на настройки экстеншена: страница настроек откроется во вкладке (`options_ui.open_in_tab`). */
function renderSettingsLink(root: HTMLElement): void {
  const link = document.createElement("a");
  link.href = "#";
  link.className = "settings-link";
  link.textContent = "Extension settings";
  link.addEventListener("click", (event) => {
    event.preventDefault();
    void chrome.runtime.openOptionsPage();
  });
  root.replaceChildren(link);
}

function requireElement(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (element === null) throw new Error(`popup.html has no #${id} element`);
  return element;
}

const flowRoot = requireElement("flow-root");
const siteRuleRoot = requireElement("site-rule-root");
const statusRoot = requireElement("status-root");
renderSettingsLink(requireElement("settings-root"));

// Статус связи: отдельная секция со своим портом.
renderStatus(statusRoot, describeStatus({ state: "unknown" }));
const port = chrome.runtime.connect({ name: BRIDGE_STATUS_PORT_NAME });
port.onMessage.addListener((message: unknown) => {
  if (!isStatusUpdateMessage(message)) {
    console.warn("popup: ignored malformed message from background", message);
    return;
  }
  renderStatus(statusRoot, describeStatus(message.status));
});

// Передача видео: шаги ведёт чистая логика, здесь только настоящие зависимости браузера.
const link = createBackgroundLink();
const flow = createPopupFlow({
  findActiveTab: link.findActiveTab,
  surveyTab: link.surveyTab,
  handOffTab: link.handOffTab,
  isLateEntryRunning: link.isLateEntryRunning,
  reloadAndOpen: link.reloadAndOpen,
  closePopup: () => window.close(),
  render: (model) => renderPopup(flowRoot, model, flow),
});
void flow.start();

// Правило автозапуска для сайта вкладки: отдельный корень после списка видео и до ссылок.
void mountSiteRuleRow(siteRuleRoot, {
  findActiveTabUrl: async () => (await link.findActiveTab())?.url,
  store: {
    get: (keys) => chrome.storage.local.get([...keys]),
    set: (items) => chrome.storage.local.set(items),
  },
  onSettingsChanged: (listener) =>
    chrome.storage.onChanged.addListener((changes, area) => {
      if (area !== "local") return;
      const settings = autoHandoffSettingsFromChange(changes);
      if (settings !== undefined) listener(settings);
    }),
}).catch((error: unknown) => console.warn("popup: site rule row failed", error));
