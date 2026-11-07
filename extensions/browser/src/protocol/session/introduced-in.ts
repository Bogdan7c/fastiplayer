// В какой версии протокола появилось каждое сообщение сеанса. Зеркало
// `PageSessionMessage::introduced_in` / `PlayerSessionMessage::introduced_in` (version.rs, правило 5).
//
// Сообщение новее общей версии сеанса сторона не шлёт, а получатель его отвергает.
// Таблицы полные по типам (`Record` по объединению `type`): новое сообщение без версии не
// соберётся. Сверка с Rust - общая фикстура fixtures/message-versions.json.

import { PROTOCOL_VERSION_1_0, PROTOCOL_VERSION_1_1, type ProtocolVersion } from "../version.ts";
import type { PageSessionMessage } from "./page.ts";
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
