// Решение режима 2 «автозапуск» (сессия 12a) по видео, которое человек сам запустил со звуком.
// Чистая функция: на входе политика сайта, задержанное видео (уже классифицированное тем же
// `classifySurvey`, что и при передаче вкладки), фильтры видео (порог длины, звук) и итог пробы рекламы;
// на выходе — что делать.
// Каждое правило — отдельная маленькая функция в списке `RULES`: порядок списка — порядок проверок.
//
// Правила по порядку (первое сработавшее решает):
// 1. политика сайта пропустила                         -> release;
// 2. видео нет в донесении                             -> release;
// 3. вердикт не фильм и не реклама                     -> release (DRM, неподдержанная форма, пусто, мусор);
// 4. без звука                                         -> release (страница — враг: «человек запустил со звуком» мы
//                                                         проверяем и здесь, а не верим сторожу на слово);
//                                                         фильтр звука выключен (решение 260) — правило молчит;
// 5. не видно (площадь 0)                              -> release;
// 6. длительность неизвестна (эфир, MediaSource)       -> hand_off сразу (решение 50);
// 7. фильтр длины выключен (решение 260)               -> hand_off (короткий ролик тоже; реклама — тоже, её
//                                                         увезёт переезд сеанса 09c2, как в правиле 8);
// 7a. не короче порога                                 -> hand_off;
// 8. короче порога и вердикт `ad`                      -> hand_off (реклама играет в нашем плеере, переезд сеанса 09c2
//                                                         сам перенесёт сеанс на фильм);
// 9. короче порога, рекламу ещё не проверяли           -> probe_ads_first;
// 10. короче порога, читатели органов видят рекламу    -> hand_off;
// 11. короче порога, рекламы нет                       -> release_with_credit: ролик отпускаем, а фильм, который сайт
//                                                         запустит сам после него, засчитается запуском человека.
//
// Политика сайта `ask` (сессия 12b, решения 255–257) правил видео не отменяет: сначала те же правила, и только
// там, где видео ушло бы в плеер (`hand_off`), решение становится `ask_viewer` — видео замирает под тенью, а
// человек выбирает на предложении поверх него. Отпускаемое (без звука, короткий ролик) не спрашивается вовсе.

import type { ClassifiedVideo } from "../handoff/video-choice.ts";
import { MAX_PLAY_CREDIT_LEASE_MS } from "../../shared/auto-handoff-contract.ts";
import type { AutoHandoffSettings } from "../../shared/auto-handoff-settings.ts";
import type { ElementKey, VideoCandidate } from "../../shared/handoff-contract.ts";
import type { SitePolicy } from "./site-policy.ts";

/**
 * Запас к сроку кредита сверх длины ролика, мс: сайту нужно время, чтобы после рекламы поднять фильм
 * (загрузка плеера, обмен с рекламной сетью). Десять секунд владелец одобрил как разумный запас.
 */
export const PLAY_CREDIT_GRACE_MS = 10_000;

/** Фильтры видео, которые человек может выключить в настройках (решение 260). */
export interface VideoFilters {
  /** Порог длины, секунды; действует, только пока `requireMinDuration`. */
  readonly minDurationS: number;
  /** Фильтр длины включён: ролик короче порога не уходит сразу, а идёт веткой рекламы. */
  readonly requireMinDuration: boolean;
  /** Фильтр звука включён: видео без звука отпускается. */
  readonly requireSound: boolean;
}

/** Фильтры видео из общей настройки режима 2. */
export function videoFiltersOf(settings: AutoHandoffSettings): VideoFilters {
  return { minDurationS: settings.min_duration_s, requireMinDuration: settings.require_min_duration, requireSound: settings.require_sound };
}

/**
 * Кандидаты донесения такими, какими их должен увидеть общий классификатор при этих фильтрах.
 *
 * Общий классификатор (`classifyVideo`) сам считает беззвучное видео мусором, если оно зацикленное, мелкое
 * или короче 30 с («беззвучное украшение»), — это правило нужно кнопке и передаче вкладки, его не трогаем.
 * Но в режиме 2 задержанное видео запустил сам человек: при снятом фильтре звука (решение 260) беззвучие
 * не должно его отсеивать — ни здесь, ни в `mutedRule`. Поэтому ровно задержанный кандидат судится как
 * звучащий; соседи остаются как есть (они влияют на вердикт, например, преролла рядом с фильмом).
 * Фильтр звука включён — кандидаты возвращаются без изменений.
 */
export function candidatesAsFiltersSee(candidates: readonly VideoCandidate[], heldElement: ElementKey, filters: VideoFilters): readonly VideoCandidate[] {
  if (filters.requireSound) return candidates;
  return candidates.map((candidate) => (candidate.element === heldElement && candidate.muted ? { ...candidate, muted: false } : candidate));
}

