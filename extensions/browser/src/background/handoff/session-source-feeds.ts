// Источники, которые фон качает плееру сам (сессия 10a2): файл `<video src>` (запросы `fetch_range`) и
// нативный HLS (запросы `fetch_resource` по номеркам). Модуль владеет ЗАГРУЗЧИКАМИ сеанса и сетевой
// «обвязкой» запросов, а сеанс (handoff-session.ts) решает только КОГДА их запускать и снимать.
//
// Что здесь держится:
// - не больше одного живого источника: новый (или конец поколения) снимает прежний, его запросы обрываются;
// - обвязка сети одинакова для обоих источников: заслон локальных адресов ВНУТРИ (сам запрос), правило
//   Referer СНАРУЖИ (оно ставится на запрос, который заслон уже пропустил);
// - стойка номерков одна на сеанс и переживает поколения (номерки только растут), а её записи
//   забываются с каждым новым поколением HLS: запоздалый номерок прошлого поколения - `refused`;
// - запрос ресурса без живого HLS (поколение MSE или файл) получает `refused` сразу: плеер держит
//   слот каждого запроса, пока не получит ответ, молчать нельзя.
//
// Адреса наружу не выходят: плееру уходят только номерки и рамки.

import type { ResourceHandle } from "../../protocol/index.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { FileReferrer } from "../../shared/referrer-policy.ts";
import { ruleAllowsFileUrl, type FileAddressRule } from "./file-address-guard.ts";
import type { FileReferrerRules } from "./file-referrer-rule.ts";
import { createHandleDesk, type HandleDesk } from "./hls/handle-desk.ts";
import { admitsResourceUrl } from "./hls/resource-admission.ts";
import { createHlsResourceFetcher, type HlsResourceFetcher, type ResourceFetchRequest } from "./hls/resource-fetcher.ts";
import type { LocalAddressBlock } from "./local-address-block-rules.ts";
import { createRangeFetcher, type RangeFeedSink, type RangeFetch, type RangeFetcher, type RangeRequest } from "./range-fetcher.ts";

/** Что фид берёт у сеанса один раз: сеть браузера, правила и часы. */
export interface SessionSourceFeedsParameters {
  /** Сеть браузера (в продукте - `fetch` фона). */
  readonly fetch: RangeFetch;
  readonly fileReferrer: FileReferrerRules;
  readonly localAddressBlock: LocalAddressBlock;
  readonly scheduler: Scheduler;
}

/** Откуда и как качать: общее для файла и HLS. */
export interface PulledSourceStart {
  /** Адрес источника (файл или корневой плейлист); остаётся в фоне. */
  readonly url: string;
  /** Referer по решению 202; `null` - заголовок не ставить. */
  readonly referrer: FileReferrer | null;
  /** Правило локальной сети сеанса на ТЕКУЩЕМ месте тени. */
  readonly rule: FileAddressRule;
  /** Очередь сеанса, куда уходят рамки и итоги. */
  readonly sink: RangeFeedSink;
}

/** Чем кончился запуск HLS. */
export type HlsStartResult =
  /** Поколение HLS запущено; `root` - номерок корневого плейлиста (его плеер и попросит первым). */
  | { readonly kind: "started"; readonly root: ResourceHandle }
  /** Адрес корня запрещён правилом (или не http(s)): в сеть не ушло ничего, прежние источники целы. */
  | { readonly kind: "address_refused" }
  /** Номерки сеанса кончились (практически недостижимо): поколение не запущено, прежние источники сняты. */
  | { readonly kind: "handles_exhausted" };

