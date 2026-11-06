// Слова исходов передачи, общие для попапа и плашки на странице (английский, пользовательский текст).
// Живут в shared/: фон (плашка) не должен импортировать модуль попапа ради строки, а формулировки
// двух мест не должны разойтись.

import type { ReloadReason, UnsupportedMediaForm } from "./handoff-contract.ts";

/** Форма видео человеческим словом. Запись по ключам: новая форма без слова не соберётся. */
const FORM_WORDS: Readonly<Record<UnsupportedMediaForm, string>> = {
  native_hls: "a stream (HLS) that the browser plays by itself",
  blob_file: "a file that exists only in the memory of the page",
  data_url: "a video embedded right into the page address",
  media_stream: "a live stream (camera, screen or call), not a file",
  other_scheme: "a video from an address that is not a regular web address",
  untracked_worker_media_source: "a video that the page plays in a way the extension cannot follow",
  started_before_script: "a video that started before Fastiplayer Bridge was ready",
};

/** Слово для формы видео (для текста «not supported yet»). */
export function describeUnsupportedForm(form: UnsupportedMediaForm): string {
  return FORM_WORDS[form];
}

/** Вкладка старше скрипта (сессия 10d): заголовок и причина одни в попапе и в плашке. */
export const RELOAD_NEEDED_ID = "reload_needed";
export const RELOAD_NEEDED_HEADLINE = "Reload this tab to use Fastiplayer";

/** Причина человеческим словом. Запись по ключам: новая причина без слова не соберётся. */
const RELOAD_REASON_WORDS: Readonly<Record<ReloadReason, string>> = {
  script_missing: "This tab was open before Fastiplayer Bridge was installed or updated.",
  script_outdated: "This tab was open before Fastiplayer Bridge was installed or updated.",
  stream_started_before_script: "This video started before Fastiplayer Bridge was ready.",
};

export function describeReloadReason(reason: ReloadReason): string {
  return RELOAD_REASON_WORDS[reason];
}

/**
 * Окно плеера уже было открыто (решение 216): видео играет там, но окно вперёд не вышло (на Wayland
 * экстеншену билет активации не дают). Заголовок и деталь одни и те же в попапе и в плашке.
 */
export const SWITCH_TO_PLAYER_ID = "switch_to_player";
export const SWITCH_TO_PLAYER_HEADLINE = "Now playing in the Fastiplayer window";
export const SWITCH_TO_PLAYER_DETAIL = "Switch to it to watch.";
