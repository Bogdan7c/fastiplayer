// Запуск источника, который качает фон сам (файл или нативный HLS): перевод в сообщения плееру и запуск
// загрузчика в правильном порядке. Вынесено из `handoff-session.ts` (сессия 10a2), чтобы сеанс решал
// только «можно ли сейчас и что делать с отказом», а порядок «фид -> перевод» жил в одном месте.
//
// Порядок для HLS обратный файлу: номерок корня нужен сообщению `hls_source_added`, поэтому сначала
// фид (проверка корня, выдача номерка), потом перевод. Файлу номерок не нужен: сначала перевод, и загрузчик
// запускается, только если перевод удался (поколение открыто).

import type { ResourceHandle } from "../../protocol/index.ts";
import type { FileReferrer } from "../../shared/referrer-policy.ts";
import type { DocumentTranslator, OutgoingItem, PageStreamProblem } from "./page-event-translator.ts";
import type { PulledSourceStart, SessionSourceFeeds } from "./session-source-feeds.ts";

/** Источник, названный страницей. */
export interface PulledSourceEvent {
  readonly form: "file" | "hls";
  readonly url: string;
  readonly referrer: FileReferrer | null;
}

/** Referer закачки по донесению страницы (решение 202): `null` - заголовок не ставим. */
export function referrerOf(event: { readonly referrer_origin: string | null; readonly referrer_policy: FileReferrer["policy"] }): FileReferrer | null {
  return event.referrer_origin === null ? null : { origin: event.referrer_origin, policy: event.referrer_policy };
}

/** Чем кончился запуск. */
export type PulledSourceStartResult =
  /** Поколение открыто, загрузчик запущен; `items` нужно отдать очереди сеанса. */
  | { readonly kind: "started"; readonly items: readonly OutgoingItem[] }
  /** Адрес запрещён правилом: загрузчик не запущен, в сеть не ушло ничего. */
  | { readonly kind: "address_refused" }
  /** Поколение или номерки исчерпаны: поток честно переслать нельзя. */
  | { readonly kind: "stream_problem"; readonly problem: PageStreamProblem };

export function startPulledSource(
  feeds: SessionSourceFeeds,
  translator: DocumentTranslator,
  source: PulledSourceEvent,
  start: PulledSourceStart,
): PulledSourceStartResult {
  if (source.form === "file") {
    const translated = translator.translateFileSource();
    if (!translated.ok) return { kind: "stream_problem", problem: translated.problem };
    feeds.startFile(start);
    return { kind: "started", items: translated.items };
  }
  const started = feeds.startHls(start);
  switch (started.kind) {
    case "address_refused":
      return { kind: "address_refused" };
    case "handles_exhausted":
      return { kind: "stream_problem", problem: "resource_handles_exhausted" };
    case "started":
      return translateHls(translator, started.root);
  }
}

function translateHls(translator: DocumentTranslator, root: ResourceHandle): PulledSourceStartResult {
  const translated = translator.translateHlsSource(root);
  return translated.ok ? { kind: "started", items: translated.items } : { kind: "stream_problem", problem: translated.problem };
}