/** Итог пробы рекламы на вкладке; `not_probed` — пробу ещё не запускали (первый заход решения). */
export type AdProbeVerdict = "ad_on_screen" | "no_ad" | "not_probed";

export interface ViewerPlayInput {
  readonly policy: SitePolicy;
  /** Задержанное видео после классификации; `undefined` — его нет среди кандидатов донесения. */
  readonly held: ClassifiedVideo | undefined;
  /** Фильтры видео из настройки: порог длины и требование звука (оба выключаются, решение 260). */
  readonly filters: VideoFilters;
  readonly adProbe: AdProbeVerdict;
}

/** Почему видео отпущено без передачи. */
export type ReleaseReason = "site_skipped" | "no_held_video" | "not_a_film" | "muted" | "hidden";

/** Почему видео передаётся. */
export type HandOffReason = "duration_unknown" | "length_filter_off" | "long_enough" | "known_ad" | "ad_on_screen";

export type ViewerPlayDecision =
  | { readonly kind: "hand_off"; readonly reason: HandOffReason }
  /** Видео ушло бы в плеер, но сайт помечен `ask`: держать видео и спросить человека. */
  | { readonly kind: "ask_viewer"; readonly reason: HandOffReason }
  /** Ролик короче порога и не опознан как реклама: сначала проба органов рекламы на вкладке. */
  | { readonly kind: "probe_ads_first" }
  /** Ролик отпускается, а вкладка получает «кредит клика» на `lease_ms` для фильма, который сайт запустит сам. */
  | { readonly kind: "release_with_credit"; readonly lease_ms: number }
  | { readonly kind: "release"; readonly reason: ReleaseReason };

const release = (reason: ReleaseReason): ViewerPlayDecision => ({ kind: "release", reason });
const handOff = (reason: HandOffReason): ViewerPlayDecision => ({ kind: "hand_off", reason });

/** Правило: решение или `undefined` («не моё дело, дальше»). */
type Rule = (input: ViewerPlayInput, held: ClassifiedVideo) => ViewerPlayDecision | undefined;

/** Срок кредита: длина ролика плюс запас, не больше предела договора. */
export function playCreditLeaseMs(clipDurationS: number): number {
  return Math.min(MAX_PLAY_CREDIT_LEASE_MS, Math.ceil(clipDurationS * 1000) + PLAY_CREDIT_GRACE_MS);
}

const verdictRule: Rule = (_input, held) => (held.verdict === "main" || held.verdict === "ad" ? undefined : release("not_a_film"));
const mutedRule: Rule = (input, held) => (input.filters.requireSound && held.candidate.muted ? release("muted") : undefined);
const hiddenRule: Rule = (_input, held) => (held.candidate.visible_area_px === 0 ? release("hidden") : undefined);

/** Правила длины: неизвестная и достаточная длина передаются, короткий ролик идёт по ветке рекламы. */
const durationRule: Rule = (input, held) => {
  const durationS = held.candidate.duration_s;
  if (durationS === null) return handOff("duration_unknown");
  if (!input.filters.requireMinDuration) return handOff("length_filter_off");
  if (durationS >= input.filters.minDurationS) return handOff("long_enough");
  if (held.verdict === "ad") return handOff("known_ad");
  switch (input.adProbe) {
    case "not_probed":
      return { kind: "probe_ads_first" };
    case "ad_on_screen":
      return handOff("ad_on_screen");
    case "no_ad":
      return { kind: "release_with_credit", lease_ms: playCreditLeaseMs(durationS) };
  }
};

/** Правила, зависящие от видео, в порядке проверки (политика сайта и отсутствие видео — выше, в `decideViewerPlay`). */
const VIDEO_RULES: readonly Rule[] = [verdictRule, mutedRule, hiddenRule, durationRule];

export function decideViewerPlay(input: ViewerPlayInput): ViewerPlayDecision {
  if (input.policy.kind === "skip") return release("site_skipped");
  if (input.held === undefined) return release("no_held_video");
  const decision = decideByVideoRules(input, input.held);
  return input.policy.kind === "ask" ? askInsteadOfHandOff(decision) : decision;
}

/** Правила видео по порядку; последнее отвечает всегда. */
function decideByVideoRules(input: ViewerPlayInput, held: ClassifiedVideo): ViewerPlayDecision {
  for (const rule of VIDEO_RULES) {
    const decision = rule(input, held);
    if (decision !== undefined) return decision;
  }
  // Последнее правило отвечает всегда; сюда не дойти.
  return release("not_a_film");
}

/** Сайт `ask`: «передать» становится «спросить», остальное (проба рекламы, отпускание) остаётся как есть. */
function askInsteadOfHandOff(decision: ViewerPlayDecision): ViewerPlayDecision {
  return decision.kind === "hand_off" ? { kind: "ask_viewer", reason: decision.reason } : decision;
}
