// Что плеер сеанса умеет из нативного HLS — по версии протокола, о которой с ним договорились (10a2, 10a3).
//
// Единственное место, где версия протокола превращается в «этому плееру можно отдать запись / эфир».
// Сеанс (`handoff-session.ts`) и решение о задержанном видео (`held-video-choice.ts`) спрашивают только
// здесь, поэтому правило «эфир — только плееру 1.2» не расходится между кнопкой, попапом и переездом 09c2.
//
// - запись HLS (VOD) плеер понимает с протокола 1.1: `hls_source_added` и `fetch_resource`;
// - эфир HLS (live) — с протокола 1.2: без `live_window` тень не знает честного окна перемотки
//   и не может показать странице «эфир» (решение владельца 232).

import type { HlsStreamKind } from "../../shared/handoff-contract.ts";
import { pageSessionIntroducedIn, playerSessionIntroducedIn, versionIncludes, type ProtocolVersion } from "../../protocol/index.ts";

/** Что умеет плеер этого сеанса (по договорённой версии протокола): от этого зависит, какие видео ему можно отдать. */
export interface PlayerAbilities {
  /** Плеер понимает `hls_source_added` и `fetch_resource` (протокол 1.1): запись HLS. */
  readonly nativeHls: boolean;
  /** Плеер шлёт `live_window` (протокол 1.2): эфир HLS. */
  readonly hlsLive: boolean;
}

/** Возможности плеера по версии, о которой договорились в `hello`. */
export function playerAbilitiesOf(agreedProtocol: ProtocolVersion): PlayerAbilities {
  return {
    nativeHls: versionIncludes(agreedProtocol, pageSessionIntroducedIn("hls_source_added")),
    hlsLive: versionIncludes(agreedProtocol, playerSessionIntroducedIn("live_window")),
  };
}

/** Может ли плеер с такими возможностями играть нативный HLS этого вида. */
export function playerCanPlayHls(abilities: PlayerAbilities, stream: HlsStreamKind): boolean {
  // Эфир — это ещё и запись по механике (номерки, переписчик): нужны обе возможности.
  return stream === "live" ? abilities.nativeHls && abilities.hlsLive : abilities.nativeHls;
}
