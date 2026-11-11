// Сторож «страница забрала элемент у тени»: всё, что решает, когда смена источника или судьба
// элемента на странице означает конец тени (`element_released`).
//
// Владелец состояния — этот модуль: сторожа «элемент вне документа» и «элемент без источника»
// скрыты за двумя намерениями — «страница сбросила источник» и «тик часов». Контроллер тени
// (`shadow-session.ts`) не знает, какие именно сторожа и допуски за этим стоят; он получает только
// решение «держать тень» или «отдать элемент странице» и сам исполняет отпускание.

import { DetachedElementWatch } from "./detached-element-watch.ts";
import { EmptySourceWatch } from "./empty-source-watch.ts";
import { type ElementOriginals, readOriginal } from "./originals.ts";
import { classifyElementSource, type MediaSourceObjectUrlCheck, type SourceVerdict } from "./source-reset-policy.ts";

/** Решение сторожа: оставить тень на элементе или отдать элемент странице. */
export type SourceTransitionDecision = "keep_shadow" | "release_to_page";

export class SourceTransitionWatch {
  readonly #element: HTMLMediaElement;
  readonly #originals: ElementOriginals;
  readonly #isMediaSourceObjectUrl: MediaSourceObjectUrlCheck;
  readonly #detachedWatch = new DetachedElementWatch();
  readonly #emptySourceWatch = new EmptySourceWatch();

  constructor(element: HTMLMediaElement, originals: ElementOriginals, isMediaSourceObjectUrl: MediaSourceObjectUrlCheck) {
    this.#element = element;
    this.#originals = originals;
    this.#isMediaSourceObjectUrl = isMediaSourceObjectUrl;
  }

  /**
   * Страница запустила «алгоритм загрузки». Чужой (не MediaSource) источник — конец сразу; снятый
   * источник запускает отсчёт допуска; новый MediaSource ожидание снимает. Инвариант тени:
   * элемент без источника — пауза решения страницы, а не конец (Д-1).
   */
  onSourceReset(nowMs: number): SourceTransitionDecision {
    switch (this.#classifySource()) {
      case "not_media_source":
        return "release_to_page";
      case "no_source":
        this.#emptySourceWatch.sourceRemoved(nowMs);
        return "keep_shadow";
      case "media_source":
        this.#emptySourceWatch.mediaSourceArrived();
        return "keep_shadow";
    }
  }

  /**
   * Тик часов: страница забрала элемент — вынула его из документа дольше допуска, оставила без
   * источника дольше допуска или (пока ждали) поставила чужой источник мимо наших переопределений.
   */
  onClockTick(nowMs: number): SourceTransitionDecision {
    if (this.#elementLeftDocument(nowMs)) return "release_to_page";
    return this.#emptySourceWatch.onClockTick(() => this.#classifySource(), nowMs);
  }

  #classifySource(): SourceVerdict {
    return classifyElementSource(this.#element, this.#originals, this.#isMediaSourceObjectUrl);
  }

  /** Элемент вне документа дольше допуска (проверка оригинальным `isConnected` на тике). */
  #elementLeftDocument(nowMs: number): boolean {
    const accessor = this.#originals.isConnected;
    if (accessor === undefined) return false;
    return this.#detachedWatch.onClockTick(readOriginal(accessor, this.#element) === true, nowMs);
  }
}
