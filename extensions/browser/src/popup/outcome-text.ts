// Чистые функции «исход -> текст попапа» (английский, пользовательский), как `status-text.ts`. DOM тут не трогаем.
// Смысл текстов обязателен: человек должен понять, что делать дальше (нажать play, сменить настройку,
// выбрать другое видео), а не прочитать код ошибки.

import type { HandOffTabOutcome, UnsupportedMediaForm } from "../shared/handoff-contract.ts";
import type { SurveyUnavailableReason } from "../shared/survey-contract.ts";

/**
 * Сообщение попапа. `id` — стабильное имя случая (для стенда и тестов), не для показа: по нему робот стенда
 * читает, какой исход видит человек, не разбирая английский текст.
 */
export interface NoticeView {
  readonly id: string;
  readonly headline: string;
  /** Абзацы пояснений. */
  readonly details: readonly string[];
}

/** Вопрос про локальный адрес (решение 199): текст и две кнопки. */
export interface ConsentQuestionView {
  readonly host: string;
  readonly text: string;
  readonly allowLabel: string;
  readonly denyLabel: string;
}

/** Форма видео человеческим словом. Запись по ключам: новая форма без слова не соберётся. */
const FORM_WORDS: Readonly<Record<UnsupportedMediaForm, string>> = {
  native_hls: "a stream (HLS) that the browser plays by itself",
  blob_file: "a file that exists only in the memory of the page",
  data_url: "a video embedded right into the page address",
  media_stream: "a live stream (camera, screen or call), not a file",
  other_scheme: "a video from an address that is not a regular web address",
  untracked_worker_media_source: "a video that the page plays in a way the extension cannot follow",
};

/** Слово для формы видео (для текста «not supported yet»). */
export function describeUnsupportedForm(form: UnsupportedMediaForm): string {
  return FORM_WORDS[form];
}

export const PLAYING_NOTICE: NoticeView = { id: "in_player", headline: "Playing in Fastiplayer", details: [] };
export const LOADING_NOTICE: NoticeView = { id: "loading", headline: "Looking for video on this page…", details: [] };
export const NO_ACTIVE_TAB_NOTICE: NoticeView = {
  id: "no_active_tab",
  headline: "Can't find the current tab",
  details: ["Open a page with a video and click the button again."],
};
export const DECLINED_NOTICE: NoticeView = {
  id: "declined",
  headline: "The video stays in the browser",
  details: ["Nothing was downloaded from your network."],
};

/** Исход команды «отдать вкладку» -> сообщение. `started` попап не показывает (он закрывается), но слово есть. */
export function describeOutcome(outcome: HandOffTabOutcome): NoticeView {
  switch (outcome.kind) {
    case "started":
      return PLAYING_NOTICE;
    case "no_video":
      return {
        id: "no_video",
        headline: "No video to hand over yet",
        details: ["Press play on the page, then click the button again."],
      };
    case "protected":
      return {
        id: "protected",
        headline: "This video is protected",
        details: ["Videos under copy protection (DRM) stay in the browser."],
      };
    case "not_supported_yet":
      return {
        id: "not_supported_yet",
        headline: "This video can't be handed to Fastiplayer yet",
        details: [`It is ${describeUnsupportedForm(outcome.form)}.`],
      };
    case "local_address_refused":
      return {
        id: "local_address_refused",
        headline: "Blocked: this page wants a video from your own network",
        details: ['A website must not make the extension download from your computer or home network. You can change this in the extension settings ("Local network videos from internet pages").'],
      };
    case "local_address_needs_consent":
      return { id: "local_address_needs_consent", headline: "This video is on your own network", details: [] };
    case "player_unavailable":
      return {
        id: "player_unavailable",
        headline: "Fastiplayer did not take the video",
        details: ["Make sure Fastiplayer is installed and connected (see the connection status below), then try again."],
      };
    case "video_gone":
      return {
        id: "video_gone",
        headline: "That video is no longer on the page",
        details: ["The list below is updated. Pick a video again."],
      };
    case "failed":
      return {
        id: "failed",
        headline: "Something went wrong",
        details: ["The video could not be handed over. Try again; if it keeps happening, reload the page."],
      };
  }
}

/** Вкладку нельзя опросить: понятный текст вместо передачи. */
export function describeUnavailable(reason: SurveyUnavailableReason): NoticeView {
  switch (reason) {
    case "tab_not_scriptable":
      return {
        id: "tab_not_scriptable",
        headline: "Fastiplayer can't use this page",
        details: ["Browser pages and extension stores are closed to extensions. Open a page with a video and click the button again."],
      };
    case "survey_timed_out":
      return {
        id: "survey_timed_out",
        headline: "The page did not answer in time",
        details: ["Wait a moment and click the button again."],
      };
    case "survey_failed":
      return {
        id: "survey_failed",
        headline: "Something went wrong",
        details: ["Could not look for video on this page. Reload the page and try again."],
      };
  }
}

/** Вопрос по `local_address_needs_consent{host}`. */
export function describeConsentQuestion(host: string): ConsentQuestionView {
  return {
    host,
    text: `This page wants the player to download from ${host} on your network. Allow?`,
    allowLabel: "Allow",
    denyLabel: "Don't allow",
  };
}
