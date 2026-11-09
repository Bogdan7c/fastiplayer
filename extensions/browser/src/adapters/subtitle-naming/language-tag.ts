// Сырое имя языка из адреса или атрибута страницы приводится к тому, что пропускает пограничник
// (`page-subtitle-validation.ts`): непустая строка не длиннее `MAX_PAGE_SUBTITLE_LANGUAGE_CHARS`.
// Проверять, что это настоящий BCP 47, здесь не нужно: так фон решает сам и при неверном теге говорит
// «язык не назван» (решение 276).

import { MAX_PAGE_SUBTITLE_LANGUAGE_CHARS, MAX_PAGE_SUBTITLE_LABEL_CHARS } from "../../shared/page-subtitle-contract.ts";

/** Язык: без пробелов по краям, 1..64 символа; иначе `null` («язык не назван»). */
export function languageOrNull(raw: string | null): string | null {
  if (raw === null) return null;
  const trimmed = raw.trim();
  return trimmed.length >= 1 && trimmed.length <= MAX_PAGE_SUBTITLE_LANGUAGE_CHARS ? trimmed : null;
}

/** Подпись: без пробелов по краям; слишком длинная обрезается по границе пограничника. */
export function labelOrNull(raw: string | null): string | null {
  if (raw === null) return null;
  const trimmed = raw.trim();
  if (trimmed.length === 0) return null;
  return trimmed.length > MAX_PAGE_SUBTITLE_LABEL_CHARS ? trimmed.slice(0, MAX_PAGE_SUBTITLE_LABEL_CHARS) : trimmed;
}
