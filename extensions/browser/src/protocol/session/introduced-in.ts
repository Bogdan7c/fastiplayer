// В какой версии протокола появилось каждое сообщение сеанса. Зеркало
// `PageSessionMessage::introduced_in` / `PlayerSessionMessage::introduced_in` (version.rs, правило 5).
//
// Сообщение новее общей версии сеанса сторона не шлёт, а получатель его отвергает.
// Таблицы полные по типам (`Record` по объединению `type`): новое сообщение без версии не
// соберётся. Сверка с Rust - общая фикстура fixtures/message-versions.json.

import {
  PROTOCOL_VERSION_1_0,
  PROTOCOL_VERSION_1_1,
  PROTOCOL_VERSION_1_2,
  PROTOCOL_VERSION_1_3,
  PROTOCOL_VERSION_1_4,
  PROTOCOL_VERSION_1_5,
  PROTOCOL_VERSION_1_6,
  type ProtocolVersion,
} from "../version.ts";
import type { PageSessionMessage, TabGoneReason } from "./page.ts";
import { type PageReopenFailure } from "./page-reopen.ts";
import type { PlayerSessionMessage } from "./player.ts";

const PAGE_SESSION_INTRODUCED_IN: Readonly<Record<PageSessionMessage["type"], ProtocolVersion>> = {
  attach: PROTOCOL_VERSION_1_0,
  generation_started: PROTOCOL_VERSION_1_0,
  file_source_added: PROTOCOL_VERSION_1_0,
  hls_source_added: PROTOCOL_VERSION_1_1,
  source_buffer_added: PROTOCOL_VERSION_1_0,
  source_buffer_type_changed: PROTOCOL_VERSION_1_0,
  timestamp_offset_changed: PROTOCOL_VERSION_1_0,
  source_buffer_removed: PROTOCOL_VERSION_1_0,
  source_buffer_aborted: PROTOCOL_VERSION_1_0,
  seek_started: PROTOCOL_VERSION_1_0,
  duration_changed: PROTOCOL_VERSION_1_0,
  end_of_stream: PROTOCOL_VERSION_1_0,
  page_playback: PROTOCOL_VERSION_1_0,
  ad_state: PROTOCOL_VERSION_1_0,
  variant_menu_changed: PROTOCOL_VERSION_1_0,
  range_fetch_finished: PROTOCOL_VERSION_1_0,
  resource_fetch_finished: PROTOCOL_VERSION_1_1,
  tab_gone: PROTOCOL_VERSION_1_0,
  page_left: PROTOCOL_VERSION_1_4,
  page_returned: PROTOCOL_VERSION_1_4,
  page_reopen_failed: PROTOCOL_VERSION_1_4,
  subtitle_track_offered: PROTOCOL_VERSION_1_5,
};

const PLAYER_SESSION_INTRODUCED_IN: Readonly<Record<PlayerSessionMessage["type"], ProtocolVersion>> = {
  attached: PROTOCOL_VERSION_1_0,
  clock: PROTOCOL_VERSION_1_0,
  buffer_state: PROTOCOL_VERSION_1_0,
  play: PROTOCOL_VERSION_1_0,
  pause: PROTOCOL_VERSION_1_0,
  seek: PROTOCOL_VERSION_1_0,
  set_rate: PROTOCOL_VERSION_1_0,
  return_to_page: PROTOCOL_VERSION_1_0,
  select_variant: PROTOCOL_VERSION_1_0,
  next_episode: PROTOCOL_VERSION_1_0,
  skip_ad: PROTOCOL_VERSION_1_0,
  fetch_range: PROTOCOL_VERSION_1_0,
  fetch_resource: PROTOCOL_VERSION_1_1,
  live_window: PROTOCOL_VERSION_1_2,
  reopen_page: PROTOCOL_VERSION_1_4,
  show_page: PROTOCOL_VERSION_1_6,
  stream_rejected: PROTOCOL_VERSION_1_0,
};

/** В какой версии появилось сообщение страницы с этим `type`. */
export function pageSessionIntroducedIn(type: PageSessionMessage["type"]): ProtocolVersion {
  return PAGE_SESSION_INTRODUCED_IN[type];
}

/** В какой версии появилось сообщение плеера с этим `type`. */
export function playerSessionIntroducedIn(type: PlayerSessionMessage["type"]): ProtocolVersion {
  return PLAYER_SESSION_INTRODUCED_IN[type];
}

/**
 * В какой версии появилась причина ухода вкладки. Таблицы версий выше знают только типы
 * сообщений; новое ЗНАЧЕНИЕ старого сообщения (1.3: `page_player_failed`) версионируется
 * здесь. Зеркало `TabGoneReason::introduced_in` (page.rs).
 */
const TAB_GONE_REASON_INTRODUCED_IN: Readonly<Record<TabGoneReason, ProtocolVersion>> = {
  closed: PROTOCOL_VERSION_1_0,
  navigated_away: PROTOCOL_VERSION_1_0,
  browser_closing: PROTOCOL_VERSION_1_0,
  page_player_failed: PROTOCOL_VERSION_1_3,
};

/** В какой версии появилась причина `tab_gone{reason}`. */
export function tabGoneReasonIntroducedIn(reason: TabGoneReason): ProtocolVersion {
  return TAB_GONE_REASON_INTRODUCED_IN[reason];
}

/**
 * В какой версии появился отказ возврата страницы (все - вместе с `page_reopen_failed`, 1.4).
 * Зеркало `PageReopenFailure::introduced_in` (page_reopen.rs); таблица значений -
 * fixtures/value-versions.json.
 */
const PAGE_REOPEN_FAILURE_INTRODUCED_IN: Readonly<Record<PageReopenFailure, ProtocolVersion>> = {
  no_browser_window: PROTOCOL_VERSION_1_4,
  page_did_not_load: PROTOCOL_VERSION_1_4,
  not_the_same_video: PROTOCOL_VERSION_1_4,
  video_did_not_start: PROTOCOL_VERSION_1_4,
};

/** В какой версии появился отказ `page_reopen_failed{reason}`. */
export function pageReopenFailureIntroducedIn(reason: PageReopenFailure): ProtocolVersion {
  return PAGE_REOPEN_FAILURE_INTRODUCED_IN[reason];
}

/**
 * Самая старая общая версия, при которой страница вправе прислать это сообщение: самая новая из
 * версий его типа и его значений. Зеркало `PageSessionMessage::required_version` (page.rs).
 */
export function pageSessionRequiredVersion(message: PageSessionMessage): ProtocolVersion {
  const typeVersion = PAGE_SESSION_INTRODUCED_IN[message.type];
  const valueVersion = (() => {
    switch (message.type) {
      case "tab_gone":
      case "page_left":
        return TAB_GONE_REASON_INTRODUCED_IN[message.reason];
      case "page_reopen_failed":
        return PAGE_REOPEN_FAILURE_INTRODUCED_IN[message.reason];
      default:
        return typeVersion;
    }
  })();
  return valueVersion.minor > typeVersion.minor ? valueVersion : typeVersion;
}
