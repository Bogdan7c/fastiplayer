// Текстовые сообщения сеанса от экстеншена (страницы) к плееру. Зеркало session/page.rs.
//
// Первое сообщение - всегда `attach` с пропуском. Дальше экстеншен пересказывает,
// что плеер страницы делает с MSE (это нужно приёмнику, чтобы собрать байты рамок
// в правильные дорожки), и сообщает события страницы.

import { ProtocolFailure } from "../errors.ts";
import { type Codec, stringEnum, taggedUnion, type TaggedUnionCodec } from "../schema.ts";
import {
  type SourceBufferIndex,
  type StreamGeneration,
  rangeRequestIdCodec,
  seekIdCodec,
  sourceBufferIndexCodec,
  streamGenerationCodec,
} from "../stream.ts";
import { mimeTypeCodec, sessionKeyCodec, sessionTokenCodec } from "../text.ts";
import {
  type ByteCount,
  type MediaDuration,
  type MediaTimeMicros,
  type MediaTimeOffsetMicros,
  type PlaybackRatePermille,
  byteOffsetCodec,
  mediaDurationCodec,
  mediaTimeCodec,
  mediaTimeOffsetCodec,
  playbackRateCodec,
} from "../time.ts";
import { type VariantMenu, variantMenuCodec, validateVariantMenu } from "./variants.ts";

/** Конец удаляемого промежутка буфера. */
export type RemovalEnd =
  | { readonly kind: "at"; readonly time_us: MediaTimeMicros }
  | { readonly kind: "end_of_timeline" };

/** Кто начал перемотку. */
export type SeekCause =
  | { readonly kind: "player_seek"; readonly seek_id: number }
  | { readonly kind: "page_seek" };

/** Запрос страницы к часам плеера. */
export type PagePlaybackRequest =
  | { readonly kind: "play" }
  | { readonly kind: "pause" }
  | { readonly kind: "set_rate"; readonly rate: PlaybackRatePermille };

/** Размер ресурса, как его узнала страница из ответа сервера. */
export type ResourceSize =
  | { readonly kind: "known"; readonly size_bytes: ByteCount }
  | { readonly kind: "unknown" };

/** Чем кончилась неудача загрузки куска файла. */
export type RangeFetchFailure = "network_error" | "http_error" | "resource_changed" | "out_of_range";

/** Итог запроса байтов файла. */
export type RangeFetchOutcome =
  | { readonly kind: "completed"; readonly resource_size: ResourceSize }
  | { readonly kind: "failed"; readonly failure: RangeFetchFailure };

/** Почему вкладка ушла. */
export type TabGoneReason = "closed" | "navigated_away" | "browser_closing";

/** Сообщения сеанса от страницы к плееру (поле `type` - имя в snake_case). */
export type PageSessionMessage =
  | { readonly type: "attach"; readonly session_key: string; readonly token: string }
  | { readonly type: "generation_started"; readonly generation: StreamGeneration }
  | { readonly type: "file_source_added"; readonly generation: StreamGeneration }
  | {
      readonly type: "source_buffer_added";
      readonly generation: StreamGeneration;
      readonly source_buffer: SourceBufferIndex;
      readonly mime_type: string;
    }
  | {
      readonly type: "source_buffer_type_changed";
      readonly generation: StreamGeneration;
      readonly source_buffer: SourceBufferIndex;
      readonly mime_type: string;
    }
  | {
      readonly type: "timestamp_offset_changed";
      readonly generation: StreamGeneration;
      readonly source_buffer: SourceBufferIndex;
      readonly offset_us: MediaTimeOffsetMicros;
    }
  | {
      readonly type: "source_buffer_removed";
      readonly generation: StreamGeneration;
      readonly source_buffer: SourceBufferIndex;
      readonly start_us: MediaTimeMicros;
      readonly end: RemovalEnd;
    }
  | {
      readonly type: "source_buffer_aborted";
      readonly generation: StreamGeneration;
      readonly source_buffer: SourceBufferIndex;
    }
  | { readonly type: "seek_started"; readonly target_us: MediaTimeMicros; readonly cause: SeekCause }
  | { readonly type: "duration_changed"; readonly generation: StreamGeneration; readonly duration: MediaDuration }
  | { readonly type: "end_of_stream"; readonly generation: StreamGeneration }
  | { readonly type: "page_playback"; readonly request: PagePlaybackRequest }
  | { readonly type: "variant_menu_changed"; readonly menu: VariantMenu }
  | { readonly type: "range_fetch_finished"; readonly request_id: number; readonly outcome: RangeFetchOutcome }
  | { readonly type: "tab_gone"; readonly reason: TabGoneReason };

