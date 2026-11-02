// Публичный вход протокола моста на TypeScript (зеркало crates/browser-bridge-protocol).
//
// Остальные части экстеншена импортируют протокол только отсюда.

import { type DecodeResult, type EncodeResult } from "./errors.ts";
import {
  type ExtensionControlMessage,
  type PlayerControlMessage,
  extensionControlCodec,
  playerControlCodec,
  validateExtensionControl,
  validatePlayerControl,
} from "./control.ts";
import { type MessageInput, type MessageKind, decodeMessage, encodeMessage } from "./message-codec.ts";
import { type PageSessionMessage, pageSessionCodec, validatePageSession } from "./session/page.ts";
import { type PlayerSessionMessage, playerSessionCodec } from "./session/player.ts";

const EXTENSION_CONTROL: MessageKind<ExtensionControlMessage> = {
  codec: extensionControlCodec,
  validate: validateExtensionControl,
};
const PLAYER_CONTROL: MessageKind<PlayerControlMessage> = { codec: playerControlCodec, validate: validatePlayerControl };
const PAGE_SESSION: MessageKind<PageSessionMessage> = { codec: pageSessionCodec, validate: validatePageSession };
// Все правила сообщений плеера выражены типами полей, проверок между полями нет.
const PLAYER_SESSION: MessageKind<PlayerSessionMessage> = {
  codec: playerSessionCodec,
  validate: () => undefined,
};

/** Разбирает сообщение экстеншена на участке «вход» (экстеншен -> посредник -> плеер). */
export function decodeExtensionControl(input: MessageInput): DecodeResult<ExtensionControlMessage> {
  return decodeMessage(EXTENSION_CONTROL, input);
}

/** Записывает сообщение экстеншена на участке «вход» (компактный JSON, поля в порядке схемы). */
export function encodeExtensionControl(message: ExtensionControlMessage): EncodeResult<string> {
  return encodeMessage(EXTENSION_CONTROL, message);
}

/** Разбирает сообщение плеера (или посредника) на участке «вход». */
export function decodePlayerControl(input: MessageInput): DecodeResult<PlayerControlMessage> {
  return decodeMessage(PLAYER_CONTROL, input);
}

/** Записывает сообщение плеера на участке «вход». */
export function encodePlayerControl(message: PlayerControlMessage): EncodeResult<string> {
  return encodeMessage(PLAYER_CONTROL, message);
}

/** Разбирает текстовую рамку сеанса от страницы (экстеншен -> плеер). */
export function decodePageSession(input: MessageInput): DecodeResult<PageSessionMessage> {
  return decodeMessage(PAGE_SESSION, input);
}

/** Записывает текстовую рамку сеанса от страницы. */
export function encodePageSession(message: PageSessionMessage): EncodeResult<string> {
  return encodeMessage(PAGE_SESSION, message);
}

/** Разбирает текстовую рамку сеанса от плеера (плеер -> экстеншен). */
export function decodePlayerSession(input: MessageInput): DecodeResult<PlayerSessionMessage> {
  return decodeMessage(PLAYER_SESSION, input);
}

/** Записывает текстовую рамку сеанса от плеера. */
export function encodePlayerSession(message: PlayerSessionMessage): EncodeResult<string> {
  return encodeMessage(PLAYER_SESSION, message);
}

export { decodeDataFrame, encodeDataFrame } from "./data-frame.ts";
export type { DataFrame, DataFrameHeader, RangeBytesHeader, SourceBufferBytesHeader } from "./data-frame.ts";

export { DATA_FRAME_ERROR_KINDS, MESSAGE_ERROR_KINDS } from "./errors.ts";
export type {
  DataFrameErrorKind,
  DecodeResult,
  EncodeResult,
  MessageErrorKind,
  ProtocolError,
} from "./errors.ts";

export type { MessageInput } from "./message-codec.ts";

export type {
  ExtensionControlMessage,
  ExtensionHello,
  ExtensionOpenSession,
  GrantOutcome,
  HelloInfo,
  PageIdentity,
  PlaybackHints,
  PlayerControlMessage,
  PlayerHelloAccepted,
  PlayerRefused,
  PlayerSessionGranted,
  PositionHint,
  RefusalReason,
  ReplyTarget,
  SessionIntent,
} from "./control.ts";

export type {
  PagePlaybackRequest,
  PageSessionMessage,
  RangeFetchFailure,
  RangeFetchOutcome,
  RemovalEnd,
  ResourceSize,
  SeekCause,
  TabGoneReason,
} from "./session/page.ts";
export type {
  BufferIntake,
  ClockState,
  PlayerSessionMessage,
  ResumeState,
  StreamRejection,
} from "./session/player.ts";
export type { VariantKind, VariantMenu, VariantOption, VariantSelection } from "./session/variants.ts";
export { SESSION_CLOSE_CODES, closeCodeOf, closeReasonFromCode } from "./session/close-reason.ts";
export type { SessionCloseReason } from "./session/close-reason.ts";

export type { MediaDuration, ByteCount, MediaTimeMicros, MediaTimeOffsetMicros, PlaybackRatePermille } from "./time.ts";
export { NORMAL_PLAYBACK_RATE_PERMILLE } from "./time.ts";
export type { SourceBufferIndex, StreamGeneration } from "./stream.ts";

export { CURRENT_PROTOCOL_VERSION, negotiateProtocolVersion } from "./version.ts";
export type { BridgeSoftware, ProtocolVersion, VersionNegotiation } from "./version.ts";

export { capabilitiesToTokens, cheapCodecToken, featureToken, parseCapabilityTokens } from "./capability.ts";
export type { OptionalFeature, ParsedCapabilities, PeerCapabilities, VideoCodecFamily } from "./capability.ts";

export { sanitizePageTitle, splitAppendPayload } from "./sender.ts";
export { utf8ByteLength } from "./text.ts";

export * from "./limits.ts";
