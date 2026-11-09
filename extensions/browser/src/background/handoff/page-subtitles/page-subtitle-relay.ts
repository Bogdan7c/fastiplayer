// Передатчик субтитров страницы плееру (сессия 13b2, протокол 1.5, решения 270-272, 275, 276).
//
// Владелец «субтитровой» половины сеанса. Сеанс (`handoff-session.ts`) только отдаёт ему события
// страницы и сообщает о смене документа; всё остальное здесь:
// - готовый документ (`subtitle_document`) уходит сразу, `<track src>` (`subtitle_track_source`) - после
//   закачки фоном (`track-downloader.ts`);
// - номера дорожек и бюджет ведёт `track-registry.ts`, имя приводит к протоколу `track-naming.ts`;
// - плееру договорённой версии ниже 1.5 не уходит НИЧЕГО (ни сообщение, ни рамка), сеанс живёт;
// - события вне потока (`streamIsOpen` ложно: сеанс кончился, страница ушла «сиротой», тени ещё нет)
//   отбрасываются; закачки документа, который ушёл, отменяются, их результат не отправляется;
// - после допуска документ уходит в очередь сеанса одним куском подряд: заголовок и все рамки вида 3.
// Адрес `<track src>` остаётся в фоне и плееру не уходит; в журнал адрес тоже не пишем.

import { dataFrameIntroducedIn, pageSessionIntroducedIn, splitPageSubtitleDocument, versionIncludes, type ProtocolVersion } from "../../../protocol/index.ts";
import { MAX_PAGE_SUBTITLE_DOCUMENT_BYTES } from "../../../protocol/limits.ts";
import type { PageSubtitleDisplay, PageSubtitleOrigin, SubtitleFormatHint } from "../../../protocol/session/page-subtitles.ts";
import type { PageSubtitleEvent, PageSubtitleKey, PageSubtitleNaming } from "../../../shared/page-subtitle-contract.ts";
import type { FileReferrer } from "../../../shared/referrer-policy.ts";
import type { Scheduler } from "../../host-link/native-port.ts";
import type { FileAddressRule } from "../file-address-guard.ts";
import { admitsResourceUrl } from "../hls/resource-admission.ts";
import type { OutgoingItem } from "../page-event-translator.ts";
import { referrerOf } from "../pulled-source-start.ts";
import type { RangeFetch } from "../range-fetcher.ts";
import type { OfferedPageTrack } from "../../choice-memory/page-track-catalog.ts";
import { protocolNamingOf } from "./track-naming.ts";
import { createTrackDownloader, type TrackDownloadOutcome, type TrackDownloader } from "./track-downloader.ts";
import { createTrackRegistry, type SubtitleDocumentIdentity, type TrackRegistry } from "./track-registry.ts";

/** Куда уходят сообщения и рамки: очередь сеанса. */
export interface PageSubtitleSink {
  enqueue(item: OutgoingItem): void;
}

/** Что передатчику даёт сеанс. Всё, что меняется по ходу сеанса, - функциями. */
export interface PageSubtitleRelayParameters {
  /** Очередь сеанса; `undefined` - сокета ещё нет. */
  readonly sink: () => PageSubtitleSink | undefined;
  /** Версия протокола, о которой договорились с плеером. */
  readonly agreedProtocol: () => ProtocolVersion;
  /** Идёт ли сейчас поток страницы в плеер (`acceptsStream` сеанса). */
  readonly streamIsOpen: () => boolean;
  /** Сеть для закачки `<track src>` с таким Referer (заслон локальных адресов внутри). */
  readonly networkFor: (referrer: FileReferrer | null) => RangeFetch;
  /** Правило адреса сеанса на ТЕКУЩЕМ месте тени. */
  readonly fileAddressRule: () => FileAddressRule;
  readonly scheduler: Scheduler;
  /** Отладочный журнал фона. */
  readonly note: (message: string) => void;
  /** Дорожка ушла плееру (и при замене тем же номером): память выбора узнаёт её язык и подпись (14b2). */
  readonly onTrackOffered?: (track: OfferedPageTrack) => void;
}

/** Документ, готовый к отправке плееру. */
export interface SubtitleDocumentToSend {
  readonly key: PageSubtitleKey;
  readonly naming: PageSubtitleNaming;
  readonly formatHint: SubtitleFormatHint;
  readonly origin: PageSubtitleOrigin;
  readonly display: PageSubtitleDisplay;
  readonly bytes: Uint8Array;
}

/** Исход приёма документа: журналу и тестам важно различать причины. */
export type SubtitleSendOutcome =
  | { readonly kind: "sent"; readonly track: number; readonly replaced: boolean }
  /** Плеер договорился на версию ниже 1.5: ничего не отправлено. */
  | { readonly kind: "player_too_old" }
  /** Поток страницы сейчас не идёт в плеер (сеанс кончился, страница ушла, тени нет). */
  | { readonly kind: "stream_closed" }
  | { readonly kind: "too_many_tracks" }
  | { readonly kind: "over_budget" }
  /** Документ пуст или длиннее 8 МиБ: протокол его не принимает. */
  | { readonly kind: "invalid_length" };

export interface PageSubtitleRelay {
  /**
   * Сеанс открыл подключение к документу `document` (порт к фрейму): прежние закачки отменены. Ключи
   * того же документа (второе видео той же страницы) попадают в прежние номера, другого - в новые.
   */
  beginDocument(document: SubtitleDocumentIdentity): void;
  /** Документ страницы ушёл (или сеанс кончился): закачки отменены, их результат не отправляется. */
  endDocument(): void;
  /** Событие субтитров от страницы. */
  accept(event: PageSubtitleEvent): void;
  /** Отправить готовый документ плееру (для событий `subtitle_document` и закачанных `<track>`). */
  sendDocument(document: SubtitleDocumentToSend): SubtitleSendOutcome;
}

