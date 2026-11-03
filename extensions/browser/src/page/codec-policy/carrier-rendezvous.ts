// MAIN-мир, сторона страницы рукопожатия с курьером (`src/content/codec-carrier.ts`).
//
// Зачем так: настройка лежит в `storage` (асинхронно), а ответы про кодеки нужны страничному плееру
// с первой строки. Поэтому фон регистрирует динамический контент-скрипт-курьер с запечённым набором
// семейств (document_start, все фреймы); курьер и этот скрипт встречаются СИНХРОННЫМИ DOM-событиями.
// Порядок запуска двух скриптов не гарантирован, поэтому рукопожатие не зависит от порядка:
//   - здесь на старте: слушатель «tell», затем событие «ask»;
//   - курьер на старте: событие «tell» и ответ на «ask» повторным «tell».
// Кто бы ни стартовал первым, второй либо услышит «tell» сразу, либо вызовет его «ask».
// Принятое «tell» отменяется (`preventDefault`): так курьер узнаёт, что ждать «ask» больше незачем.
// Нет ответа — политика прозрачная.
import { CODEC_POLICY_ASK_EVENT, CODEC_POLICY_TELL_EVENT, decodeRefusedFamilies, refuse } from "../../shared/codec-policy.ts";
import type { PolicyCell } from "./policy-cell.ts";

/** Часть документа, нужная рукопожатию (в тестах — любой EventTarget). */
export interface RendezvousDocument {
  addEventListener(type: string, listener: (event: Event) => void, options: { readonly capture: boolean }): void;
  removeEventListener(type: string, listener: (event: Event) => void, options: { readonly capture: boolean }): void;
  dispatchEvent(event: Event): boolean;
}

/**
 * Начать рукопожатие. Принимается только ПЕРВОЕ валидное «tell» (нагрузка — примитивная строка, строго
 * разобранная `decodeRefusedFamilies`); оно гасится (`stopImmediatePropagation`), после чего слушатель
 * снимается и последующие «tell» (в том числе подделки страницы) игнорируются. Глобалов не создаётся.
 */
export function startCarrierRendezvous(doc: RendezvousDocument, cell: PolicyCell, createEvent: (type: string) => Event): void {
  const listenerOptions = { capture: true } as const;
  const onTell = (event: Event): void => {
    // `detail` — только примитив: объект из изолированного мира в Firefox пришёл бы обёрткой Xray.
    const families = decodeRefusedFamilies((event as { readonly detail?: unknown }).detail);
    if (families === undefined) return; // не наше или подделка: слушаем дальше
    event.stopImmediatePropagation();
    // Отмена — ответ курьеру «принято»: он сразу снимает слушатель «ask» (находка 10 ревью 09b).
    event.preventDefault();
    doc.removeEventListener(CODEC_POLICY_TELL_EVENT, onTell, listenerOptions);
    cell.adoptFromCarrier(refuse(families));
  };
  doc.addEventListener(CODEC_POLICY_TELL_EVENT, onTell, listenerOptions);
  // Курьер мог стартовать раньше и уже промолчать в пустоту: просим повторить.
  doc.dispatchEvent(createEvent(CODEC_POLICY_ASK_EVENT));
}
