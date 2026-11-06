// Отрисовка модели попапа в DOM. Только textContent / createElement: данные страницы (заголовки,
// имена хостов) в innerHTML не попадают. Логики здесь нет — её ведёт `popup-flow.ts`.
//
// Порядок элементов задаёт порядок фокуса по Tab (контракт стенда): сначала кнопки вопроса (Allow первым),
// затем строки выбираемых видео сверху вниз, затем ссылки. Строка, которую выбрать нельзя (в плеере, DRM,
// not supported yet), — не кнопка и в обход Tab не попадает.

import type { PopupFlow, PopupModel } from "./popup-flow.ts";
import type { VideoRowView } from "./video-row-text.ts";

function textElement<Tag extends keyof HTMLElementTagNameMap>(tag: Tag, text: string, className?: string): HTMLElementTagNameMap[Tag] {
  const element = document.createElement(tag);
  element.textContent = text;
  if (className !== undefined) element.className = className;
  return element;
}

function renderNotice(model: PopupModel): HTMLElement {
  const block = document.createElement("section");
  block.className = "notice";
  // Стабильное имя случая для стенда: робот читает исход, не разбирая английский текст.
  block.dataset["notice"] = model.notice.id;
  block.append(textElement("h1", model.notice.headline));
  for (const detail of model.notice.details) block.append(textElement("p", detail));
  return block;
}

function renderQuestion(model: PopupModel, flow: PopupFlow): HTMLElement | undefined {
  if (model.question === null) return undefined;
  const block = document.createElement("section");
  block.className = "consent";
  block.append(textElement("p", model.question.text));
  const allow = textElement("button", model.question.allowLabel, "consent-allow");
  allow.type = "button";
  allow.disabled = model.busy;
  allow.addEventListener("click", () => void flow.allowLocalAddress());
  const deny = textElement("button", model.question.denyLabel, "consent-deny");
  deny.type = "button";
  deny.disabled = model.busy;
  deny.addEventListener("click", () => flow.declineLocalAddress());
  block.append(allow, deny);
  return block;
}

/** Содержимое строки: заголовок, подробности, метки. */
function rowParts(row: VideoRowView): HTMLElement[] {
  const parts: HTMLElement[] = [textElement("span", row.title, "video-title")];
  if (row.details !== "") parts.push(textElement("span", row.details, "video-details"));
  for (const label of row.labels) parts.push(textElement("span", label, "video-label"));
  return parts;
}

function renderRow(row: VideoRowView, busy: boolean, flow: PopupFlow): HTMLElement {
  const item = document.createElement("li");
  if (row.selectable) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "video-row";
    button.disabled = busy;
    button.append(...rowParts(row));
    button.addEventListener("click", () => void flow.chooseVideo(row.ref));
    item.append(button);
  } else {
    const block = document.createElement("div");
    block.className = "video-row-static";
    block.append(...rowParts(row));
    item.append(block);
  }
  return item;
}

function renderList(model: PopupModel, flow: PopupFlow): HTMLElement | undefined {
  if (model.rows.length === 0) return undefined;
  const block = document.createElement("section");
  block.className = "videos";
  block.append(textElement("h2", "Videos on this page"));
  const list = document.createElement("ul");
  list.append(...model.rows.map((row) => renderRow(row, model.busy, flow)));
  block.append(list);
  return block;
}

/** Нарисовать модель в корень; прежнее содержимое заменяется целиком. */
export function renderPopup(root: HTMLElement, model: PopupModel, flow: PopupFlow): void {
  const children = [renderNotice(model), renderQuestion(model, flow), renderList(model, flow)];
  root.replaceChildren(...children.filter((child): child is HTMLElement => child !== undefined));
}
