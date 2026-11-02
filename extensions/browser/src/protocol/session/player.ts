// Текстовые сообщения сеанса от плеера к экстеншену (странице). Зеркало session/player.rs.
//
// Часы ведёт плеер: экстеншен получает позицию, паузу, скорость, «жду данных» и
// «конец» и сам генерирует события муляжа видеоэлемента.

import { type Codec, stringEnum, taggedUnion, type TaggedUnionCodec } from "../schema.ts";
import {
  type StreamGeneration,
  clockSequenceCodec,
  rangeRequestIdCodec,
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
  | {
      readonly type: "fetch_range";
      readonly request_id: number;
      readonly offset: ByteCount;
      readonly length: ByteCount;
    }
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
  fetch_range: { request_id: rangeRequestIdCodec, offset: byteOffsetCodec, length: rangeFetchLengthCodec },
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
