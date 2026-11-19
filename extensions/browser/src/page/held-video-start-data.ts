// Есть ли у моста данные под секундой, с которой задержанное видео пошло бы в окне (16b3b, решение 378).
//
// Зачем. Прокрутка назад к живому ролику ленты: окно берёт его новым поколением с той секунды, где страница его
// держит (решение 372), а данные — повтором журнала «памяти без копий». Если потолок памяти журнал уже опустошил,
// повтор — один init, страница свои данные второй раз не пришлёт, и окно ждало бы без предела. Страница знает это
// заранее, до передачи: ответ уходит фону в донесении сторожа (`next_video_held.start_data`), а решает фон
// (held-video-choice.ts) — только он знает, переезд это или первая передача.
//
// Здесь только перевод знания журнала (`MediaJournal.startCoverageAt`) на язык договора страница ↔ фон и
// правило для источников без журнала. Модуль никогда не бросает: донесение сторожа идёт внутри `play()`
// страницы, а сбой чтения — это честное «не знаю», при котором переезд ведёт себя как раньше.

import type { HeldVideoStartData } from "../shared/handoff-contract.ts";
import type { ElementSource } from "./element-source.ts";
import type { JournalStartCoverage } from "./journal/media-journal.ts";
import type { OddityReporter } from "./prototype-wrap.ts";

/** Что нужно, чтобы ответить про один элемент. */
export interface HeldVideoStartDataReaders {
  /** Форма источника элемента (чтения могут бросить). */
  readonly sourceOf: (element: Element) => ElementSource;
  /** Позиция страницы, секунды: с неё передача начнёт окно (может бросить, может быть не числом). */
  readonly currentTimeSeconds: (element: Element) => number;
  readonly reportOddity: OddityReporter;
}

/** Ответ для одного задержанного элемента. */
export function heldVideoStartDataOf(element: Element, readers: HeldVideoStartDataReaders): HeldVideoStartData {
  try {
    const source = readers.sourceOf(element);
    switch (source.kind) {
      case "media_source": {
        const positionSeconds = readers.currentTimeSeconds(element);
        // Позиция не прочитана (нет метаданных): с какой секунды пойдёт окно, неизвестно.
        if (!Number.isFinite(positionSeconds)) return "unknown";
        return startDataOfCoverage(source.record.journal.startCoverageAt(positionSeconds));
      }
      case "file":
      case "hls":
        // Файл и плейлист фон качает сам с любой секунды: данных у окна не отнять.
        return "available";
      case "worker_media_source":
        // Журнал живёт в воркере, синхронно его не спросить: честное «не знаю», переезд как раньше.
        return "unknown";
      case "unsupported":
      case "none":
        return "unknown";
    }
  } catch {
    readers.reportOddity("could not tell whether a held video still has its data");
    return "unknown";
  }
}

/** Знание журнала словами договора. */
function startDataOfCoverage(coverage: JournalStartCoverage): HeldVideoStartData {
  switch (coverage) {
    case "covered":
      return "available";
    case "lost":
      return "lost";
    case "unknown":
      return "unknown";
  }
}
