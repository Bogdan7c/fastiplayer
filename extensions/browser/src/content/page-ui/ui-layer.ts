// Общий слой страницы экстеншена: ОДИН хост на документ, внутри закрытая тень, в ней плашка и кнопка.
//
// Что обещает слой:
//  - страница не сдвигается: хост имеет нулевой вклад в поток (position: fixed, 0x0, инлайн с !important);
//  - сайт не видит и не стилизует внутренности: тень закрытая, ссылка на неё только в этом мире;
//  - виден поверх всего: `popover="manual"` кладёт хост в верхний слой браузера (поверх полноэкранного
//    видео, независимо от z-index сайта); нет Popover API -> обычный `position: fixed` с огромным z-index;
//  - ленивое создание: до первого показа в документе нет ни одного нашего узла.
import { applyImportantStyles, type UiDocument, type UiHostElement, type UiShadowRoot } from "./ui-dom.ts";

/** Неопределённый custom-тег (с дефисом, но не зарегистрированный): браузер не навешивает на него поведения. */
export const UI_HOST_TAG_NAME = "fastiplayer-bridge-ui";

/** Поверх всего, что сайт может придумать, когда Popover API недоступен (максимум 32-битного z-index). */
const FALLBACK_Z_INDEX = "2147483647";

export interface UiLayer {
  /** Закрытая тень; создаёт хост при первом вызове. `null`, если документ ещё не готов принять хост. */
  shadowRoot(): UiShadowRoot | null;
  /** Хост, если он уже создан (нужен кнопке: событие над ней приходит со `target === host`). */
  host(): UiHostElement | null;
  /** Убедиться, что хост в документе и на самом верху верхнего слоя (после полноэкранного режима сайта и т. п.). */
  raise(): void;
}

export function createUiLayer(document: UiDocument): UiLayer {
  let host: UiHostElement | null = null;
  let shadow: UiShadowRoot | null = null;
  /** Документ не принимает наш хост (XML/SVG: `attachShadow` бросает): больше не пробуем. */
  let unsupported = false;

  /** Построить хост один раз. */
  function buildHost(): UiHostElement {
    const created = document.createHost(UI_HOST_TAG_NAME);
    // `all: initial` стирает всё, что сайт навесил селекторами по тегу; остальное идёт ПОСЛЕ него,
    // иначе `all` затёрло бы их. Нулевые размеры + fixed = ни сдвига вёрстки, ни вклада в размер документа.
    applyImportantStyles(created, {
      all: "initial",
      display: "block",
      position: "fixed",
      top: "0",
      left: "0",
      width: "0",
      height: "0",
      margin: "0",
      padding: "0",
      border: "0",
      overflow: "visible",
      "pointer-events": "none",
      "z-index": FALLBACK_Z_INDEX,
    });
    created.setAttribute("popover", "manual");
    shadow = created.attachShadow({ mode: "closed" });
    return created;
  }

  function raise(): void {
    if (host === null) return;
    if (!host.isConnected) document.documentRoot()?.append(host);
    if (host.isConnected && host.showPopover !== undefined && host.hidePopover !== undefined) {
      // Снять и показать заново: последний показанный popover лежит выше остальных в верхнем слое.
      try {
        host.hidePopover();
      } catch {
        // Уже скрыт (или не был показан) — для нас это то же самое.
      }
      try {
        host.showPopover();
      } catch (error) {
        // Нельзя показать popover (хост не в документе) — остаёмся на запасном position: fixed.
        console.debug("[fastiplayer] popover was not shown:", error);
      }
    }
  }

  return {
    shadowRoot() {
      if (unsupported) return null;
      if (host === null) {
        const root = document.documentRoot();
        if (root === null) return null;
        try {
          host = buildHost();
          root.append(host);
        } catch (error) {
          // Не HTML-документ: тихо ничего не показываем, значок экстеншена всё равно скажет об отказе.
          unsupported = true;
          host = null;
          shadow = null;
          console.debug("[fastiplayer] page UI is not available in this document:", error);
          return null;
        }
        // В верхний слой хост поднимают сами показывающие (`raise`), когда есть что показывать.
      }
      return shadow;
    },
    host: () => host,
    raise,
  };
}
