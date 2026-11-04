// Контроллер передачи в MAIN-мире: отвечает на команды участка 1 и ведёт живой поток MSE.
//
// `begin_handoff` по шагам (порядок — договор с фоном и плеером):
//  1. найти элемент по ключу и форму его источника (`element-source.ts`) → `handoff_began{outcome}`;
//  2. открыть поток источника:
//     - MSE — повтор «памяти без копий» этого MediaSource (снимок + хвост журнала);
//     - MSE выделенного воркера (09b-C) — тот же повтор, но его шлёт хук воркера по личному каналу:
//       шаги 3–5 ждут его конца (`replayed`), срок — у реестра воркеров; не дождались — честный конец;
//     - файл `<video src>` (09b) — `file_source{url}`: байты по запросам плеера качает фон;
//     - у обоих видов MSE `page_seek{T0}` идёт сразу за первым событием повтора: фон ставит перед
//       этим событием `generation_started`, так что T0 — первое, что плеер знает о поколении.
//       Журнал хранит историю страницы целиком, вместе со стёртым: дождись плеер конца повтора,
//       он успел бы поставить поколение на начало истории и попросить страницу туда перемотаться,
//       хотя страница стоит совсем в другом месте (живая проверка 09b2, запись Twitch);
//  3. `page_seek{T0}` ещё раз (решение 111: перемотка во время открытия поколения = его старт).
//     Повтор главного потока синхронный, и это тот же T0; повтор воркера долгий, и элемент мог
//     уйти вперёд. Поколение эфира плеер на T0 при загрузке не ставит и идёт за этой перемоткой
//     после показа;
//  4. `page_playback{play}`, если элемент играл;
//  5. `startShadow(...)` — элемент уходит в тень (у файла — с снятым источником, решение 186);
//  6. дальше — живой поток событий и байтов MediaSource этого элемента (и новых MediaSource, которые
//     страница подключит к нему же: «src элемента = blob-адрес MediaSource при его первом
//     addSourceBuffer»; у воркера — «srcObject элемента = handle этого MediaSource», когда у него
//     появился первый SourceBuffer).
// `end_handoff` → тень снимается, живой поток останавливается. Событие `handoff_ended` от тени
// (страница честно забрала видео: чужой источник, пусто дольше допуска, элемент убран) тоже
// останавливает поток. Снятый на мгновение источник (YouTube: `srcObject = null` перед новым
// MediaSource) передачу НЕ кончает — тень остаётся, и новый MediaSource идёт живым потоком (Д-1).
// Пока тень жива, через контроллер идёт и её пульс `shadow_alive` (решение 180).
//
// Пока передачи нет, в порт не уходит НИЧЕГО, кроме ответов на запросы, а воркеры молчат: их
// потоки закрываются вместе с передачей.
// Байты: журнал держит виды без копий; копия делается только в момент отправки (`stream-sender.ts`).
// Куски воркера уже приходят плотной копией с transfer и уходят дальше тем же буфером.

