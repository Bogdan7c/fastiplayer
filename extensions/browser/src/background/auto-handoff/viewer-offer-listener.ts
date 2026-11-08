// Слушатель выбора человека на предложении «Ask» (сессия 12b; договор — src/shared/viewer-offer-contract.ts).
//
// Принимаем только свой контент-скрипт из вкладки: у сообщения есть `sender.tab` и `sender.frameId`, а
// `sender.id` — наш. Страница сайта `runtime`-сообщений слать не может; форма сообщения проверяется строго.
// Вкладку и фрейм берём из отправителя, а не из сообщения. Ответ приходит позже: канал держим (`true`).
import type { FrameSender } from "../handoff/hand-off-command.ts";
import { parseViewerOfferChoiceMessage, type ViewerOfferAnswer, type ViewerOfferChoiceMessage } from "../../shared/viewer-offer-contract.ts";

export function createViewerOfferChoiceListener(
  extensionId: string,
  onChoice: (tabId: number, frameId: number, message: ViewerOfferChoiceMessage) => Promise<ViewerOfferAnswer>,
): (message: unknown, sender: FrameSender, sendResponse: (answer: ViewerOfferAnswer) => void) => boolean {
  return (message, sender, sendResponse) => {
    const tabId = sender.tab?.id;
    if (sender.id !== extensionId || tabId === undefined || sender.frameId === undefined) return false;
    const choice = parseViewerOfferChoiceMessage(message);
    if (choice === undefined) return false;
    // `onChoice` не отклоняется по договору (`ViewerOffers.choose`); страховка — честное «предложения нет».
    onChoice(tabId, sender.frameId, choice).then(sendResponse, () => sendResponse("no_offer"));
    return true;
  };
}
