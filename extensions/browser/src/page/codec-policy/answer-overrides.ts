// Переопределения трёх «вопросов о кодеках»: `MediaSource.isTypeSupported`,
// `HTMLMediaElement.prototype.canPlayType` и `MediaCapabilities.prototype.decodingInfo`.
//
// Правила (общие с prototype-wrap.ts):
//  - «оригинал» — значение на момент установки (document_start); вызывается ВСЕГДА через
//    `Reflect.apply` с тем же `this` и аргументами, поэтому его исключения, проверка `this` и побочные
//    эффекты те же, что без нас; мы лишь потом подменяем ОТВЕТ, если политика велит отказать;
//  - обёртка не бросает в страницу своё: сбой нашего решения = ответ оригинала + одна отладочная запись;
//  - имя и `length` как у оригинала, не конструктор, флаги свойства те же;
//  - чужие обёртки (блокировщики): если они были до нас — мы оборачиваем их, если будут после — они
//    позовут нашу; вызов проходит через обеих в любом порядке.
import type { CodecPolicy } from "../../shared/codec-policy.ts";
import { isObject, type OddityReporter } from "../prototype-wrap.ts";
import { shouldRefuseMime } from "./mime-refusal.ts";

/** Откуда читать действующую политику (ячейка документа). */
export interface PolicySource {
  current(): CodecPolicy;
}

/** Окно (или его часть), чьи прототипы переопределяем; любого члена может не быть (песочница, воркер). */
export interface CodecAnswerWindow {
  readonly MediaSource?: unknown;
  readonly HTMLMediaElement?: unknown;
  readonly MediaCapabilities?: unknown;
}

/** Что делает переопределение с ответом оригинала. */
type AnswerRewrite = (args: readonly unknown[], originalResult: unknown) => unknown;

/** Поставить все три переопределения; возвращает, какие из них встали (для тестов и диагностики). */
export function installCodecAnswerOverrides(
  win: CodecAnswerWindow,
  policy: PolicySource,
  reportOddity: OddityReporter,
): { readonly isTypeSupported: boolean; readonly canPlayType: boolean; readonly decodingInfo: boolean } {
  return {
    isTypeSupported: rewriteMethodAnswer(win.MediaSource, "isTypeSupported", reportOddity, (args, result) =>
      refusesFirstString(policy, args) ? false : result,
    ),
    canPlayType: rewriteMethodAnswer(prototypeOf(win.HTMLMediaElement), "canPlayType", reportOddity, (args, result) =>
      refusesFirstString(policy, args) ? "" : result,
    ),
    decodingInfo: rewriteMethodAnswer(prototypeOf(win.MediaCapabilities), "decodingInfo", reportOddity, (args, result) =>
      refusesConfiguration(policy, args[0]) ? refuseDecodingInfo(result, reportOddity) : result,
    ),
  };
}

function prototypeOf(constructorLike: unknown): unknown {
  return isObject(constructorLike) ? (constructorLike as { readonly prototype?: unknown }).prototype : undefined;
}

/**
 * Заменить метод обёрткой, которая отдаёт `rewrite(args, ответОригинала)`. `false` — метода нет,
 * ничего не тронуто.
 */
function rewriteMethodAnswer(owner: unknown, name: string, reportOddity: OddityReporter, rewrite: AnswerRewrite): boolean {
  if (!isObject(owner)) return false;
  const descriptor = Object.getOwnPropertyDescriptor(owner, name);
  const original: unknown = descriptor?.value;
  if (descriptor === undefined || typeof original !== "function") return false;
  const wrapper = {
    [name](this: unknown, ...args: unknown[]): unknown {
      const result: unknown = Reflect.apply(original, this, args);
      try {
        return rewrite(args, result);
      } catch {
        reportOddity(`codec answer rewrite failed: ${name}`);
        return result;
      }
    },
  }[name];
  if (wrapper === undefined) return false;
  Object.defineProperty(wrapper, "length", { value: (original as { readonly length: number }).length });
  Object.defineProperty(owner, name, { ...descriptor, value: wrapper });
  return true;
}

/**
 * Отказывать ли по первому аргументу-строке. Не строка (в том числе отсутствие аргумента) — не наше
 * дело: оригинал ответит или бросит сам, как без нас.
 */
function refusesFirstString(policy: PolicySource, args: readonly unknown[]): boolean {
  const mime = args[0];
  return typeof mime === "string" && shouldRefuseMime(policy.current(), mime);
}

/** Отказывать ли по конфигурации `decodingInfo` (`video.contentType` / `audio.contentType`). */
function refusesConfiguration(policy: PolicySource, configuration: unknown): boolean {
  if (!isObject(configuration)) return false;
  const type = (configuration as { readonly type?: unknown }).type;
  // Только «file» и «media-source»; «webrtc» и мусор остаются за браузером.
  if (type !== "file" && type !== "media-source") return false;
  return ["video", "audio"].some((track) => {
    const description = (configuration as Record<string, unknown>)[track];
    const contentType = isObject(description) ? (description as { readonly contentType?: unknown }).contentType : undefined;
    return typeof contentType === "string" && shouldRefuseMime(policy.current(), contentType);
  });
}

/**
 * Ответ `decodingInfo` при отказе: тот же объект-ответ оригинала (чтобы форма и `configuration`
 * остались), но `supported/smooth/powerEfficient` ложны, а `keySystemAccess`, если он есть, — `null`.
 * Отказ оригинала (rejected promise) уходит странице как был.
 */
function refuseDecodingInfo(originalResult: unknown, reportOddity: OddityReporter): unknown {
  if (!(originalResult instanceof Promise)) return originalResult;
  return originalResult.then((info: unknown) => {
    if (!isObject(info)) return info;
    try {
      const refused: Record<string, unknown> = { ...info };
      refused["supported"] = false;
      refused["smooth"] = false;
      refused["powerEfficient"] = false;
      if ("keySystemAccess" in refused) refused["keySystemAccess"] = null;
      return refused;
    } catch {
      reportOddity("codec answer rewrite failed: decodingInfo result");
      return info;
    }
  });
}