/** Плеер версии `agreed` понимает субтитры страницы (и сообщение, и рамки вида 3). */
export function playerAcceptsPageSubtitles(agreed: ProtocolVersion): boolean {
  return versionIncludes(agreed, pageSessionIntroducedIn("subtitle_track_offered")) && versionIncludes(agreed, dataFrameIntroducedIn("page_subtitle_bytes"));
}

export function createPageSubtitleRelay(
  parameters: PageSubtitleRelayParameters,
  registry: TrackRegistry = createTrackRegistry(),
  downloader: TrackDownloader = createTrackDownloader(parameters.scheduler),
): PageSubtitleRelay {
  // Документ текущего подключения; до первого подключения событий страницы быть не может.
  let currentDocument: SubtitleDocumentIdentity | undefined;
  // Отмена закачек текущего документа; новый документ получает новый контроллер.
  let downloads = new AbortController();

  function sendDocument(document: SubtitleDocumentToSend): SubtitleSendOutcome {
    const sink = parameters.sink();
    if (!parameters.streamIsOpen() || sink === undefined || currentDocument === undefined) return { kind: "stream_closed" };
    if (!playerAcceptsPageSubtitles(parameters.agreedProtocol())) return { kind: "player_too_old" };
    const byteLength = document.bytes.length;
    if (byteLength === 0 || byteLength > MAX_PAGE_SUBTITLE_DOCUMENT_BYTES) return { kind: "invalid_length" };
    const admission = registry.admit({ document: currentDocument, key: document.key, byteLength });
    if (admission.kind !== "admitted") return admission;
    const { language, label } = protocolNamingOf(document.naming);
    const sending = splitPageSubtitleDocument(
      { track: admission.track, language, label, format_hint: document.formatHint, origin: document.origin, page_display: document.display },
      document.bytes,
    );
    // Длина и номер проверены выше: отказ кодировщика - наша ошибка, глушить её нельзя.
    if (!sending.ok) throw new Error(`page subtitle document was rejected by the encoder: ${sending.error.kind}`);
    // Заголовок и рамки - одним подряд идущим куском очереди: байты документа ни с чем не перемешаны.
    sink.enqueue({ kind: "message", message: sending.value.offer });
    for (const piece of sending.value.pieces) sink.enqueue({ kind: "frame", header: piece.header, payload: piece.payload });
    parameters.onTrackOffered?.({
      track: admission.track,
      language: language.kind === "known" ? language.tag : null,
      label: label.kind === "text" ? label.text : null,
    });
    return { kind: "sent", track: admission.track, replaced: admission.replaced };
  }

  /** Записать в журнал итог приёма (без адресов и содержимого). */
  function noteSendOutcome(source: string, key: PageSubtitleKey, outcome: SubtitleSendOutcome): void {
    if (outcome.kind === "sent") parameters.note(`page subtitles: ${source} key ${key} sent as track ${outcome.track}${outcome.replaced ? " (replacement)" : ""}`);
    else parameters.note(`page subtitles: ${source} key ${key} not sent: ${outcome.kind}`);
  }

  /** `<track src>`: закачка фоном, затем отправка, если документ ещё жив. */
  async function relayTrackSource(event: Extract<PageSubtitleEvent, { kind: "subtitle_track_source" }>): Promise<void> {
    const documentDownloads = downloads;
    // Версию проверяем до закачки: плееру 1.4 субтитры не нужны, ходить в сеть незачем.
    if (!playerAcceptsPageSubtitles(parameters.agreedProtocol())) {
      parameters.note(`page subtitles: track key ${event.key} not downloaded: player_too_old`);
      return;
    }
    const rule = parameters.fileAddressRule();
    const outcome = await downloader.download({
      url: event.url,
      network: parameters.networkFor(referrerOf(event)),
      allowsAddress: (url) => admitsResourceUrl(url, rule),
      signal: documentDownloads.signal,
    });
    if (outcome.kind !== "ready") {
      parameters.note(`page subtitles: track key ${event.key} download failed: ${describeDownloadFailure(outcome)}`);
      return;
    }
    if (documentDownloads.signal.aborted) return;
    noteSendOutcome(
      "track",
      event.key,
      sendDocument({ key: event.key, naming: event.naming, formatHint: outcome.formatHint, origin: "track_element", display: event.display, bytes: outcome.bytes }),
    );
  }

  return {
    beginDocument(document) {
      downloads.abort();
      downloads = new AbortController();
      currentDocument = document;
    },
    endDocument() {
      downloads.abort();
    },
    accept(event) {
      if (!parameters.streamIsOpen()) {
        parameters.note(`page subtitles: ${event.kind} key ${event.key} dropped: stream_closed`);
        return;
      }
      if (event.kind === "subtitle_track_source") {
        relayTrackSource(event).catch((error: unknown) => parameters.note(`page subtitles: track key ${event.key} failed: ${String(error)}`));
        return;
      }
      noteSendOutcome(
        "document",
        event.key,
        sendDocument({ key: event.key, naming: event.naming, formatHint: event.format_hint, origin: event.origin, display: event.display, bytes: event.bytes }),
      );
    },
    sendDocument,
  };
}

/** Слово для журнала: причина неудачной закачки. */
function describeDownloadFailure(outcome: Exclude<TrackDownloadOutcome, { readonly kind: "ready" }>): string {
  return outcome.kind === "http_error" ? `http_error ${outcome.status}` : outcome.kind;
}
