// Отладочный вариант шва записи (подставляется вместо `page/recorder-seam.ts`, см. там): собирает
// запись потока страницы для `page/main.ts`. Всё, что может не получиться (снять функции DOM для
// скачивания), здесь и не роняет мост — нет записи, мост работает как обычно.

import type { OddityReporter } from "../prototype-wrap.ts";
import type { createPageRecording as productSeam } from "../recorder-seam.ts";
import { createAnchorDownloader, type DownloadScope } from "./file-download.ts";
import type { RecordedBrowser } from "./format.ts";
import { PageRecording, type PageRecordingWindow } from "./page-recording.ts";

/** Целевой браузер подставляет сборка (`define` в scripts/build.ts, только в отладочной сборке). */
declare const __FASTIPLAYER_STREAM_RECORDER_BROWSER__: string;

export function createPageRecording(win: Window & typeof globalThis, reportOddity: OddityReporter): PageRecording | null {
  const browser = __FASTIPLAYER_STREAM_RECORDER_BROWSER__;
  if (browser !== "firefox" && browser !== "chrome") {
    reportOddity("stream recorder: unknown target browser");
    return null;
  }
  const recordedBrowser: RecordedBrowser = browser;
  try {
    return new PageRecording(win as unknown as PageRecordingWindow, {
      browser: recordedBrowser,
      downloader: createAnchorDownloader(win as unknown as DownloadScope),
      reportOddity,
      wallClockMs: Date.now.bind(Date),
    });
  } catch {
    reportOddity("stream recorder failed to start");
    return null;
  }
}

/** Проверка на этапе типов: отладочный шов совместим с продуктовым. */
export const matchesProductSeam: typeof productSeam = createPageRecording;
