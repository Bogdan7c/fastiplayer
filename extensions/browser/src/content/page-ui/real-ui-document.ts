// Переходник: настоящий `document` -> узкий `UiDocument`. Единственное место слоя страницы, знающее про
// браузерный DOM; остальное тестируется фейками.
import type { UiDocument, UiDocumentRoot, UiElement, UiHostElement } from "./ui-dom.ts";

const SVG_NAMESPACE = "http://www.w3.org/2000/svg";

/** Иконка «играть»: белый треугольник. Инлайновый SVG — без `web_accessible_resources` и новых прав. */
function createPlayIcon(document: Document): UiElement {
  const svg = document.createElementNS(SVG_NAMESPACE, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("width", "14");
  svg.setAttribute("height", "14");
  svg.setAttribute("aria-hidden", "true");
  const triangle = document.createElementNS(SVG_NAMESPACE, "path");
  triangle.setAttribute("d", "M3 1.5v13l11-6.5z");
  triangle.setAttribute("fill", "#ffffff");
  svg.append(triangle);
  return svg as unknown as UiElement;
}

export function createRealUiDocument(document: Document): UiDocument {
  return {
    documentRoot: () => (document.documentElement as unknown as UiDocumentRoot | null),
    createHost: (tagName) => document.createElement(tagName) as unknown as UiHostElement,
    createElement: (tagName) => document.createElement(tagName) as unknown as UiElement,
    createPlayIcon: () => createPlayIcon(document),
  };
}
