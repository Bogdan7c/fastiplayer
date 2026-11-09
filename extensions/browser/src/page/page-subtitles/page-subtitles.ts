// Субтитры страницы в MAIN-мире: склад скачанных документов + отдача при передаче (сессия 13b2).
//
// Владеет: складом скачанных документов (`DownloadedSubtitleStore`), выдачей ключей и сессией отдачи
// текущей передачи. Не знает ничего о MSE, тени и портах: отправку ему даёт контроллер через
// `PageSubtitleHandoffContext.send`.
//
// Когда что уходит (решения 270–272, 275):
//  - пока передачи нет — ничего, документы копятся на складе;
//  - `begin()` — сразу отдаёт весь склад (самый свежий — `showing`, остальные — `not_showing`) и
//    запускает осмотр `<track>`/`TextTrack` видео;
//  - документ, пойманный во время передачи, уходит сразу и становится `showing` (плеер сайта скачал его
//    последним — зритель включил его);
//  - `end()` — слушатели сняты, дальше ничего не уходит.

import type { StoredSubtitleDocument, DownloadedSubtitleStore } from "./downloaded-store.ts";
import type { ByteTools } from "./byte-tools.ts";
import type { ChannelOriginals } from "../page-channel.ts";
import type { BinaryIntrinsics } from "../binary-view.ts";
import type { ForwardByteMeter } from "../forward-byte-meter.ts";
import type { OddityReporter } from "../prototype-wrap.ts";
import type { PageSubtitleHandoff, PageSubtitleHandoffContext, PageSubtitleSeam } from "./subtitle-seam.ts";
import { createSubtitleSender, type SubtitleSender } from "./subtitle-sender.ts";
import type { SubtitleKeyAllocator } from "./subtitle-keys.ts";
import { TrackSubtitleWatch } from "./track-watch.ts";
import type { TrackReaders } from "./track-readers.ts";

export interface PageSubtitlesTools {
  readonly store: DownloadedSubtitleStore;
  readonly keys: SubtitleKeyAllocator;
  readonly binary: BinaryIntrinsics;
  readonly meter: ForwardByteMeter;
  readonly bytes: ByteTools;
  readonly channel: Pick<ChannelOriginals, "addListener" | "removeListener">;
  /** Читатели `<track>`/`TextTrack`; `null` — окружение их не даёт, дорожки видео не осматриваются. */
  readonly trackReaders: TrackReaders | null;
  /** Монотонные часы окна, мс. */
  readonly now: () => number;
  readonly reportOddity: OddityReporter;
}

/** Отдача субтитров одной передачи. */
class SubtitleHandoffSession implements PageSubtitleHandoff {
  readonly #sender: SubtitleSender;
  readonly #store: DownloadedSubtitleStore;
  readonly #watch: TrackSubtitleWatch | null;
  #ended = false;

  constructor(sender: SubtitleSender, store: DownloadedSubtitleStore, watch: TrackSubtitleWatch | null) {
    this.#sender = sender;
    this.#store = store;
    this.#watch = watch;
  }

  /** Отдать склад целиком (от старого к свежему) и начать осмотр дорожек видео. */
  open(): void {
    for (const document of this.#store.snapshot()) this.sendDownloaded(document);
    this.#watch?.start();
  }

  /** Отправить скачанный документ как есть: `display` — по месту на складе. */
  sendDownloaded(document: StoredSubtitleDocument): void {
    if (this.#ended) return;
    this.#sender.sendDocument({
      key: document.key,
      naming: document.naming,
      format_hint: document.formatHint,
      origin: "page_download",
      display: this.#store.displayOf(document.key),
      bytes: document.bytes,
    });
  }

  clockTick(): void {
    if (!this.#ended) this.#watch?.tick();
  }

  end(): void {
    if (this.#ended) return;
    this.#ended = true;
    this.#watch?.stop();
  }
}

export class PageSubtitles implements PageSubtitleSeam {
  readonly #tools: PageSubtitlesTools;
  #session: SubtitleHandoffSession | null = null;

  constructor(tools: PageSubtitlesTools) {
    this.#tools = tools;
  }

  begin(context: PageSubtitleHandoffContext): PageSubtitleHandoff {
    this.#session?.end();
    const { trackReaders } = this.#tools;
    const sender = createSubtitleSender(this.#tools, (event, transfer) => context.send(event, transfer));
    const watch =
      trackReaders === null
        ? null
        : new TrackSubtitleWatch(
            { readers: trackReaders, keys: this.#tools.keys, bytes: this.#tools.bytes, channel: this.#tools.channel, now: this.#tools.now, reportOddity: this.#tools.reportOddity },
            context.element,
            sender,
            context.referrerFacts,
          );
    const session = new SubtitleHandoffSession(sender, this.#tools.store, watch);
    this.#session = session;
    session.open();
    return {
      clockTick: () => session.clockTick(),
      end: () => {
        session.end();
        if (this.#session === session) this.#session = null;
      },
    };
  }

  /** Хук поймал новый скачанный документ: идёт передача — отправить сразу, нет — он уже на складе. */
  noteDownloaded(document: StoredSubtitleDocument): void {
    this.#session?.sendDownloaded(document);
  }
}
