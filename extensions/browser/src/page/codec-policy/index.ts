// Политика видеокодеков в MAIN-мире: сборка ячейки, переопределений и рукопожатия с курьером.
// Ставится на document_start из `src/page/main.ts`, до скриптов страницы.
import type { OddityReporter } from "../prototype-wrap.ts";
import { installCodecAnswerOverrides, type CodecAnswerWindow } from "./answer-overrides.ts";
import { startCarrierRendezvous, type RendezvousDocument } from "./carrier-rendezvous.ts";
import { createPolicyCell, type PolicyCell } from "./policy-cell.ts";

/** Окно, против которого ставится политика (в тестах — фейк с тем же составом). */
export interface CodecPolicyWindow extends CodecAnswerWindow {
  readonly document: RendezvousDocument;
  readonly CustomEvent: typeof CustomEvent;
}

/** Что отдаёт установка: чтение действующей политики (для воркерной обёртки пакета C). */
export interface CodecPolicyHandle {
  /** Синхронно: действующая политика документа сейчас (прозрачная, пока курьер не ответил). */
  readonly policy: Pick<PolicyCell, "current">;
}

/**
 * Поставить переопределения и начать рукопожатие. Не бросает: сбой одной части не мешает другой
 * (в худшем случае страница видит ответы браузера как обычно).
 */
export function installCodecPolicy(win: CodecPolicyWindow, reportOddity: OddityReporter): CodecPolicyHandle {
  const cell = createPolicyCell();
  try {
    installCodecAnswerOverrides(win, cell, reportOddity);
  } catch {
    reportOddity("codec answer overrides failed to install");
  }
  try {
    startCarrierRendezvous(win.document, cell, (type) => new win.CustomEvent(type));
  } catch {
    reportOddity("codec policy rendezvous failed to start");
  }
  return { policy: cell };
}
