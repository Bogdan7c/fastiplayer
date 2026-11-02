// Контроллер передачи в MAIN-мире: отвечает на команды участка 1 и ведёт живой поток MSE.
//
// `begin_handoff` по шагам (порядок — договор с фоном и плеером):
//  1. найти элемент по ключу и его живой MediaSource → `handoff_began{outcome}`;
//  2. повтор «памяти без копий» этого MediaSource (снимок + хвост журнала);
//  3. `page_seek{T0}` — настоящая позиция элемента (решение 111: перемотка во время открытия
//     поколения = его старт);
//  4. `page_playback{play}`, если элемент играл;
//  5. `startShadow(...)` — элемент уходит в тень;
//  6. дальше — живой поток событий и байтов MediaSource этого элемента (и новых MediaSource, которые
//     страница подключит к нему же: «src элемента = blob-адрес MediaSource при его первом
//     addSourceBuffer»).
// `end_handoff` → тень снимается, живой поток останавливается. Событие `handoff_ended` от тени
// (страница сама убрала источник) тоже останавливает поток.
//
// Пока передачи нет, в порт не уходит НИЧЕГО, кроме ответов на запросы.
// Байты: журнал держит виды без копий; копия делается только в момент отправки — плотная (ровно
// кусок, а не весь буфер страницы) и передаётся порту через transfer (одна копия на кусок).
// Кусок больше предела события (32 МиБ, договор с изолированным миром) режется на части.

