// Попап: открывает порт «bridge-status» (это и есть сигнал «связь нужна») и перерисовывается на каждое обновление.
import { BRIDGE_STATUS_PORT_NAME, isStatusUpdateMessage } from "../shared/internal-messages.ts";
import { describeStatus, HELP_URL, type PopupView } from "./status-text.ts";

// Рисуем только через textContent / createElement — данные в innerHTML не попадают.
function render(root: HTMLElement, view: PopupView): void {
  const headline = document.createElement("h1");
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

const root = document.getElementById("status-root");
if (root === null) throw new Error("popup.html has no #status-root element");

render(root, describeStatus({ state: "unknown" }));
const port = chrome.runtime.connect({ name: BRIDGE_STATUS_PORT_NAME });
port.onMessage.addListener((message: unknown) => {
  if (!isStatusUpdateMessage(message)) {
    console.warn("popup: ignored malformed message from background", message);
    return;
  }
  render(root, describeStatus(message.status));
});