export interface SessionSourceFeeds {
  /** Запустить загрузку файла; прежний источник снят. Адрес сеанс проверил заранее (`ruleAllowsFileUrl`). */
  startFile(source: PulledSourceStart): void;
  /** Запустить поколение HLS: проверить корень, снять прежний источник, выдать номерок корня. */
  startHls(source: PulledSourceStart): HlsStartResult;
  /** `fetch_range` плеера; без файла - ничего (запрос не наш, плеер ошибся сам). */
  requestRange(request: RangeRequest): void;
  /** `fetch_resource` плеера; без живого HLS - сразу `refused` в `sink`. */
  requestResource(request: ResourceFetchRequest, sink: RangeFeedSink): void;
  /** Новое поколение сеанса (чужое, не наше): загрузки обрываются, номерки HLS забыты. */
  endGeneration(): void;
  /** Конец сеанса: то же, что `endGeneration`; запросы больше ничего не отдают. */
  dispose(): void;
}

/**
 * Сеть для источника: заслон локальных адресов внутри, Referer снаружи. Общая для файла, HLS и
 * `<track src>` (13b2): одна композиция, чтобы правила не расходились.
 */
export function networkForSource(
  parameters: Pick<SessionSourceFeedsParameters, "fetch" | "fileReferrer" | "localAddressBlock">,
  source: Pick<PulledSourceStart, "referrer" | "rule">,
): RangeFetch {
  const guarded = parameters.localAddressBlock.around(parameters.fetch, source.rule);
  return source.referrer === null ? guarded : parameters.fileReferrer.around(guarded, source.referrer);
}

export function createSessionSourceFeeds(parameters: SessionSourceFeedsParameters): SessionSourceFeeds {
  const { scheduler } = parameters;
  // Одна стойка на сеанс: счётчик номерков не сбрасывается между поколениями.
  const desk: HandleDesk = createHandleDesk();
  let fileFeed: RangeFetcher | undefined;
  let hlsFeed: HlsResourceFetcher | undefined;

  const networkFor = (source: PulledSourceStart): RangeFetch => networkForSource(parameters, source);

  /** Снять живые загрузки и забыть номерки; повтор безопасен. */
  function endActiveFeeds(): void {
    fileFeed?.dispose();
    fileFeed = undefined;
    if (hlsFeed !== undefined) {
      hlsFeed.dispose();
      hlsFeed = undefined;
      desk.beginGeneration();
    }
  }

  return {
    startFile(source) {
      endActiveFeeds();
      fileFeed = createRangeFetcher({
        url: source.url,
        fetch: networkFor(source),
        allowsAddress: (finalUrl) => ruleAllowsFileUrl(source.rule, finalUrl),
        sink: source.sink,
        scheduler,
      });
    },
    startHls(source) {
      // Запрещённый корень отказывает ДО любых изменений: сеанс всё равно кончается, но прежний источник цел.
      if (!admitsResourceUrl(source.url, source.rule)) return { kind: "address_refused" };
      endActiveFeeds();
      // Новое поколение стойки даже если HLS раньше не было: записей нет, счётчик продолжается.
      desk.beginGeneration();
      const root = desk.issue(source.url, "playlist");
      if (root === undefined) return { kind: "handles_exhausted" };
      const rule = source.rule;
      hlsFeed = createHlsResourceFetcher({
        desk,
        fetch: networkFor(source),
        allowsAddress: (finalUrl) => admitsResourceUrl(finalUrl, rule),
        // Адрес из плейлиста выдаётся номерком только после того же допуска, что и корень;
        // отказанный адрес стойка не получает - переписчик заменит его номерком «отказ».
        issueFromPlaylist: (url, kind) => (admitsResourceUrl(url, rule) ? desk.issue(url, kind) : undefined),
        sink: source.sink,
        scheduler,
      });
      return { kind: "started", root };
    },
    requestRange(request) {
      fileFeed?.request(request);
    },
    requestResource(request, sink) {
      if (hlsFeed === undefined) {
        sink.enqueue({
          kind: "message",
          message: { type: "resource_fetch_finished", request_id: request.request_id, outcome: { kind: "failed", failure: { kind: "refused" } } },
        });
        return;
      }
      hlsFeed.request(request);
    },
    endGeneration: endActiveFeeds,
    dispose: endActiveFeeds,
  };
}
