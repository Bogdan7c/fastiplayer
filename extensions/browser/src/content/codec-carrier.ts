// Курьер политики кодеков: ИЗОЛИРОВАННЫЙ мир, document_start, все фреймы. Регистрируется фоном
// динамически (`scripting.registerContentScripts`), поэтому значение политики лежит в самом файле
// (запечено при сборке), а не читается из `storage`: чтение асинхронно, а ответы про кодеки нужны
// страничному плееру с первой строки.
//
// Курьер ничего не знает о кодеках: он отдаёт MAIN-миру одну строку — отказываемые семейства.
// Сторона страницы — `src/page/codec-policy/carrier-rendezvous.ts`; порядок запуска двух скриптов не
// гарантирован, поэтому: на старте «tell» сразу, и «tell» в ответ на «ask» (один раз — дальше слушатель
// снимается, чтобы страница не могла опрашивать курьера бесконечно).
//
// «Tell» отменяемый (`cancelable`): MAIN-мир, приняв его, зовёт `preventDefault()`, и
// `dispatchEvent` здесь возвращает `false`. Значит, рукопожатие уже состоялось — слушатель «ask»
// снимается сразу (находка 10 ревью 09b): иначе он висел бы до первого «ask», и любой скрипт
// страницы позже узнал бы политику кодеков пользователя (лишний признак для отпечатка браузера).
// Не принято (MAIN-мир ещё не встал) — слушаем первый «ask», как раньше.
import { CODEC_POLICY_ASK_EVENT, CODEC_POLICY_TELL_EVENT, encodeRefusedFamilies, type RefusableFamily } from "../shared/codec-policy.ts";

/** Часть документа, нужная курьеру (в тестах — любой EventTarget). */
export interface CarrierDocument {
  addEventListener(type: string, listener: (event: Event) => void, options: { readonly capture: boolean }): void;
  removeEventListener(type: string, listener: (event: Event) => void, options: { readonly capture: boolean }): void;
  dispatchEvent(event: Event): boolean;
}

/**
 * Запустить курьера. `createTellEvent` обязан создавать ОТМЕНЯЕМОЕ событие (`cancelable: true`):
 * по отмене курьер узнаёт, что MAIN-мир политику принял.
 */
export function startCodecCarrier(
  doc: CarrierDocument,
  refusedFamilies: readonly RefusableFamily[],
  createTellEvent: (type: string, payload: string) => Event,
): void {
  const payload = encodeRefusedFamilies(refusedFamilies);
  const listenerOptions = { capture: true } as const;
  /** «Tell»; `"adopted"` — MAIN-мир принял политику (`preventDefault` на отменяемом событии). */
  const tell = (): TellOutcome => (doc.dispatchEvent(createTellEvent(CODEC_POLICY_TELL_EVENT, payload)) ? "not_adopted" : "adopted");
  const onAsk = (): void => {
    doc.removeEventListener(CODEC_POLICY_ASK_EVENT, onAsk, listenerOptions);
    tell();
  };
  // Сначала слушатель, потом «tell»: если страничный скрипт стартовал раньше, он услышит сразу.
  doc.addEventListener(CODEC_POLICY_ASK_EVENT, onAsk, listenerOptions);
  if (tell() === "adopted") doc.removeEventListener(CODEC_POLICY_ASK_EVENT, onAsk, listenerOptions);
}

/** Чем кончилось «tell». */
type TellOutcome = "adopted" | "not_adopted";
