// Участок «вход»: экстеншен <-> посредник <-> плеер (решение 85). Зеркало control.rs.
//
// Посредник native messaging - «звонок в дверь». По этому участку идут только
// короткие разговоры в начале:
// 1. `hello` - версии и возможности сторон;
// 2. `open_session` - открыть сеанс для видео во вкладке;
// 3. ответ плеера: порт и одноразовый пропуск WebSocket (`session_granted`) или
//    отказ с причиной (`refused`).
// Всё живое (байты, часы, команды) идёт дальше напрямую по WebSocket (см. session/).

import { MAX_CAPABILITY_TOKENS } from "./limits.ts";
import { ProtocolFailure, MESSAGE_ERROR_KINDS, type MessageErrorKind } from "./errors.ts";
import { safeInteger } from "./numbers.ts";
import { type Codec, listOf, stringEnum, struct, taggedUnion, type TaggedUnionCodec } from "./schema.ts";
import { controlRequestIdCodec } from "./stream.ts";
import {
  capabilityTokenCodec,
  pageTitleCodec,
  sessionKeyCodec,
  sessionTokenCodec,
  siteDomainCodec,
  softwareVersionCodec,
} from "./text.ts";
import { type MediaDuration, type MediaTimeMicros, mediaDurationCodec, mediaTimeCodec } from "./time.ts";
import { type BridgeSoftware, type ProtocolVersion, bridgeSoftwareCodec, protocolVersionCodec } from "./version.ts";

/** Приветствие стороны. Форма не меняется никогда. */
export interface HelloInfo {
  /** Версия протокола, которую сторона реализует. */
  readonly protocol: ProtocolVersion;
  /** Версия программы - только для диагностики. */
  readonly software_version: string;
  /** Метки возможностей (см. capability.ts). */
  readonly capabilities: readonly string[];
}

/** Что видно о странице: домен и заголовок (без адреса и кук - инвариант 2). */
export interface PageIdentity {
  readonly title: string;
  readonly domain: string;
}

/** Как человек хочет смотреть. */
export type SessionIntent = "window" | "picture_in_picture" | "in_page";

/** Где сейчас позиция плеера страницы. */
export type PositionHint =
  | { readonly kind: "unknown" }
  | { readonly kind: "at"; readonly position_us: MediaTimeMicros };

/** Подсказки для окна до прихода первых байтов. */
export interface PlaybackHints {
  readonly duration: MediaDuration;
  readonly position: PositionHint;
}

/** Чем стал запрос `open_session`. */
export type GrantOutcome = "started" | "reused_existing" | "replaced_previous";

/** На что отказ. */
export type ReplyTarget =
  | { readonly kind: "hello" }
  | { readonly kind: "open_session"; readonly request_id: number }
  | { readonly kind: "unparsed_message" };

/** Причина отказа на участке «вход». */
export type RefusalReason =
  | { readonly kind: "player_unavailable" }
  | { readonly kind: "player_not_responding" }
  | { readonly kind: "player_shutting_down" }
  | { readonly kind: "protected_content" }
  | { readonly kind: "incompatible_version"; readonly update: BridgeSoftware }
  | { readonly kind: "unsupported_codec" }
  | { readonly kind: "malformed_message"; readonly error: MessageErrorKind }
  | { readonly kind: "hello_required" };

/** Приветствие экстеншена. */
export interface ExtensionHello extends HelloInfo {
  readonly type: "hello";
}

/** Запрос «открой сеанс для этого видео». */
export interface ExtensionOpenSession {
  readonly type: "open_session";
  readonly request_id: number;
  /** Ключ сеанса: повтор с тем же ключом - тот же сеанс. */
  readonly session_key: string;
  readonly page: PageIdentity;
  readonly intent: SessionIntent;
  readonly hints: PlaybackHints;
}

/** Сообщения участка «вход» от экстеншена к плееру. */
export type ExtensionControlMessage = ExtensionHello | ExtensionOpenSession;

/** Ответ плеера на `hello`. */
export interface PlayerHelloAccepted {
  readonly type: "hello_accepted";
  /** Общая версия: на ней обе стороны говорят дальше. */
  readonly agreed_protocol: ProtocolVersion;
  /** Приветствие плеера: его версия и возможности. */
  readonly player: HelloInfo;
}

/** Пропуск в сеанс. */
export interface PlayerSessionGranted {
  readonly type: "session_granted";
  readonly request_id: number;
  readonly session_key: string;
  /** Порт WebSocket на 127.0.0.1 (адреса в протоколе нет). */
  readonly port: number;
  /** Одноразовый пропуск: первым сообщением сеанса. */
  readonly token: string;
  readonly outcome: GrantOutcome;
}