import type { MediaTimeMicros } from "../protocol/time.ts";
import type { DocumentTag, ElementKey, HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../shared/handoff-contract.ts";
import type { BinaryIntrinsics } from "./binary-view.ts";
import type { DomReaders } from "./dom-readers.ts";
import type { ElementRegistry } from "./element-registry.ts";
import type { ElementSource } from "./element-source.ts";
import type { ShadowSessionHandle, ShadowSourceKind, ShadowStarter } from "./handoff-seam.ts";
import { notifyTap, type HandoffTap, type StreamOrigin } from "./handoff-tap.ts";
import type { ReplayItem } from "./journal/media-journal.ts";
import type { MediaSourceRecord, MseListener } from "./mse-tracker.ts";
import { positionMicros } from "./mse-values.ts";
import type { IncomingPageMessage } from "./page-command-guard.ts";
import type { PagePort } from "./page-channel.ts";
import type { OddityReporter } from "./prototype-wrap.ts";
import { sendStreamItem, type StreamMessage } from "./stream-sender.ts";
import type { VideoCandidates } from "./video-candidates.ts";
import type { WorkerMediaSourceListener, WorkerMediaSourceRef, WorkerMediaSources } from "./worker-capture/index.ts";

/** Что нужно контроллеру снаружи. */
export interface HandoffControllerEnvironment {
  readonly registry: ElementRegistry;
  readonly candidates: VideoCandidates;
  readonly dom: DomReaders;
  readonly binary: BinaryIntrinsics;
  readonly startShadow: ShadowStarter;
  readonly workerMediaSources: WorkerMediaSources;
  readonly reportOddity: OddityReporter;
  /** Подслушивание шагов передачи (только отладочная сборка записи потока); без него — ничего. */
  readonly tap?: HandoffTap;
}

/** Идущая передача одного элемента. */
interface ActiveHandoff {
  readonly element: Element;
  /** Ключи MediaSource (главного потока и «w<N>/ms-K» воркеров), чьи события и байты идут живым потоком. */
  readonly liveMediaSources: Set<string>;
  session: ShadowSessionHandle | null;
  /** T0, уже названный плееру в начале повтора (шаг 2); `null` — ещё не назван. */
  announcedStartUs: MediaTimeMicros | null;
}

/** Зачем открывается поток MediaSource воркера. */
type WorkerStreamRole =
  /** Источник самой передачи: тень встаёт после конца повтора. */
  | "handoff_source"
  /** Новый MediaSource на уже переданном элементе (новое поколение): тень уже стоит. */
  | "new_generation";

export class HandoffController implements MseListener, WorkerMediaSourceListener {
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
    if (message.kind !== "describe_candidates") this.#tap((tap) => tap.commandReceived(message));
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
      this.#sendItem(record.key, item, "live");
    } catch {
      this.#failLiveStream();
    }
  }

  onWorkerMediaSourceStarted(mediaSource: WorkerMediaSourceRef): void {
    const active = this.#active;
    if (active === null || active.liveMediaSources.has(mediaSource.key)) return;
    // Страница поставила переданному элементу handle нового MediaSource воркера (09a2: сначала
    // `srcObject = null`, потом новый handle) — он идёт новым поколением, как в главном потоке.
    const source = this.#sourceOf(active.element);
    if (source.kind === "worker_media_source" && source.mediaSource.key === mediaSource.key) {
      this.#openWorkerStream(active, mediaSource, "new_generation");
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
    const source = this.#sourceOf(element);
    if (source.kind === "unsupported" || source.kind === "none") {
      // Источник сняли или сменили на неподдержанную форму между опросом фона и командой.
      port.send({ kind: "handoff_began", request_id: requestId, outcome: "no_media" });
      return;
    }

    port.send({ kind: "handoff_began", request_id: requestId, outcome: "started" });
    this.#tap((tap) => tap.handoffBegan(element));
    const liveMediaSources = new Set<string>(source.kind === "media_source" ? [source.record.key] : []);
    const active: ActiveHandoff = { element, liveMediaSources, session: null, announcedStartUs: null };
    this.#active = active;
    if (source.kind === "worker_media_source") {
      // Повтор журнала идёт из воркера асинхронно: шаги 3–5 — по его концу.
      this.#openWorkerStream(active, source.mediaSource, "handoff_source");
      return;
    }
    try {
      const shadowSource = this.#openStream(active, port, source);
      // Повтор главного потока синхронный: элемент стоит там же, T0 — ровно названный.
      this.#startShadow(active, shadowSource, active.announcedStartUs ?? this.#startPositionOf(element));
    } catch {
      this.#failToStart(active);
    }
  }

  /**
   * Шаги 3–5: `page_seek{T0}`, `page_playback{play}` (если играл), тень. Состояние настоящего
   * элемента читается здесь, ДО тени: тень покажет странице уже время плеера. Бросает — решает
   * вызывающий (`#failToStart`).
   */
  #startShadow(active: ActiveHandoff, shadowSource: ShadowSourceKind, startPositionUs: MediaTimeMicros): void {
    const port = this.#port;
    if (port === null) throw new Error("page port is gone");
    const { dom, registry } = this.#environment;
    const { element } = active;
    const wasPlaying = !dom.isPaused(element) && !dom.isEnded(element);
    // Дальше живой поток: его события T0 уже не называют, даже если повтор был пустым.
    active.announcedStartUs = startPositionUs;
    port.send({ kind: "page_seek", target_us: startPositionUs });
    if (wasPlaying) port.send({ kind: "page_playback", request: { kind: "play" } });
    active.session = this.#environment.startShadow(element as HTMLMediaElement, (event) => this.#emitFromShadow(active, event), {
      isMediaSourceObjectUrl: (url) => registry.isMediaSourceObjectUrl(url),
      source: shadowSource,
    });
  }

  /** Настоящая позиция элемента (T0), мкс. Бросает — решает вызывающий. */
  #startPositionOf(element: Element): MediaTimeMicros {
    return positionMicros(this.#environment.dom.currentTimeSeconds(element)) ?? 0;
  }

  /**
   * Шаг 2: как только первое событие повтора объявило поколение, назвать плееру T0, ещё до байтов
   * (почему — в описании шагов наверху). Байты журнала событием не считаются: без объявленного
   * SourceBuffer фон их отвергнет, а `page_seek` без поколения плееру сказать не о чем.
   */
  #announceStartOnGeneration(active: ActiveHandoff, streamed: ReplayItem | StreamMessage): void {
    if (active.announcedStartUs !== null || streamed.kind !== "mse") return;
    active.announcedStartUs = this.#startPositionOf(active.element);
    this.#port?.send({ kind: "page_seek", target_us: active.announcedStartUs });
  }

  /**
   * Повтор или тень не удались: передача невозможна — честно сообщаем об окончании (фон закроет
   * сеанс), а страница остаётся со своим настоящим элементом.
   */
  #failToStart(active: ActiveHandoff): void {
    this.#environment.reportOddity("handoff failed to start");
    this.#finishActive(active);
  }

  /**
   * Кусок не ушёл (копия не выделилась, порт отказал): дальше в потоке была бы дыра, и плеер
   * показал бы мусор. Честнее кончить передачу: тень снимается, страница играет сама.
   */
  #failLiveStream(): void {
    this.#environment.reportOddity("live stream send failed");
    this.#endHandoff({ kind: "shadow_position" }, { kind: "as_shadow" });
  }

  /**
   * Поток MediaSource воркера: повтор журнала из воркера, затем живые элементы (реестр уже
   * поставил ключ договора «w<N>/ms-K»). Всё, что приходит после конца этой передачи, отброшено.
   */
  #openWorkerStream(active: ActiveHandoff, mediaSource: WorkerMediaSourceRef, role: WorkerStreamRole): void {
    active.liveMediaSources.add(mediaSource.key);
    this.#environment.workerMediaSources.openStream(mediaSource, {
      deliver: (message, transfer) => {
        if (this.#active !== active) return;
        try {
          this.#port?.send(message, transfer);
          if (role === "handoff_source") this.#announceStartOnGeneration(active, message);
        } catch {
          this.#failLiveStream();
        }
      },
      replayed: () => {
        if (this.#active !== active || role !== "handoff_source") return;
        try {
          // Воркер повторяет журнал асинхронно, и элемент мог уйти вперёд: T0 читается заново.
          this.#startShadow(active, "media_source", this.#startPositionOf(active.element));
        } catch {
          this.#failToStart(active);
        }
      },
      unavailable: () => {
        if (this.#active !== active) return;
        // Без повтора нечего показать, без живого потока плеер встанет: передача честно кончается.
        this.#environment.reportOddity("worker media source stream is unavailable");
        this.#endHandoff({ kind: "shadow_position" }, { kind: "as_shadow" });
      },
    });
  }

  /** Форма источника; сломанное чтение (чужой «бренд») — «нечего передавать», со следом. */
  #sourceOf(element: Element): ElementSource {
    try {
      return this.#environment.candidates.sourceOf(element);
    } catch {
      this.#environment.reportOddity("could not read the source of a video element");
      return { kind: "none" };
    }
  }

  /**
   * Открыть поток источника до итогового `page_seek` (договор с фоном): MSE — повтор «памяти без копий»,
   * файл — `file_source{url}` (поколение-файл; адрес дальше фона не уходит). Возвращает, в каком
   * виде ставить тень.
   */
  #openStream(active: ActiveHandoff, port: PagePort, source: Extract<ElementSource, { kind: "media_source" | "file" }>): ShadowSourceKind {
    switch (source.kind) {
      case "media_source":
        for (const item of source.record.journal.replay()) {
          this.#sendItem(source.record.key, item, "replay");
          this.#announceStartOnGeneration(active, item);
        }
        this.#tap((tap) => tap.replayFinished());
        return "media_source";
      case "file":
        port.send({ kind: "file_source", url: source.url });
        return "file";
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

  /**
   * События тени: пропускаем только пока передача та же; `handoff_ended` её закрывает.
   *
   * Отправка могла бросить (копия не выделилась, порт отказал). Тогда событие потеряно, и тень
   * со страницей разошлись бы с фоном и плеером (Д-2). Поэтому:
   * - след — через репортёр странностей (не молча);
   * - любое другое событие — передача честно кончается у нас (тень возвращает элемент странице,
   *   её пульс прекращается); фон узнает либо из `handoff_ended`, если он дойдёт, либо по
   *   тишине пульса дольше `SHADOW_SILENCE_LIMIT_MS` (решение 180);
   * - сам `handoff_ended` уже закрыл передачу: дальше тень молчит, фон узнает по тишине пульса.
   * Исключение в тень не возвращается: её внутренний `#attempt` проглотил бы его без последствий.
   */
  #emitFromShadow(active: ActiveHandoff, event: PageEvent): void {
    if (this.#active !== active) return;
    const endsHandoff = event.kind === "handoff_ended";
    if (event.kind === "handoff_ended") this.#tap((tap) => tap.handoffEnded(event.cause));
    // До отправки: тень после `handoff_ended` не шлёт ничего, и её запоздавшие события не наши.
    if (endsHandoff) this.#release();
    try {
      this.#port?.send(event);
    } catch {
      this.#environment.reportOddity(`shadow event ${event.kind} could not be sent`);
      if (!endsHandoff) this.#endHandoff({ kind: "shadow_position" }, { kind: "as_shadow" });
    }
  }

  /** Закрыть передачу от своего имени: `handoff_ended{returned}`, живой поток остановлен. */
  #finishActive(active: ActiveHandoff): void {
    if (this.#active !== active) return;
    this.#release();
    this.#tap((tap) => tap.handoffEnded("returned"));
    try {
      this.#port?.send({ kind: "handoff_ended", cause: "returned" });
    } catch {
      // Порт сломан — сообщить некому; фон узнает об обрыве по разрыву связи с изолированным миром.
      this.#environment.reportOddity("could not report the end of a handoff");
    }
  }

  /** Передачи больше нет: живой поток главного потока и потоки воркеров остановлены. */
  #release(): void {
    this.#active = null;
    this.#environment.workerMediaSources.closeAllStreams();
  }

  #sendItem(mediaSourceKey: string, item: ReplayItem, origin: StreamOrigin): void {
    const port = this.#port;
    if (port === null) return;
    sendStreamItem(this.#environment.binary, mediaSourceKey, item, (message: StreamMessage, transfer) => {
      // Подписчик видит кусок ДО отправки: `send` передаёт буфер (transfer) и обнуляет его. Если
      // отправка сорвётся, передача кончится (`#failLiveStream`), так что запись «ушло» не врёт надолго.
      if (message.kind === "bytes") {
        const chunk = { mediaSource: message.media_source, sourceBuffer: message.source_buffer, bytes: message.bytes, origin };
        this.#tap((tap) => tap.chunkSent(chunk));
      }
      port.send(message, transfer);
    });
  }

  /** Вызвать подписчика (если он есть), не давая его сбою попасть в передачу. */
  #tap(notify: (tap: HandoffTap) => void): void {
    const tap = this.#environment.tap;
    if (tap !== undefined) notifyTap(this.#environment.reportOddity, () => notify(tap));
  }
}
