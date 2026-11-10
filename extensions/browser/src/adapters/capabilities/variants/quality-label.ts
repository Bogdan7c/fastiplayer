// Подпись качества для меню и памяти выбора (сессия 15a): ступень разрешения без частоты кадров и приписок.
//
// Сайты подписывают одно и то же качество по-разному от ролика к ролику: YouTube — «720p» и «720p60»,
// «1080p Premium»; Twitch — «720p60» и «480p». Память выбора сравнивает пункты по ПОДПИСИ (14b2), а на
// YouTube и Twitch она одна на весь сайт (решение 289): выбор «720p60» на одном ролике не нашёл бы «720p»
// на другом. Поэтому адаптеры этих сайтов подписывают пункт ступенью — «720p»; частоту кадров и так видно
// на экране, а наша кнопка качества подписывает картинку ступенью (решение 277).

import type { PageVariantOption } from "./contract.ts";

/** Ведущая ступень вида «720p» в начале подписи сайта. */
const RESOLUTION_STEP = /^(\d{2,5}p)\d{0,3}\b/u;

/**
 * Ступень разрешения из подписи сайта: «720p60» → «720p», «1080p Premium» → «1080p», «2160p60 HDR» → «2160p».
 * Подпись без ступени в начале («Auto», «Source») возвращается как есть.
 */
export function resolutionStepLabel(siteLabel: string): string {
  const trimmed = siteLabel.trim();
  const match = RESOLUTION_STEP.exec(trimmed);
  return match?.[1] ?? trimmed;
}

/**
 * Подписи пунктов одного меню ступенями, без двойников: если два пункта получили одну ступень (у канала Twitch
 * есть и «720p60», и «720p30»), этим двоим остаются подписи сайта — иначе в меню были бы две одинаковые строки,
 * а память выбора всегда находила бы первую. Порядок и число подписей — как на входе.
 */
export function distinctResolutionStepLabels(siteLabels: readonly string[]): readonly string[] {
  const steps = siteLabels.map(resolutionStepLabel);
  const stepCounts = new Map<string, number>();
  for (const step of steps) stepCounts.set(step, (stepCounts.get(step) ?? 0) + 1);
  return steps.map((step, index) => ((stepCounts.get(step) ?? 0) > 1 ? (siteLabels[index] ?? step).trim() : step));
}

/**
 * Меню качеств с подписями-ступенями (без двойников). Автоматический пункт («Auto») не трогаем: его подпись
 * не ступень, и в память выбора он не пишется (решение 284).
 */
export function withResolutionStepLabels(options: readonly PageVariantOption[]): readonly PageVariantOption[] {
  const manual = options.filter((option) => option.automatic !== true);
  const stepLabels = distinctResolutionStepLabels(manual.map((option) => option.label));
  const stepLabelOf = new Map(manual.map((option, index) => [option, stepLabels[index] ?? option.label]));
  return options.map((option) => {
    const label = stepLabelOf.get(option);
    return label === undefined ? option : { ...option, label };
  });
}