const removalEndCodec: Codec<RemovalEnd> = taggedUnion<RemovalEnd, "kind">("kind", {
  at: { time_us: mediaTimeCodec },
  end_of_timeline: {},
});

const seekCauseCodec: Codec<SeekCause> = taggedUnion<SeekCause, "kind">("kind", {
  player_seek: { seek_id: seekIdCodec },
  page_seek: {},
});

const pagePlaybackRequestCodec: Codec<PagePlaybackRequest> = taggedUnion<PagePlaybackRequest, "kind">("kind", {
  play: {},
  pause: {},
  set_rate: { rate: playbackRateCodec },
});

const resourceSizeCodec: Codec<ResourceSize> = taggedUnion<ResourceSize, "kind">("kind", {
  known: { size_bytes: byteOffsetCodec },
  unknown: {},
});

const rangeFetchOutcomeCodec: Codec<RangeFetchOutcome> = taggedUnion<RangeFetchOutcome, "kind">("kind", {
  completed: { resource_size: resourceSizeCodec },
  failed: {
    failure: stringEnum<RangeFetchFailure>(["network_error", "http_error", "resource_changed", "out_of_range"]),
  },
});

/** Кодек сообщений сеанса от страницы. */
export const pageSessionCodec: TaggedUnionCodec<PageSessionMessage> = taggedUnion<PageSessionMessage, "type">("type", {
  attach: { session_key: sessionKeyCodec, token: sessionTokenCodec },
  generation_started: { generation: streamGenerationCodec },
  file_source_added: { generation: streamGenerationCodec },
  source_buffer_added: {
    generation: streamGenerationCodec,
    source_buffer: sourceBufferIndexCodec,
    mime_type: mimeTypeCodec,
  },
  source_buffer_type_changed: {
    generation: streamGenerationCodec,
    source_buffer: sourceBufferIndexCodec,
    mime_type: mimeTypeCodec,
  },
  timestamp_offset_changed: {
    generation: streamGenerationCodec,
    source_buffer: sourceBufferIndexCodec,
    offset_us: mediaTimeOffsetCodec,
  },
  source_buffer_removed: {
    generation: streamGenerationCodec,
    source_buffer: sourceBufferIndexCodec,
    start_us: mediaTimeCodec,
    end: removalEndCodec,
  },
  source_buffer_aborted: { generation: streamGenerationCodec, source_buffer: sourceBufferIndexCodec },
  seek_started: { target_us: mediaTimeCodec, cause: seekCauseCodec },
  duration_changed: { generation: streamGenerationCodec, duration: mediaDurationCodec },
  end_of_stream: { generation: streamGenerationCodec },
  page_playback: { request: pagePlaybackRequestCodec },
  variant_menu_changed: { menu: variantMenuCodec },
  range_fetch_finished: { request_id: rangeRequestIdCodec, outcome: rangeFetchOutcomeCodec },
  tab_gone: { reason: stringEnum<TabGoneReason>(["closed", "navigated_away", "browser_closing"]) },
});

/** Проверки между полями: удаляемый промежуток непуст, меню корректно. */
export function validatePageSession(message: PageSessionMessage): void {
  if (message.type === "source_buffer_removed" && message.end.kind === "at" && message.end.time_us <= message.start_us) {
    throw new ProtocolFailure("empty_range").prependPath("end");
  }
  if (message.type === "variant_menu_changed") {
    validateVariantMenu(message.menu);
  }
}

