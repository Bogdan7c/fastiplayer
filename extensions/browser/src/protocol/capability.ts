// Возможности сторон, о которых они сообщают в `hello`. Зеркало capability.rs.
//
// Возможности - список меток-строк, а не поля `hello`: форма `hello` не меняется
// никогда. Незнакомая метка - ЕДИНСТВЕННОЕ, что протокол пропускает молча: она
// лишь значит «собеседник умеет то, чем мы пользоваться не умеем».

/** Необязательные функции, о которых стороны договариваются метками `feature.*`. */
export type OptionalFeature = "page_geometry";

/** Семейства видеокодеков, которые плеер декодирует «дёшево» (метки `cheap_codec.*`). */
export type VideoCodecFamily = "h264" | "hevc" | "vp9" | "av1";

const OPTIONAL_FEATURES: readonly OptionalFeature[] = ["page_geometry"];
const VIDEO_CODEC_FAMILIES: readonly VideoCodecFamily[] = ["h264", "hevc", "vp9", "av1"];

/** Метка необязательной функции. */
export function featureToken(feature: OptionalFeature): string {
  return `feature.${feature}`;
}

/** Метка «дешёвого» кодека. */
export function cheapCodecToken(codec: VideoCodecFamily): string {
  return `cheap_codec.${codec}`;
}

/** Что умеет собеседник (только то, что знает эта версия экстеншена). */
export interface PeerCapabilities {
  readonly features: ReadonlySet<OptionalFeature>;
  readonly cheapVideoCodecs: ReadonlySet<VideoCodecFamily>;
}

/** Итог разбора меток: знакомое распознано, незнакомое только посчитано. */
export interface ParsedCapabilities {
  readonly capabilities: PeerCapabilities;
  readonly ignoredUnknownTokens: number;
}

/** Разбирает метки собеседника; незнакомые (в том числе будущие `feature.*`) пропускает со счётом. */
export function parseCapabilityTokens(tokens: readonly string[]): ParsedCapabilities {
  const features = new Set<OptionalFeature>();
  const cheapVideoCodecs = new Set<VideoCodecFamily>();
  let ignoredUnknownTokens = 0;
  for (const token of tokens) {
    const feature = OPTIONAL_FEATURES.find((candidate) => featureToken(candidate) === token);
    const codec = VIDEO_CODEC_FAMILIES.find((candidate) => cheapCodecToken(candidate) === token);
    if (feature !== undefined) {
      features.add(feature);
    } else if (codec !== undefined) {
      cheapVideoCodecs.add(codec);
    } else {
      ignoredUnknownTokens += 1;
    }
  }
  return { capabilities: { features, cheapVideoCodecs }, ignoredUnknownTokens };
}

/** Записывает возможности метками: сначала функции, потом кодеки, в порядке объявления. */
export function capabilitiesToTokens(capabilities: PeerCapabilities): string[] {
  return [
    ...OPTIONAL_FEATURES.filter((feature) => capabilities.features.has(feature)).map(featureToken),
    ...VIDEO_CODEC_FAMILIES.filter((codec) => capabilities.cheapVideoCodecs.has(codec)).map(cheapCodecToken),
  ];
}
