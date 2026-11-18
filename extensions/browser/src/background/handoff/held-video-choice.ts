// Решение о видео, которое задержал сторож (сессия 09c2): переезжает ли на него сеанс.
//
// Второго правила о том, что такое фильм, реклама и мусор, здесь нет: вердикт даёт тот же `classifyVideo`
// через `classifySurvey`, что и при первой передаче. Здесь только то, что отличает переезд от передачи:
// - видео должно быть видно. Скрытый зеркальный плеер (решение 68) играет сам, сеанс на него не уходит;
// - нативный HLS переезжает как файл, но только к плееру, который его знает (hls-player-support.ts): запись —
//   с протокола 1.1 (10a2), эфир — с 1.2 (10a3); старому плееру сеанс его не покажет, и видео остаётся у страницы;
// - спросить человека посреди ролика некому, поэтому адрес, требующий согласия, считается отказом;
// - на ленте (16b2, решение 357) годится и тихий ролик на месте плеера: следующие ролики Instagram без жеста
//   тоже без звука. Вне ленты — нет (беззвучный баннер после рекламы, страница `ad-then-banner`).

import type { HeldVideoReport } from "../../shared/handoff-contract.ts";
import { decideFileAddress, NO_LOCAL_ADDRESS_CONSENT, type FileAddressRule, type LocalAddressConsent } from "./file-address-guard.ts";
import { playerCanPlayHls, type PlayerAbilities } from "./hls-player-support.ts";
import { chosenVideoOf, classifySurvey, isSilentClipInPlayerPlace, type ChosenVideo } from "./video-choice.ts";
import type { SilentClipAdoption } from "./video-relocation.ts";
import { isLiveHlsCandidate } from "../tab-model/video-verdict.ts";

/** Итог решения. */
export type HeldVideoDecision =
  /** Сеанс переезжает: видео и правило адреса для нового документа. */
  | { readonly kind: "adopt"; readonly video: ChosenVideo; readonly fileAddressRule: FileAddressRule }
  | { readonly kind: "release" };

const RELEASE: HeldVideoDecision = { kind: "release" };

/**
 * Согласие человека, данное при первой передаче, действует только на хост, про который спрашивали. Им же
 * открывается сеанс-замена после перезагрузки страницы (16c): согласие не должно ни пропасть, ни расшириться.
 */
export function consentOf(rule: FileAddressRule): LocalAddressConsent {
  return rule.consentedHost === null ? NO_LOCAL_ADDRESS_CONSENT : { kind: "given_for", host: rule.consentedHost };
}

/**
 * @param frameId фрейм-отправитель: браузер назвал его сам.
 * @param rule правило адреса сеанса: настройка и согласие те же, хост страницы у нового фрейма свой.
 * @param player возможности плеера сеанса.
 * @param silentClips брать ли тихий ролик ленты (так решает переезд: только когда лента листается).
 */
export function judgeHeldVideo(
  frameId: number,
  report: HeldVideoReport,
  rule: FileAddressRule,
  player: PlayerAbilities,
  silentClips: SilentClipAdoption,
): HeldVideoDecision {
  const entry = classifySurvey([{ frameId, candidates: report.candidates }]).find(({ candidate }) => candidate.element === report.element);
  if (entry === undefined) return RELEASE;
  const { verdict, candidate } = entry;
  const silentClipWelcome = silentClips === "accepted" && isSilentClipInPlayerPlace(entry);
  if ((verdict !== "main" && verdict !== "ad" && !silentClipWelcome) || candidate.visible_area_px === 0) return RELEASE;
  const kind = candidate.media.kind;
  if (kind !== "media_source" && kind !== "file" && kind !== "hls") return RELEASE;
  if (kind === "hls" && !playerCanPlayHls(player, isLiveHlsCandidate(candidate) ? "live" : "vod")) return RELEASE;
  const video = chosenVideoOf(entry);
  // Согласие человека дано странице, а не вкладке: чужой фрейм им не пользуется.
  const fileAddressRule: FileAddressRule = {
    ...rule,
    pageHost: video.frameHost,
    consentedHost: video.frameHost === rule.pageHost ? rule.consentedHost : null,
  };
  if (video.fileHost === null) return { kind: "adopt", video, fileAddressRule };
  const decision = decideFileAddress(video.frameHost, video.fileHost, rule.setting, consentOf(fileAddressRule));
  return decision.kind === "allowed" ? { kind: "adopt", video, fileAddressRule } : RELEASE;
}
