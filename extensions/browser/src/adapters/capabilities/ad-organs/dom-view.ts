// Настоящий взгляд читателей на документ: тонкая обвязка над DOM изолированного мира. Логики решений
// здесь нет (она в читателях); этот файл в тестах заменяет маленькая подделка.

import type { AdElement, AdPageView, ElementRect } from "./contract.ts";

/** DOM-элемент, которым владеет читатель: контракт знает его только как «можно нажать». */
function asElement(element: AdElement): HTMLElement {
  return element as HTMLElement;
}

export function createDomAdPageView(documentToRead: Document, windowToRead: Window): AdPageView {
  return {
    hostname: documentToRead.location?.hostname ?? "",
    contains: (selector) => documentToRead.querySelector(selector) !== null,
    documentRoot: () => documentToRead.documentElement,
    find: (selector, within) => ((within === undefined ? documentToRead : asElement(within)).querySelector<HTMLElement>(selector) ?? undefined),
    findAll: (selector, within) => [...(within === undefined ? documentToRead : asElement(within)).querySelectorAll<HTMLElement>(selector)],
    // `textContent`, а не `innerText`: он не заставляет браузер считать раскладку на каждое чтение.
    textOf: (element) => asElement(element).textContent ?? "",
    hasClass: (element, className) => asElement(element).classList.contains(className),
    hasChildElements: (element) => asElement(element).childElementCount > 0,
    isRendered(element): boolean {
      const domElement = asElement(element);
      // Элемент под `display:none` (свой или предка) не имеет раскладки: прямоугольник пуст.
      const box = domElement.getBoundingClientRect();
      if (box.width <= 0 || box.height <= 0) return false;
      return windowToRead.getComputedStyle(domElement).visibility !== "hidden";
    },
    rectOf(element): ElementRect {
      const { left, top, right, bottom } = asElement(element).getBoundingClientRect();
      return { left, top, right, bottom };
    },
    viewportSize: () => ({ width: windowToRead.innerWidth, height: windowToRead.innerHeight }),
  };
}
