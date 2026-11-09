// Настоящий взгляд адаптеров вариантов на документ: взгляд читателей рекламы плюс чтение атрибутов.
// Логики решений здесь нет (она в адаптерах); в тестах этот файл заменяет подделка.

import { createDomAdPageView } from "../ad-organs/dom-view.ts";
import type { VariantPageView } from "./contract.ts";

export function createDomVariantPageView(documentToRead: Document, windowToRead: Window): VariantPageView {
  return {
    ...createDomAdPageView(documentToRead, windowToRead),
    // Элемент контракта — DOM-элемент изолированного мира (см. `ad-organs/dom-view.ts`).
    attributeOf: (element, name) => (element as unknown as HTMLElement).getAttribute(name),
  };
}
