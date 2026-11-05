// Защищённый MediaSource не копит память (09c, инвариант 4): передавать такое видео мы не будем,
// а «память без копий» (журнал) держала бы ссылки на каждый кусок, пока идёт просмотр.
//
// Две ситуации, обе закрывает этот модуль:
//  1. элемент стал защищённым, когда его MediaSource уже был → журнал сразу отпускается;
//  2. MediaSource подключают к УЖЕ защищённому элементу (порядок EME: сначала `setMediaKeys`, потом
//     `src`) → журнал отпускается в момент появления первого SourceBuffer, до первого куска.
// Журнал после `discard()` остаётся рабочим объектом (события сворачиваются в снимок, байты не
// копятся), поэтому остальной код не должен знать, что MediaSource защищён.
//
// Что НЕ покрыто: MediaSource в воркере — его журнал живёт внутри воркера, а EME работает на элементе
// главного потока. Память воркера ограничена тем же потолком документа и снимается его обычным путём.

import type { DomReaders } from "./dom-readers.ts";
import type { MediaSourceRecord, MseListener } from "./mse-tracker.ts";
import type { OddityReporter } from "./prototype-wrap.ts";
import type { ReplayItem } from "./journal/media-journal.ts";

/** Что модулю нужно от владельца меток DRM. */
export interface ProtectedElementSource {
  /** Помеченные (ещё живые) элементы. */
  markedElements(): object[];
}

/** Что модулю нужно от описания кандидатов. */
export interface LiveMediaSourceLookup {
  /** Живой MediaSource элемента (`null` — нет). */
  liveMediaSourceOf(element: Element): MediaSourceRecord | null;
}

export class ProtectedMediaRelease {
  readonly #marks: ProtectedElementSource;
  readonly #dom: DomReaders;
  readonly #lookup: LiveMediaSourceLookup;
  readonly #reportOddity: OddityReporter;

  constructor(marks: ProtectedElementSource, dom: DomReaders, lookup: LiveMediaSourceLookup, reportOddity: OddityReporter) {
    this.#reportOddity = reportOddity;
    this.#marks = marks;
    this.#dom = dom;
    this.#lookup = lookup;
  }

  /** Элемент стал защищённым: отпустить журнал его текущего MediaSource (если он есть). */
  releaseForElement(element: object): void {
    this.#lookup.liveMediaSourceOf(element as Element)?.journal.discard();
  }

  /**
   * Слушатель трекера, который перед `inner` отпускает журнал нового MediaSource, если тот
   * подключён к защищённому элементу (ситуация 2). Остальное пропускает как есть.
   */
  wrapListener(inner: MseListener): MseListener {
    return {
      onMediaSourceStarted: (record) => {
        this.#releaseIfAttachedToProtected(record);
        inner.onMediaSourceStarted(record);
      },
      onItem: (record: MediaSourceRecord, item: ReplayItem) => inner.onItem(record, item),
    };
  }

  #releaseIfAttachedToProtected(record: MediaSourceRecord): void {
    if (record.objectUrl === null) return;
    for (const element of this.#marks.markedElements()) {
      // Сбой чтения одного элемента не должен прервать учёт MediaSource: он идёт внутри хука страницы.
      try {
        if (this.#dom.mediaSourceUrlsOf(element).includes(record.objectUrl)) {
          record.journal.discard();
          return;
        }
      } catch {
        this.#reportOddity("could not match a media source to a protected element");
      }
    }
  }
}
