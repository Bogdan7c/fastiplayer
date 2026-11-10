// Настоящий взгляд адаптеров вариантов на документ: взгляд читателей рекламы плюс чтение атрибутов и
// «нажатие как человек». Логики решений здесь нет (она в адаптерах); в тестах этот файл заменяет подделка.

import type { AdElement } from "../ad-organs/contract.ts";
import { createDomAdPageView } from "../ad-organs/dom-view.ts";
import type { VariantPageView } from "./contract.ts";

/**
 * Порядок событий одного нажатия левой кнопкой, как у настоящей мыши: указатель нажат, мышь нажата,
 * указатель отпущен, мышь отпущена, щелчок. Плееры слушают разные из них (PlayerJS — нажатие), поэтому
 * шлём всю цепочку.
 */
const PERSON_PRESS_EVENT_TYPES = ["pointerdown", "mousedown", "pointerup", "mouseup", "click"] as const;

/** DOM-элемент изолированного мира (см. `ad-organs/dom-view.ts`). */
function asElement(element: AdElement): HTMLElement {
  return element as unknown as HTMLElement;
}

/**
 * Нажатие как человек: события `MouseEvent` над серединой элемента. `detail`-объектов в событиях нет,
 * поэтому Firefox пропускает их из контент-скрипта к обработчикам страницы без `cloneInto`
 * (MDN, «Sharing objects with page scripts»). Тип `MouseEvent` для `pointer*` — так проверено вживую
 * на PlayerJS (разведка 15b); обработчики страницы читают координаты и кнопку, которые у него есть.
 */
function pressLikePerson(element: AdElement, windowToRead: Window): void {
  const target = asElement(element);
  const box = target.getBoundingClientRect();
  const clientX = box.left + box.width / 2;
  const clientY = box.top + box.height / 2;
  for (const type of PERSON_PRESS_EVENT_TYPES) {
    // `button: 0` — левая кнопка; `bubbles` — сайт часто слушает на предке пункта.
    target.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, view: windowToRead, clientX, clientY, button: 0 }));
  }
}

export function createDomVariantPageView(documentToRead: Document, windowToRead: Window): VariantPageView {
  return {
    ...createDomAdPageView(documentToRead, windowToRead),
    attributeOf: (element, name) => asElement(element).getAttribute(name),
    pressLikePerson: (element) => pressLikePerson(element, windowToRead),
  };
}