/** Отказ. Его же шлёт посредник, если плеер недоступен. */
export interface PlayerRefused {
  readonly type: "refused";
  readonly in_reply_to: ReplyTarget;
  readonly reason: RefusalReason;
}

/** Сообщения участка «вход» от плеера (или посредника) к экстеншену. */
export type PlayerControlMessage = PlayerHelloAccepted | PlayerSessionGranted | PlayerRefused;

// ---------------------------------------------------------------- кодеки

const protocolVersionField = protocolVersionCodec;

const helloInfoCodec: Codec<HelloInfo> = struct<HelloInfo>({
  protocol: protocolVersionField,
  software_version: softwareVersionCodec,
  capabilities: listOf(capabilityTokenCodec),
});

const pageIdentityCodec: Codec<PageIdentity> = struct<PageIdentity>({
  title: pageTitleCodec,
  domain: siteDomainCodec,
});

const sessionIntentCodec: Codec<SessionIntent> = stringEnum<SessionIntent>(["window", "picture_in_picture", "in_page"]);

const positionHintCodec: Codec<PositionHint> = taggedUnion<PositionHint, "kind">("kind", {
  unknown: {},
  at: { position_us: mediaTimeCodec },
});

const playbackHintsCodec: Codec<PlaybackHints> = struct<PlaybackHints>({
  duration: mediaDurationCodec,
  position: positionHintCodec,
});

const grantOutcomeCodec: Codec<GrantOutcome> = stringEnum<GrantOutcome>(["started", "reused_existing", "replaced_previous"]);

const loopbackPortCodec: Codec<number> = safeInteger({ primitive: "u16", min: 1 });

const replyTargetCodec: Codec<ReplyTarget> = taggedUnion<ReplyTarget, "kind">("kind", {
  hello: {},
  open_session: { request_id: controlRequestIdCodec },
  unparsed_message: {},
});

const refusalReasonCodec: Codec<RefusalReason> = taggedUnion<RefusalReason, "kind">("kind", {
  player_unavailable: {},
  player_not_responding: {},
  player_shutting_down: {},
  protected_content: {},
  incompatible_version: { update: bridgeSoftwareCodec },
  unsupported_codec: {},
  malformed_message: { error: stringEnum<MessageErrorKind>(MESSAGE_ERROR_KINDS) },
  hello_required: {},
});

/** Кодек сообщений экстеншена на участке «вход». */
export const extensionControlCodec: TaggedUnionCodec<ExtensionControlMessage> = taggedUnion<ExtensionControlMessage, "type">("type", {
  hello: {
    protocol: protocolVersionField,
    software_version: softwareVersionCodec,
    capabilities: listOf(capabilityTokenCodec),
  },
  open_session: {
    request_id: controlRequestIdCodec,
    session_key: sessionKeyCodec,
    page: pageIdentityCodec,
    intent: sessionIntentCodec,
    hints: playbackHintsCodec,
  },
});

/** Кодек сообщений плеера (и посредника) на участке «вход». */
export const playerControlCodec: TaggedUnionCodec<PlayerControlMessage> = taggedUnion<PlayerControlMessage, "type">("type", {
  hello_accepted: { agreed_protocol: protocolVersionField, player: helloInfoCodec },
  session_granted: {
    request_id: controlRequestIdCodec,
    session_key: sessionKeyCodec,
    port: loopbackPortCodec,
    token: sessionTokenCodec,
    outcome: grantOutcomeCodec,
  },
  refused: {
    in_reply_to: replyTargetCodec,
    reason: refusalReasonCodec,
  },
});

// ---------------------------------------------------------------- проверки между полями

/** Список меток не длиннее лимита и без повторов. */
function validateCapabilityList(capabilities: readonly string[]): void {
  if (capabilities.length > MAX_CAPABILITY_TOKENS) {
    throw new ProtocolFailure("too_many_items").prependPath("capabilities");
  }
  if (new Set(capabilities).size !== capabilities.length) {
    throw new ProtocolFailure("duplicate_id").prependPath("capabilities");
  }
}

/** Проверки `hello` между полями (после успешного разбора всех полей). */
export function validateExtensionControl(message: ExtensionControlMessage): void {
  if (message.type === "hello") {
    validateCapabilityList(message.capabilities);
  }
}

/** Проверки сообщения плеера между полями. */
export function validatePlayerControl(message: PlayerControlMessage): void {
  if (message.type === "hello_accepted") {
    validateCapabilityList(message.player.capabilities);
  }
}

