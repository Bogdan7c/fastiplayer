// Мышь настоящего документа для кнопки поверх видео. Отдельный файл, чтобы параметры подписки
// (capture, isTrusted) проверялись тестом на фейковой цели.
import type { PointerSource } from "./overlay-button-controller.ts";

/** Цель событий (в проде — `document`). */
export interface PointerEventTargetLike {
  addEventListener(type: string, listener: (event: PointerEventLike) => void, options: { capture: boolean; passive: boolean }): void;
  removeEventListener(type: string, listener: (event: PointerEventLike) => void, options: { capture: boolean }): void;
}

export interface PointerEventLike {
  readonly isTrusted: boolean;
  readonly clientX?: number;
  readonly clientY?: number;
  readonly target: unknown;
  /** Куда ушла мышь; `null` — за пределы документа. Есть только у `mouseleave`. */
  readonly relatedTarget?: unknown;
}

/**
 * - `mousemove` слушаем в capture: плееры, гасящие движение в своём контейнере, иначе лишили бы нас событий.
 * - Только настоящие (`isTrusted`) движения: страница иначе заставляла бы MAIN обходить DOM синтетикой.
 * - `mouseleave` не всплывает, поэтому слушаем его в capture на самом документе: корневой элемент не нужен
 *   (его может ещё не быть), и подписка не может бросить. Но в capture документ видит уход мыши с ЛЮБОГО
 *   элемента, в том числе с видео на нашу же кнопку. Мышь потеряна только при `relatedTarget === null`:
 *   она вышла за пределы документа. Иначе кнопка пряталась бы прямо под курсором перед кликом.
 */
export function createDocumentPointerSource(target: PointerEventTargetLike): PointerSource {
  return {
    onMouseMove(listener) {
      const handler = (event: PointerEventLike): void => {
        if (!event.isTrusted) return;
        listener({ x: event.clientX ?? 0, y: event.clientY ?? 0, target: event.target });
      };
      target.addEventListener("mousemove", handler, { capture: true, passive: true });
      return () => target.removeEventListener("mousemove", handler, { capture: true });
    },
    onPointerLost(listener) {
      const handleLeave = (event: PointerEventLike): void => {
        if (event.relatedTarget === null) listener();
      };
      const handleScroll = (): void => listener();
      target.addEventListener("mouseleave", handleLeave, { capture: true, passive: true });
      target.addEventListener("scroll", handleScroll, { capture: true, passive: true });
      return () => {
        target.removeEventListener("mouseleave", handleLeave, { capture: true });
        target.removeEventListener("scroll", handleScroll, { capture: true });
      };
    },
  };
}
