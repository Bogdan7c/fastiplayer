// Текстовые сообщения сеанса от плеера к экстеншену (странице). Зеркало session/player.rs.
//
// Часы ведёт плеер: экстеншен получает позицию, паузу, скорость, «жду данных» и
// «конец» и сам генерирует события муляжа видеоэлемента.

import { ProtocolFailure } from "../errors.ts";
import { type ResourceByteRange, resourceByteRangeCodec, resourceFetchLengthCodec } from "../resource.ts";
import { type Codec, stringEnum, taggedUnion, type TaggedUnionCodec } from "../schema.ts";
import {
  type ResourceHandle,
  type StreamGeneration,
  clockSequenceCodec,
  rangeRequestIdCodec,
  resourceHandleCodec,
  seekIdCodec,
  streamGenerationCodec,
} from "../stream.ts";
import { variantIdCodec } from "../text.ts";
import {
  type ByteCount,
  type MediaTimeMicros,
  type PlaybackRatePermille,
  byteOffsetCodec,
  mediaTimeCodec,
  playbackRateCodec,
  rangeFetchLengthCodec,
} from "../time.ts";
import { type LiveSeekable, checkLiveWindowWithinEdge, liveSeekableCodec } from "./live-window.ts";
import { type VariantKind, variantKindCodec } from "./variants.ts";

/** Состояние часов плеера. */
export type ClockState = "playing" | "paused" | "waiting" | "ended";

/** Принимает ли плеер новые байты. */
export type BufferIntake = "accepting" | "full";

/** В каком состоянии продолжить на странице после возврата. */
export type ResumeState = "playing" | "paused";

/** Почему плеер отверг поколение. */
export type StreamRejection =
  | "protected_content"
  | "unsupported_codec"
  | "unparsable_media"
  | "file_unavailable"
  | "playback_failed";

/** Сообщения сеанса от плеера к странице. */
export type PlayerSessionMessage =
  | { readonly type: "attached" }
  | {
      readonly type: "clock";
      readonly seq: number;
      readonly position_us: MediaTimeMicros;
      readonly state: ClockState;
      readonly rate: PlaybackRatePermille;
    }
  | {
      readonly type: "buffer_state";
      readonly generation: StreamGeneration;
      readonly ahead_us: MediaTimeMicros;
      readonly intake: BufferIntake;
    }
  | { readonly type: "play" }
  | { readonly type: "pause" }
  | { readonly type: "seek"; readonly seek_id: number; readonly target_us: MediaTimeMicros }
  | { readonly type: "set_rate"; readonly rate: PlaybackRatePermille }
  | { readonly type: "return_to_page"; readonly position_us: MediaTimeMicros; readonly resume: ResumeState }
  | { readonly type: "select_variant"; readonly kind: VariantKind; readonly variant_id: string }
  | { readonly type: "next_episode" }
  | { readonly type: "skip_ad" }
  | {
      readonly type: "fetch_range";
      readonly request_id: number;
      readonly offset: ByteCount;
      readonly length: ByteCount;
    }
  /**
   * Протокол 1.1: дай ресурс HLS по номерку. Номер запроса - из того же счётчика,
   * что у `fetch_range`; ответ - рамки вида 2 со смещениями внутри тела с 0 и
   * `resource_fetch_finished`.
   */
  | {
      readonly type: "fetch_resource";
      readonly request_id: number;
      readonly resource: ResourceHandle;
      readonly byte_range: ResourceByteRange;
      readonly max_bytes: ByteCount;
    }
  /**
   * Протокол 1.2 (решение 232): окно эфира live HLS - край и честное окно перемотки,
   * всё во времени плеера (как `clock.position_us`). `start_us <= end_us <= edge_us`.
   */
  | {
      readonly type: "live_window";
      readonly generation: StreamGeneration;
      readonly edge_us: MediaTimeMicros;
      readonly seekable: LiveSeekable;
    }
  /**
   * Протокол 1.4 (решение 241): зритель нажал play, пока страница ушла, - вернуть страницу на
   * этой секунде (время плеера, как `clock.position_us`). Ответ - `page_returned` или
   * `page_reopen_failed`.
   */
  | { readonly type: "reopen_page"; readonly position_us: MediaTimeMicros }
  | { readonly type: "stream_rejected"; readonly generation: StreamGeneration; readonly reason: StreamRejection };

const clockStateCodec: Codec<ClockState> = stringEnum<ClockState>(["playing", "paused", "waiting", "ended"]);

/** Кодек сообщений сеанса от плеера. */
export const playerSessionCodec: TaggedUnionCodec<PlayerSessionMessage> = taggedUnion<PlayerSessionMessage, "type">("type", {
  attached: {},
  clock: {
    seq: clockSequenceCodec,
    position_us: mediaTimeCodec,
    state: clockStateCodec,
    rate: playbackRateCodec,
  },
  buffer_state: {
    generation: streamGenerationCodec,
    ahead_us: mediaTimeCodec,
    intake: stringEnum<BufferIntake>(["accepting", "full"]),
  },
  play: {},
  pause: {},
  seek: { seek_id: seekIdCodec, target_us: mediaTimeCodec },
  set_rate: { rate: playbackRateCodec },
  return_to_page: { position_us: mediaTimeCodec, resume: stringEnum<ResumeState>(["playing", "paused"]) },
  select_variant: { kind: variantKindCodec, variant_id: variantIdCodec },
  next_episode: {},
  skip_ad: {},
  fetch_range: { request_id: rangeRequestIdCodec, offset: byteOffsetCodec, length: rangeFetchLengthCodec },
  fetch_resource: {
    request_id: rangeRequestIdCodec,
    resource: resourceHandleCodec,
    byte_range: resourceByteRangeCodec,
    max_bytes: resourceFetchLengthCodec,
  },
  live_window: {
    generation: streamGenerationCodec,
    edge_us: mediaTimeCodec,
    seekable: liveSeekableCodec,
  },
  reopen_page: { position_us: mediaTimeCodec },
  stream_rejected: {
    generation: streamGenerationCodec,
    reason: stringEnum<StreamRejection>([
      "protected_content",
      "unsupported_codec",
      "unparsable_media",
      "file_unavailable",
      "playback_failed",
    ]),
  },
});

/**
 * Проверки между полями (виды ошибок - как в Rust):
 * - `fetch_resource`: кусок длиннее потолка ответа - запрос сам себе противоречит
 *   (`number_out_of_range` у `max_bytes`);
 * - `live_window`: начало окна не позже конца, конец не дальше края (`checkLiveWindowWithinEdge`).
 */
export function validatePlayerSession(message: PlayerSessionMessage): void {
  if (message.type === "fetch_resource" && message.byte_range.kind === "slice" && message.byte_range.length > message.max_bytes) {
    throw new ProtocolFailure("number_out_of_range").prependPath("max_bytes");
  }
  if (message.type === "live_window") {
    checkLiveWindowWithinEdge(message.edge_us, message.seekable);
  }
}
