// Сигналы «прямоугольник переданного видео мог сдвинуться» для заглушки (сессия 11a). Только подписки на
// браузер; что делать по сигналу (перемерить, показать, спрятать) — решает `placeholder-controller.ts`.
//
// Что сдвигает видео и чем это видно:
//  - размер самого элемента — `ResizeObserver` (сайт перестроил плеер, сменился режим «театр»);
//  - прокрутка страницы ИЛИ любого вложенного блока — `scroll` в фазе захвата на `window` (`scroll` не
//    всплывает, но захват слышит прокрутку любого элемента), пассивный: прокрутку не тормозим;
//  - размер окна — `resize`;
//  - полноэкранный режим страницы — `fullscreenchange`: ещё и повод поднять слой над полноэкранным элементом.
// Сдвиги вёрстки без событий (сосед вырос, баннер вставился выше) ловит периодический перемер контроллера.

/** Почему стоит перемерить. */
export type PlaceholderGeometryChange = "layout_changed" | "fullscreen_changed";

/** Элемент, за которым следим (настоящий `<video>` или хозяин его теневого дерева). */
export type ObservedElement = object;

/** Источник сигналов (в продукте — браузер, в тестах — фейк). */
export interface PlaceholderGeometrySignals {
  /** Подписаться на сигналы для элемента; вернуть отписку. */
  watch(element: ObservedElement, onChange: (change: PlaceholderGeometryChange) => void): () => void;
}

/** Пассивный слушатель захвата: прокрутку любого блока слышим, а её саму не задерживаем. */
const SCROLL_LISTENER_OPTIONS: AddEventListenerOptions = { capture: true, passive: true };
const RESIZE_LISTENER_OPTIONS: AddEventListenerOptions = { passive: true };
/** Полноэкранный режим объявляется на документе; захват — чтобы страница не погасила его до нас. */
const FULLSCREEN_LISTENER_OPTIONS: AddEventListenerOptions = { capture: true };

/** Сигналы настоящего браузера (окно и документ изолированного мира). */
export function createBrowserGeometrySignals(win: Window, note: (message: string, error: unknown) => void): PlaceholderGeometrySignals {
  return {
    watch(element, onChange) {
      const onLayout = (): void => onChange("layout_changed");
      const onFullscreen = (): void => onChange("fullscreen_changed");
      let observer: ResizeObserver | null = null;
      try {
        const created = new ResizeObserver(onLayout);
        created.observe(element as Element);
        observer = created;
      } catch (error) {
        // Нет ResizeObserver или элемент чужого вида: размер всё равно догонит периодический перемер.
        note("placeholder could not observe the video size", error);
      }
      win.addEventListener("scroll", onLayout, SCROLL_LISTENER_OPTIONS);
      win.addEventListener("resize", onLayout, RESIZE_LISTENER_OPTIONS);
      win.document.addEventListener("fullscreenchange", onFullscreen, FULLSCREEN_LISTENER_OPTIONS);
      return () => {
        observer?.disconnect();
        win.removeEventListener("scroll", onLayout, SCROLL_LISTENER_OPTIONS);
        win.removeEventListener("resize", onLayout, RESIZE_LISTENER_OPTIONS);
        win.document.removeEventListener("fullscreenchange", onFullscreen, FULLSCREEN_LISTENER_OPTIONS);
      };
    },
  };
}
