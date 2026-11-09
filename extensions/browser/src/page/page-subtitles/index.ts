// Сборка субтитров страницы (сессия 13b2): снять оригиналы, поставить хуки `fetch`/XHR, завести склад.
//
// Зовётся из `bridge.ts` на document_start, как и остальные хуки. Каждая часть ставится независимо:
// нет `fetch`, XHR или типов `<track>` в окружении — работает то, что есть; сбой установки одной части
// не мешает остальным и странице (одна отладочная запись).

import type { BinaryIntrinsics } from "../binary-view.ts";
import type { ChannelOriginals } from "../page-channel.ts";
import type { ForwardByteMeter } from "../forward-byte-meter.ts";
import type { OddityReporter } from "../prototype-wrap.ts";
import { captureByteTools, type ByteToolsWindow } from "./byte-tools.ts";
import { SubtitleDownloadIntake } from "./download-intake.ts";
import { DownloadedSubtitleStore } from "./downloaded-store.ts";
import { installFetchSubtitleHook } from "./fetch-hook.ts";
import { PageSubtitles } from "./page-subtitles.ts";
import { capturePromiseObserver, type PromiseWindow } from "./promise-observer.ts";
import { captureResponseReaders, type ResponseReadersWindow } from "./response-readers.ts";
import { SubtitleKeyAllocator } from "./subtitle-keys.ts";
import { captureTrackReaders, type TrackReadersWindow } from "./track-readers.ts";
import { captureXhrReaders, type XhrReadersWindow } from "./xhr-readers.ts";
import { installXhrSubtitleHook } from "./xhr-hook.ts";

/** Окно, против которого работают хуки субтитров. Всё необязательное — чего нет, то не наблюдается. */
export interface PageSubtitleWindow extends PromiseWindow, ResponseReadersWindow, XhrReadersWindow, ByteToolsWindow, TrackReadersWindow {
  readonly fetch?: unknown;
  /** Часы окна; нет — осмотр дорожек по тикам не троттлится по времени (работает раз за передачу). */
  readonly performance?: { now(): number };
}

export interface PageSubtitleInstallTools {
  readonly binary: BinaryIntrinsics;
  readonly channel: ChannelOriginals;
  readonly meter: ForwardByteMeter;
  readonly reportOddity: OddityReporter;
}

/** Результат установки: шов для контроллера и что именно встало (для тестов). */
export interface InstalledPageSubtitles {
  readonly seam: PageSubtitles;
  readonly store: DownloadedSubtitleStore;
  readonly fetchHooked: boolean;
  readonly xhrHooked: boolean;
}

/** Часы окна, снятые сейчас: `performance.now` связывается здесь, чтобы страница не подменила его позже. */
function captureClock(win: PageSubtitleWindow): () => number {
  const performanceObject = win.performance;
  if (performanceObject === undefined) return () => 0;
  const now = performanceObject.now.bind(performanceObject);
  return () => now();
}

/** Выполнить установку части так, чтобы сбой не вышел наружу. */
function installSafely(reportOddity: OddityReporter, what: string, install: () => boolean): boolean {
  try {
    return install();
  } catch {
    reportOddity(`could not install the ${what} subtitle hook`);
    return false;
  }
}

export function installPageSubtitles(win: PageSubtitleWindow, tools: PageSubtitleInstallTools): InstalledPageSubtitles {
  const { binary, channel, meter, reportOddity } = tools;
  const keys = new SubtitleKeyAllocator();
  const store = new DownloadedSubtitleStore(keys);
  const bytes = captureByteTools(win, binary);
  const seam = new PageSubtitles({ store, keys, binary, meter, bytes, channel, trackReaders: captureTrackReaders(win), now: captureClock(win), reportOddity });
  const intake = new SubtitleDownloadIntake(store, (document) => seam.noteDownloaded(document));

  const fetchHooked = installSafely(reportOddity, "fetch", () => {
    const readers = captureResponseReaders(win);
    if (readers === null || typeof win.fetch !== "function") return false;
    return installFetchSubtitleHook(win, { readers, observePromise: capturePromiseObserver(win), binary, bytes, intake, reportOddity });
  });
  const xhrHooked = installSafely(reportOddity, "XHR", () => {
    const readers = captureXhrReaders(win);
    return readers !== null && installXhrSubtitleHook({ readers, channel, bytes, intake, reportOddity });
  });
  return { seam, store, fetchHooked, xhrHooked };
}

export type { PageSubtitleHandoff, PageSubtitleHandoffContext, PageSubtitleSeam } from "./subtitle-seam.ts";
