// Шов MAIN-мира с «тенью» (пакет A2, `./shadow/index.ts`) — как его видит контроллер передачи.
//
// Контроллеру не нужно знать окружение тени (оригиналы свойств элемента, гашение mediaSession,
// таймеры): это собирает `main.ts` один раз на document_start и отдаёт сюда готовую функцию.
// Так контроллер тестируется с фейковой тенью, а настоящая подключается в одном месте.

import type { HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../shared/handoff-contract.ts";

/** Отложенное обещание `play()`, отданное странице; его исполнит тень. */
export interface PendingPlay {
  readonly resolve: () => void;
  readonly reject: (reason: unknown) => void;
}

/** Запущенная тень одного элемента (подмножество `ShadowSession` из `./shadow/index.ts`). */
export interface ShadowSessionHandle {
  apply(command: ShadowCommand): void;
  /** Снять тень; тень шлёт `handoff_ended{returned}` через свой `emit`. */
  end(position: HandoffEndPosition, resume: HandoffEndResume): void;
}

/** Точный ответ реестра MAIN-мира: адрес создан `URL.createObjectURL` для MediaSource документа. */
export type MediaSourceObjectUrlCheck = (url: string) => boolean;

/**
 * Что передаётся под тенью (сессия 09b):
 * - `media_source` — MSE: настоящий элемент остаётся со своим MediaSource (браузер раскладывает
 *   куски, `buffered`/`duration` настоящие), только стоит на паузе;
 * - `file` — файл `<video src>`: источник настоящего элемента снимается (решение 186), браузер файл
 *   не качает и не декодирует, а тень показывает странице снимок файла (длительность, размер
 *   кадра, `buffered` = весь файл — решение 187) и при возврате незаметно возвращает источник;
 * - `live_hls` — эфир нативного HLS (10a3, решение 232): источник снят, как у файла, но страница видит
 *   эфир — `duration = Infinity`, окно перемотки по `live_window` плеера, время плеера со сдвигом к
 *   секунде страницы при передаче; возврат перезагружает `src` и оставляет элемент там, где браузер
 *   сам встаёт в эфире, а не на T.
 */
export type ShadowSourceKind = "media_source" | "file" | "live_hls";

/** Параметры запуска тени. */
export interface ShadowStartOptions {
  /**
   * Знание реестра: адрес — blob-адрес MediaSource этого документа (тень по нему отличает новое
   * поколение на том же элементе от blob-адреса обычного файла).
   */
  readonly isMediaSourceObjectUrl: MediaSourceObjectUrlCheck;
  readonly source: ShadowSourceKind;
  /**
   * Только для задержанной тени: страница уже просила играть, а настоящий `play()` сторож не звал (взвод
   * возврата страницы, 11b2) — тогда модель тени начинает «играющей», хотя настоящий элемент стоит.
   * Нет поля — как настоящий элемент.
   */
  readonly initialPlayback?: HeldShadowPlayback;
}

/** С чего начинает модель задержанной тени: как настоящий элемент или «страница просила играть». */
export type HeldShadowPlayback = "as_real_element" | "page_asked_to_play";

/** Поставить элемент в тень. `emit` — события страницы (page_seek, page_playback, handoff_ended, …). */
export type ShadowStarter = (element: HTMLMediaElement, emit: (event: PageEvent) => void, options: ShadowStartOptions) => ShadowSessionHandle;

/**
 * Тень, поставленная сторожем следующего видео ДО решения фона (сессия 09c2). Настоящий элемент уже
 * стоит и молчит, а файл ещё не снят: если фон откажет, элемент просто заиграет дальше, без перезагрузки.
 */
export interface HeldShadowHandle extends ShadowSessionHandle {
  /** Фон взял видео: снять файл с настоящего элемента и ждать часы нового источника. */
  commitHold(): void;
  /** Страница видит видео играющим. */
  pageSawPlaying(): boolean;
  /** Забрать обещания `play()`, которые страница получила до тени. */
  takeOverPendingPlays(plays: readonly PendingPlay[]): void;
  /**
   * Предложение «Ask» (сессия 12b): модель тени показывает странице паузу (пока человек выбирает) или снова игру.
   * Настоящий элемент не трогается — он и так стоит. Сбой гасится внутри тени (след — в её репортёре).
   */
  showHeldPagePlayback(action: "play" | "pause"): void;
}

/** Поставить тень на задержанное видео. */
export type HeldShadowStarter = (element: HTMLMediaElement, emit: (event: PageEvent) => void, options: ShadowStartOptions) => HeldShadowHandle;

/**
 * Что сторож отвечает на `play()` элемента не в тени:
 * - `not_mine` — обычный вызов браузера;
 * - `now_shadowed` — на элемент только что встала тень, отвечает она;
 * - `answered` — сторож сам позвал настоящий `play()` и отдаёт странице своё обещание.
 */
export type PlayWatchAnswer = { readonly kind: "not_mine" } | { readonly kind: "now_shadowed" } | { readonly kind: "answered"; readonly promise: unknown };

/** События настоящего элемента, которые слушает шов. */
export type RealMediaEventType = "play" | "loadedmetadata" | "seeked";

/** Наблюдатель за `play()` страницы. Ошибка наблюдателя не должна долететь до страницы. */
export type PlayWatcher = (element: object, realPlay: () => unknown) => PlayWatchAnswer;

/** Шов сторожа с переопределениями прототипов и событиями настоящих элементов. */
export interface PlayWatchSeam {
  /** Ставится один раз; до этого `play()` строго прозрачен. */
  watchPlayCalls(watcher: PlayWatcher): void;
  /** Стоит ли тень на элементе. */
  isShadowed(element: object): boolean;
  /**
   * Доверенные `play`, `loadedmetadata` и `seeked` настоящих элементов (захватом на `target`): окно видит
   * всё, кроме теневого DOM, поэтому `play()` такого элемента подписывает и его самого. Вторым аргументом
   * слушатель получает вид события: режиму 2 (12a) `seeked` ничего не начинает, а `play` и `loadedmetadata`
   * судятся по-разному.
   */
  onRealMediaEvent(target: object, listener: (element: object, eventType: RealMediaEventType) => void): void;
  /**
   * Настоящая перемотка элемента НЕ в тени (возврат страницы, 11b2): оригинальный сеттер `currentTime`.
   * Страница видит обычную перемотку (`seeking`/`seeked`), и её плеер сам качает данные с этой секунды.
   */
  seekRealElement(element: object, seconds: number): void;
  /** Идёт ли настоящая перемотка элемента (оригинальный геттер `seeking`; нет геттера — `false`). */
  isSeekingReal(element: object): boolean;
  /** `Promise` мира страницы: страница получает обещание своего мира. */
  readonly PagePromise: PromiseConstructor;
}
