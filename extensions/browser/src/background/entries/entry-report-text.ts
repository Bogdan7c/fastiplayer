// Итог входа -> плашка на странице (английский, пользовательский текст). Чистая функция, DOM не трогает.
//
// Плашка не обязана дословно повторять попап: у неё нет «списка ниже» и «статуса ниже», поэтому следующий
// шаг называется явно («нажми кнопку Fastiplayer на панели браузера»). Формы слов для `not_supported_yet` и
// совет «переключись на окно плеера» берутся из shared/outcome-words.ts: один источник с попапом.

import type { EntryReport } from "../../shared/entry-contract.ts";
import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import { describeReloadReason, describeUnsupportedForm, RELOAD_NEEDED_HEADLINE, RELOAD_NEEDED_ID, SWITCH_TO_PLAYER_DETAIL, SWITCH_TO_PLAYER_HEADLINE, SWITCH_TO_PLAYER_ID } from "../../shared/outcome-words.ts";

/**
 * Что сообщает вход. Кроме исхода команды передачи есть два исхода, которых у команды нет: вход по ссылке
 * не дождался видео и вход по ссылке не смог открыть вкладку.
 */
export type EntryResult =
  | { readonly kind: "outcome"; readonly outcome: HandOffTabOutcome }
  /** Вкладку по ссылке открыли, но видео за срок ожидания не запустилось (вкладку не закрываем). */
  | { readonly kind: "link_no_video_started" }
  /** Вкладку по ссылке открыть не удалось (плашка идёт на исходную вкладку). */
  | { readonly kind: "link_not_opened" }
  /** Поздний вход: страница перезагружена, сама видео не запустила, ждём нажатия play (сессия 10d). */
  | { readonly kind: "late_press_play" }
  /** Поздний вход: за срок ожидания видео не заиграло. */
  | { readonly kind: "late_no_video_started" }
  /** Поздний вход: вкладку не удалось перезагрузить. */
  | { readonly kind: "late_reload_failed" }
  /** Поздний вход: видео в плеере, но сайт не дал встать на прежнюю секунду. */
  | { readonly kind: "late_started_from_site_position" };

const refusal = (id: string, headline: string, ...details: string[]): EntryReport => ({ id, tone: "refusal", headline, details });

/** Текст по исходу команды передачи. Исчерпывающий `switch`: новый вид исхода без текста не соберётся. */
function describeOutcome(outcome: HandOffTabOutcome): EntryReport {
  switch (outcome.kind) {
    case "started":
      return outcome.player_window === "already_open"
        ? { id: SWITCH_TO_PLAYER_ID, tone: "success", headline: SWITCH_TO_PLAYER_HEADLINE, details: [SWITCH_TO_PLAYER_DETAIL] }
        : { id: "opened", tone: "success", headline: "Opened in Fastiplayer", details: [] };
    case "no_video":
      return refusal("no_video", "No video to hand over yet", "Press play on the page, then try again.");
    case "reload_needed":
      return refusal(RELOAD_NEEDED_ID, RELOAD_NEEDED_HEADLINE, describeReloadReason(outcome.reason), "Click the Fastiplayer button on the browser toolbar to reload it.");
    case "protected":
      return refusal("protected", "This video is protected", "Videos under copy protection (DRM) stay in the browser.");
    case "not_supported_yet":
      return refusal("not_supported_yet", "This video can't be handed to Fastiplayer yet", `It is ${describeUnsupportedForm(outcome.form)}.`);
    case "local_address_refused":
      return refusal(
        "local_address_refused",
        "Blocked: this page wants a video from your own network",
        'You can change this in the extension settings ("Local network videos from internet pages").',
      );
    case "local_address_needs_consent":
      return refusal(
        "local_address_needs_consent",
        `This video is on your own network (${outcome.host})`,
        "Click the Fastiplayer button on the browser toolbar to allow it.",
      );
    case "player_unavailable":
      return refusal("player_unavailable", "Fastiplayer did not take the video", "Click the Fastiplayer button on the browser toolbar to see the connection status.");
    case "video_gone":
      return refusal("video_gone", "That video is no longer on the page", "Try again.");
    case "failed":
      return refusal("failed", "Something went wrong", "The video could not be handed over. Try again; if it keeps happening, reload the page.");
  }
}

export function describeEntryResult(result: EntryResult): EntryReport {
  switch (result.kind) {
    case "outcome":
      return describeOutcome(result.outcome);
    case "link_no_video_started":
      return refusal(
        "link_no_video_started",
        "No video started on this page",
        "Press play on the page to send it to Fastiplayer, or click the Fastiplayer button on the browser toolbar.",
      );
    case "link_not_opened":
      return refusal("link_not_opened", "Could not open this link", "The link was not opened. Try opening it in a tab yourself and use the Fastiplayer button.");
    case "late_press_play":
      return { id: "late_press_play", tone: "hint", headline: "Press play on the page", details: ["The video will open in Fastiplayer when it starts."] };
    case "late_no_video_started":
      return refusal("late_no_video_started", "No video started after the reload", "Press play on the page, then click the Fastiplayer button on the browser toolbar.");
    case "late_reload_failed":
      return refusal("late_reload_failed", "Could not reload this tab", "Reload it yourself, then click the Fastiplayer button on the browser toolbar.");
    case "late_started_from_site_position":
      return { id: "opened_from_site_position", tone: "success", headline: "Opened in Fastiplayer", details: ["The site chose the starting point, so the video may not continue from the same second."] };
  }
}