import type { DocumentTag, ElementKey, HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../shared/handoff-contract.ts";
import { MAX_PAGE_BYTES_EVENT_BYTES } from "../shared/page-message-validation.ts";
import { copyForTransfer, type BinaryIntrinsics } from "./binary-view.ts";
import type { DomReaders } from "./dom-readers.ts";
import type { ElementRegistry } from "./element-registry.ts";
import type { ShadowSessionHandle, ShadowStarter } from "./handoff-seam.ts";
import type { ReplayItem } from "./journal/media-journal.ts";
import type { MediaSourceRecord, MseListener } from "./mse-tracker.ts";
import { positionMicros } from "./mse-values.ts";
import type { IncomingPageMessage } from "./page-command-guard.ts";
import type { PagePort } from "./page-channel.ts";
import type { OddityReporter } from "./prototype-wrap.ts";
import type { VideoCandidates } from "./video-candidates.ts";

/** Что нужно контроллеру снаружи. */
export interface HandoffControllerEnvironment {
  readonly registry: ElementRegistry;
  readonly candidates: VideoCandidates;
  readonly dom: DomReaders;
  readonly binary: BinaryIntrinsics;
  readonly startShadow: ShadowStarter;
  readonly reportOddity: OddityReporter;
}

/** Идущая передача одного элемента. */
interface ActiveHandoff {
  readonly element: Element;
  /** Ключи MediaSource, чьи события и байты идут живым потоком. */
  readonly liveMediaSources: Set<string>;
  session: ShadowSessionHandle | null;
}

export class HandoffController implements MseListener {
  readonly #environment: HandoffControllerEnvironment;
  #port: PagePort | null = null;
  #active: ActiveHandoff | null = null;

  constructor(environment: HandoffControllerEnvironment) {
    this.#environment = environment;
  }

  /** Порт участка 1 установлен. */
  connect(port: PagePort): void {
    this.#port = port;
  }

  /** Есть ли идущая передача (для тестов и диагностики). */
  get isHandingOff(): boolean {
    return this.#active !== null;
  }

  /** Сообщение изолированного мира (форма уже проверена). */
  handle(message: IncomingPageMessage): void {
    switch (message.kind) {
      case "describe_candidates":
        this.#port?.send({ kind: "candidates", request_id: message.request_id, candidates: this.#environment.candidates.describe() });
        return;
      case "begin_handoff":
        this.#beginHandoff(message.request_id, message.document, message.element);
        return;
      case "shadow":
        this.#applyShadowCommand(message.command);
        return;
      case "end_handoff":
        this.#endHandoff(message.position, message.resume);
        return;
    }
  }

  onMediaSourceStarted(record: MediaSourceRecord): void {
    const active = this.#active;
    if (active === null || record.objectUrl === null) return;
    // Страница подключила к переданному элементу новый MediaSource (смена качества, реклама,
    // перезагрузка плеера): он тоже идёт живым потоком, со своим новым локальным ключом.
    if (this.#environment.dom.mediaSourceUrlsOf(active.element).includes(record.objectUrl)) {
      active.liveMediaSources.add(record.key);
    }
  }

  onItem(record: MediaSourceRecord, item: ReplayItem): void {
    const active = this.#active;
    if (active === null || !active.liveMediaSources.has(record.key)) return;
    try {
      this.#sendItem(record.key, item);
    } catch {
      // Кусок не ушёл (копия не выделилась, порт отказал): дальше в потоке была бы дыра, и плеер
      // показал бы мусор. Честнее кончить передачу: тень снимается, страница играет сама.
      this.#environment.reportOddity("live stream send failed");
      this.#endHandoff({ kind: "shadow_position" }, { kind: "as_shadow" });
    }
  }

  #beginHandoff(requestId: number, documentTag: DocumentTag, elementKey: ElementKey): void {
    const port = this.#port;
    if (port === null) return;
    // Новая передача, пока идёт старая (фон так не делает): старую честно закрываем.
    if (this.#active !== null) this.#endHandoff({ kind: "shadow_position" }, { kind: "as_shadow" });

    const { registry, candidates, dom } = this.#environment;
    // Чужая метка — элемент выбран в другом документе этого фрейма (ключи там совпадают случайно).
    const element = documentTag === candidates.documentTag ? registry.elementByKey(elementKey) : null;
    if (element === null || !dom.isVideoElement(element)) {
      port.send({ kind: "handoff_began", request_id: requestId, outcome: "element_not_found" });
      return;
    }
    const record = candidates.liveMediaSourceOf(element);
    if (record === null) {
      port.send({ kind: "handoff_began", request_id: requestId, outcome: "no_live_media" });
      return;
    }
    // Состояние настоящего элемента читаем ДО тени: тень покажет странице уже время плеера.
    const startPosition = positionMicros(dom.currentTimeSeconds(element)) ?? 0;
    const wasPlaying = !dom.isPaused(element) && !dom.isEnded(element);

    port.send({ kind: "handoff_began", request_id: requestId, outcome: "started" });
    const active: ActiveHandoff = { element, liveMediaSources: new Set([record.key]), session: null };
    this.#active = active;
    try {
      for (const item of record.journal.replay()) this.#sendItem(record.key, item);
      port.send({ kind: "page_seek", target_us: startPosition });
      if (wasPlaying) port.send({ kind: "page_playback", request: { kind: "play" } });
      active.session = this.#environment.startShadow(
        element as HTMLMediaElement,
        (event) => this.#emitFromShadow(active, event),
        (url) => registry.isMediaSourceObjectUrl(url),
      );
    } catch {
      // Повтор или тень не удались: передача невозможна — честно сообщаем об окончании
      // (фон закроет сеанс), а страница остаётся со своим настоящим элементом.
      this.#environment.reportOddity("handoff failed to start");
      this.#finishActive(active);
    }
  }

  #applyShadowCommand(command: ShadowCommand): void {
    const session = this.#active?.session;
    if (session === undefined || session === null) return;
    try {
      session.apply(command);
    } catch {
      this.#environment.reportOddity("shadow command failed");
    }
  }

  #endHandoff(position: HandoffEndPosition, resume: HandoffEndResume): void {
    const active = this.#active;
    if (active === null) return;
    try {
      active.session?.end(position, resume);
    } catch {
      this.#environment.reportOddity("shadow failed to end");
    }
    // Тень обязана прислать handoff_ended сама; если не прислала — закрываем за неё, чтобы фон
    // не ждал вечно. Её запоздавшее событие потом будет отброшено.
    if (this.#active === active) this.#finishActive(active);
  }

  /** События тени: пропускаем только пока передача та же; `handoff_ended` её закрывает. */
  #emitFromShadow(active: ActiveHandoff, event: PageEvent): void {
    if (this.#active !== active) return;
    if (event.kind === "handoff_ended") {
      this.#active = null;
    }
    this.#port?.send(event);
  }

  /** Закрыть передачу от своего имени: `handoff_ended{returned}`, живой поток остановлен. */
  #finishActive(active: ActiveHandoff): void {
    if (this.#active !== active) return;
    this.#active = null;
    try {
      this.#port?.send({ kind: "handoff_ended", cause: "returned" });
    } catch {
      // Порт сломан — сообщить некому; фон узнает об обрыве по разрыву связи с изолированным миром.
      this.#environment.reportOddity("could not report the end of a handoff");
    }
  }

  #sendItem(mediaSourceKey: string, item: ReplayItem): void {
    const port = this.#port;
    if (port === null) return;
    if (item.kind === "mse") {
      port.send({ kind: "mse", event: item.event });
      return;
    }
    // Изолированный мир принимает событие `bytes` не больше MAX_PAGE_BYTES_EVENT_BYTES: больший
    // кусок уходит несколькими событиями подряд, каждое — точная копия своей части с transfer.
    for (let offset = 0; offset < item.view.length; offset += MAX_PAGE_BYTES_EVENT_BYTES) {
      const part = item.view.subarray(offset, offset + MAX_PAGE_BYTES_EVENT_BYTES);
      const bytes = copyForTransfer(this.#environment.binary, part);
      port.send({ kind: "bytes", media_source: mediaSourceKey, source_buffer: item.sourceBuffer, bytes }, [bytes.buffer as ArrayBuffer]);
    }
  }
}
