// Хук `fetch`: ловим документы субтитров, которые скачал плеер сайта (сессия 13b2).
//
// Порядок, от которого зависит работа:
//  1. оригинальный `fetch` вызывается первым, страница получает ТОТ ЖЕ промис (`wrapMethod`);
//  2. до возврата на этот промис подписывается снятый оригинал `Promise.prototype.then` — наша реакция
//     выполняется раньше страничной, и `clone()` успевает до того, как страница начнёт читать тело;
//  3. по заголовкам ответа решает `decideSubtitleResponse`: двоичное, чужие типы и огромное тело не
//     читаются вовсе — `clone()` не вызывается (иначе тело копилось бы в памяти, пока страница читает свою ветку);
//  4. копия читается с потолком (`readBodyWithLimit`), дальше — `SubtitleDownloadIntake`.
// Сбои наблюдателя странице не видны: одна отладочная запись (`reportOddity`).

import type { BinaryIntrinsics } from "../binary-view.ts";
import { wrapMethod, type OddityReporter } from "../prototype-wrap.ts";
import { readBodyWithLimit } from "./body-limit.ts";
import type { ByteTools } from "./byte-tools.ts";
import type { SubtitleDownloadIntake } from "./download-intake.ts";
import type { PromiseObserver } from "./promise-observer.ts";
import { decideSubtitleResponse, MAX_SUBTITLE_RESPONSE_BYTES } from "./response-filter.ts";
import type { BodyReader, ResponseReaders } from "./response-readers.ts";

/** Что нужно хуку. */
export interface FetchHookTools {
  readonly readers: ResponseReaders;
  readonly observePromise: PromiseObserver;
  readonly binary: BinaryIntrinsics;
  readonly bytes: ByteTools;
  readonly intake: SubtitleDownloadIntake;
  readonly reportOddity: OddityReporter;
}

/**
 * Прочитать копию и передать приём. Никогда не отклоняется: ошибка чтения — просто «документа нет»,
 * иначе страница получила бы `unhandledrejection` из-за нашего наблюдателя.
 */
async function collectAndOffer(reader: BodyReader, address: string, tools: FetchHookTools): Promise<void> {
  try {
    const body = await readBodyWithLimit(reader, tools.binary, tools.bytes, MAX_SUBTITLE_RESPONSE_BYTES);
    if (body !== null) tools.intake.offer(address, body);
  } catch {
    tools.reportOddity("reading a subtitle response failed");
  }
}

/** Реакция на ответ `fetch`: решить по заголовкам и, если стоит, сразу (синхронно) взять копию тела. */
function onFetchResponse(response: unknown, tools: FetchHookTools): void {
  if (typeof response !== "object" || response === null) return;
  const candidate = tools.readers.describe(response);
  if (!decideSubtitleResponse(candidate).read) return;
  // `clone()` — синхронно, в той же реакции: страничная реакция идёт после нашей и тело ещё не тронуто.
  const reader = tools.readers.openClonedBody(response);
  void collectAndOffer(reader, candidate.url, tools);
}

/** Поставить хук на `window.fetch`. `false` — `fetch` в окне нет, ничего не тронуто. */
export function installFetchSubtitleHook(owner: object, tools: FetchHookTools): boolean {
  return wrapMethod(owner, "fetch", tools.reportOddity, (_target, _args, result) => {
    tools.observePromise(result, (response) => {
      try {
        onFetchResponse(response, tools);
      } catch {
        tools.reportOddity("observing a fetch response failed");
      }
    });
  });
}
