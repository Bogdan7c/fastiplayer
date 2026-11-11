// Сторож «страница забрала элемент у тени»: всё, что решает, когда смена источника или судьба
// элемента на странице означает конец тени (`element_released`) или ожидание следующей серии (16a).
//
// Владелец состояния — этот модуль: сторожа «элемент вне документа» и «элемент без источника»
// скрыты за двумя намерениями — «страница сбросила источник» и «тик часов». Контроллер тени
// (`shadow-session.ts`) не знает, какие именно сторожа и допуски за этим стоят; он получает только
// решение «держать тень», «держать и сказать, что ждём следующую серию» или «отдать элемент странице
// с такой-то причиной» и сам исполняет отпускание.
//
// Знание «серия доиграла» (часы плеера сказали `ended`) принадлежит модели часов тени
// (`ShadowTimeline.ended`): сторож только спрашивает его в момент снятия источника, до того как
// контроллер повторит «алгоритм загрузки» и модель сбросит `ended` для нового источника.

import type { HandoffEndCause } from "../../shared/handoff-contract.ts";
import { DetachedElementWatch } from "./detached-element-watch.ts";
import { EmptySourceWatch, type SourceRemovalMoment } from "./empty-source-watch.ts";
import { type ElementOriginals, readOriginal } from "./originals.ts";
import { classifyElementSource, type MediaSourceObjectUrlCheck, type SourceVerdict } from "./source-reset-policy.ts";

/** Тень уходит с элемента: почему (из этого фон решит, каким кодом закрыть сеанс). */
export type SourceReleaseDecision =
  /** Страница забрала видео: чужой источник, пустота посреди серии, элемент вне документа. */
  | "release_to_page"
  /** Серия доиграла, а следующую страница не поставила за `NEXT_EPISODE_WAIT_MS`. */
  | "next_episode_not_started";

/** Решение по тику часов: оставить тень или уйти с причиной. */
export type SourceTickDecision = "keep_shadow" | SourceReleaseDecision;

/** Решение по сбросу источника страницей. */
export type SourceResetDecision =
  | "keep_shadow"
  /** Источник снят после конца серии: тень ждёт следующую (сказать фону один раз на ожидание). */
  | "await_next_episode"
  | "release_to_page";

/** Вопрос модели часов «часы плеера уже сказали `ended` для текущего источника?». */
export type EpisodeEndQuery = () => boolean;

/** Причина конца передачи для решения об уходе тени (договор `handoff_ended{cause}`). */
export function endCauseOfRelease(decision: SourceReleaseDecision): Extract<HandoffEndCause, "element_released" | "next_episode_not_started"> {
  return decision === "release_to_page" ? "element_released" : "next_episode_not_started";
}

export class SourceTransitionWatch {
  readonly #element: HTMLMediaElement;
  readonly #originals: ElementOriginals;
  readonly #isMediaSourceObjectUrl: MediaSourceObjectUrlCheck;
  readonly #playerReachedEpisodeEnd: EpisodeEndQuery;
  readonly #detachedWatch = new DetachedElementWatch();
  readonly #emptySourceWatch = new EmptySourceWatch();

  constructor(element: HTMLMediaElement, originals: ElementOriginals, isMediaSourceObjectUrl: MediaSourceObjectUrlCheck, playerReachedEpisodeEnd: EpisodeEndQuery) {
    this.#element = element;
    this.#originals = originals;
    this.#isMediaSourceObjectUrl = isMediaSourceObjectUrl;
    this.#playerReachedEpisodeEnd = playerReachedEpisodeEnd;
  }

  /**
   * Страница запустила «алгоритм загрузки». Чужой (не MediaSource) источник — конец сразу; снятый
   * источник запускает отсчёт допуска (короткого посреди серии, долгого после её конца); новый
   * MediaSource ожидание снимает. Инвариант тени: элемент без источника — пауза решения страницы, а
   * не конец (Д-1). Зовётся ДО того, как модель часов сбросит `ended` для нового источника.
   */
  onSourceReset(nowMs: number): SourceResetDecision {
    switch (this.#classifySource()) {
      case "not_media_source":
        return "release_to_page";
      case "no_source": {
        const effect = this.#emptySourceWatch.sourceRemoved(nowMs, this.#removalMoment());
        return effect === "next_episode_wait_began" ? "await_next_episode" : "keep_shadow";
      }
      case "media_source":
        this.#emptySourceWatch.mediaSourceArrived();
        return "keep_shadow";
    }
  }

  /**
   * Тик часов: страница забрала элемент — вынула его из документа дольше допуска, оставила без
   * источника дольше допуска или (пока ждали) поставила чужой источник мимо наших переопределений;
   * или серия доиграла, а следующая не пришла в срок.
   */
  onClockTick(nowMs: number): SourceTickDecision {
    if (this.#elementLeftDocument(nowMs)) return "release_to_page";
    return this.#emptySourceWatch.onClockTick(() => this.#classifySource(), nowMs);
  }

  /** Момент снятия источника: доиграла ли серия у плеера (владелец знания — модель часов). */
  #removalMoment(): SourceRemovalMoment {
    return this.#playerReachedEpisodeEnd() ? "after_episode_end" : "mid_episode";
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
