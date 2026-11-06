// DOM кнопки «Play in Fastiplayer» поверх видео: рисуется в закрытой тени общего слоя. Только вид и
// позиционирование; решения («показывать ли») — в `overlay-button-controller.ts`.
import type { PageUiRect } from "../../shared/entry-contract.ts";
import { placeButtonAnchor, type ViewportSize } from "./button-placement.ts";
import type { OverlayButtonView } from "./overlay-button-controller.ts";
import { applyImportantStyles, type UiDocument, type UiElement, type UiEvent } from "./ui-dom.ts";
import type { UiLayer } from "./ui-layer.ts";

export const OVERLAY_BUTTON_LABEL = "Play in Fastiplayer";

/** События, которые кнопка гасит у себя, чтобы страница не увидела ни одного: иначе её плеер среагирует на «клик по видео». */
const SWALLOWED_EVENT_TYPES = ["mousedown", "mouseup", "pointerdown", "pointerup", "dblclick", "contextmenu"] as const;

export function createOverlayButtonView(document: UiDocument, layer: UiLayer, viewportSize: () => ViewportSize): OverlayButtonView {
  let button: UiElement | null = null;
  let visible = false;
  let pressListener: ((event: UiEvent) => void) | null = null;

  /** Построить кнопку один раз (при первом показе). */
  function buildButton(): UiElement {
    const created = document.createElement("button");
    created.setAttribute("type", "button");
    created.setAttribute("aria-label", OVERLAY_BUTTON_LABEL);
    applyImportantStyles(created, {
      all: "initial",
      position: "fixed",
      display: "flex",
      "align-items": "center",
      gap: "6px",
      padding: "6px 10px",
      background: "rgba(20, 20, 24, 0.88)",
      color: "#ffffff",
      "border-radius": "16px",
      font: "13px/1 system-ui, sans-serif",
      cursor: "pointer",
      "pointer-events": "auto",
      "white-space": "nowrap",
      // Правый верхний угол кнопки = точка привязки; позицию выставляет `show`.
      transform: "translateX(-100%)",
    });
    created.append(document.createPlayIcon());
    const label = document.createElement("span");
    label.textContent = OVERLAY_BUTTON_LABEL;
    created.append(label);
    const swallow = (event: UiEvent): void => {
      event.stopPropagation();
      event.stopImmediatePropagation();
    };
    for (const type of SWALLOWED_EVENT_TYPES) created.addEventListener(type, swallow);
    return created;
  }

  function detachPressListener(): void {
    if (button !== null && pressListener !== null) button.removeEventListener("click", pressListener);
    pressListener = null;
  }

  return {
    show(videoRect: PageUiRect, onPressed) {
      const shadow = layer.shadowRoot();
      if (shadow === null) return;
      if (button === null) {
        button = buildButton();
        shadow.append(button);
      }
      // Каждый показ ставит свежий обработчик нажатия (тот же контроллер, но без накопления дублей).
      detachPressListener();
      pressListener = onPressed;
      button.addEventListener("click", pressListener);
      const anchor = placeButtonAnchor(videoRect, viewportSize());
      applyImportantStyles(button, { display: "flex", left: `${anchor.right}px`, top: `${anchor.top}px` });
      // Вверх верхнего слоя поднимаем только при появлении: цикл hide/show на каждое перемещение мерцает
      // и теряет пару mousedown -> click.
      if (!visible) layer.raise();
      visible = true;
    },
    hide() {
      visible = false;
      // Кнопки ещё не было — слой не создаём: спящее расширение не должно оставлять узлов в документе.
      if (button === null) return;
      applyImportantStyles(button, { display: "none" });
    },
    measure() {
      if (button === null) return null;
      const rect = button.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) return null;
      // Простой объект, а не сам DOMRect: у настоящего DOMRect поля лежат геттерами на прототипе, и результат
      // `executeScript` (клонирование/JSON) приходил пустым `{}` — стенд не видел, где кнопка.
      return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
    },
  };
}
