// Действующая политика кодеков из двух входов: настройки владельца и метки плеера (решения 43, 189).
// Чистая функция — вся таблица проверяется тестом.
import { TRANSPARENT_POLICY, refuse, type CodecPolicy, type CodecSetting, type RefusableFamily, type VideoCodecFamily } from "../../shared/codec-policy.ts";

/** Семейства, которые `auto` вправе отвергнуть по метке: H.264 не отвергается никогда, VP8 в метке не бывает. */
const LABEL_REFUSABLE = ["av1", "vp9", "hevc"] as const satisfies readonly (RefusableFamily & VideoCodecFamily)[];

/**
 *  - `off`          — прозрачно: сайты получают родные ответы браузера;
 *  - `always_h264`  — владелец явно хочет только H.264: отказать AV1, VP9, HEVC и VP8;
 *  - `prefer_vp9`   — «VP9, но не AV1»: отказать только AV1;
 *  - `auto`         — следуем плееру: отказать тем из {AV1, VP9, HEVC}, кого НЕТ в метке. Метки нет
 *    (плеер ещё ни разу не отвечал на hello) или она пуста («ограничений нет») — прозрачно. Если в
 *    метке нет H.264, он всё равно остаётся разрешённым: H.264 мы не отвергаем по метке никогда.
 *
 * Метка берётся из КЭШа последнего подключения, а не «живая»: плеер просыпается лишь по делу
 * (решение 162), и спросить его до загрузки страницы нельзя. Первое подключение после установки
 * идёт, пока политика прозрачна; со второго — по метке.
 */
export function computeEffectivePolicy(setting: CodecSetting, cachedCheapCodecs: readonly VideoCodecFamily[]): CodecPolicy {
  switch (setting) {
    case "off":
      return TRANSPARENT_POLICY;
    case "always_h264":
      return refuse(["av1", "vp9", "hevc", "vp8"]);
    case "prefer_vp9":
      return refuse(["av1"]);
    case "auto":
      if (cachedCheapCodecs.length === 0) return TRANSPARENT_POLICY;
      return refuse(LABEL_REFUSABLE.filter((family) => !cachedCheapCodecs.includes(family)));
  }
}
