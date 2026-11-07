// Решение о видео, которое задержал сторож (сессия 09c2): переезжает ли на него сеанс.
//
// Второго правила о том, что такое фильм, реклама и мусор, здесь нет: вердикт даёт тот же `classifyVideo`
// через `classifySurvey`, что и при первой передаче. Здесь только то, что отличает переезд от передачи:
// - видео должно быть видно. Скрытый зеркальный плеер (решение 68) играет сам, сеанс на него не уходит;
// - запись нативного HLS (10a2) переезжает как файл, но только к плееру, который знает протокол 1.1; старому
//   плееру сеанс её не покажет, и видео остаётся у страницы;
// - спросить человека посреди ролика некому, поэтому адрес, требующий согласия, считается отказом.

import type { HeldVideoReport } from "../../shared/handoff-contract.ts";
import { decideFileAddress, NO_LOCAL_ADDRESS_CONSENT, type FileAddressRule, type LocalAddressConsent } from "./file-address-guard.ts";
import { chosenVideoOf, classifySurvey, type ChosenVideo } from "./video-choice.ts";

/** Итог решения. */
export type HeldVideoDecision =
  /** Сеанс переезжает: видео и правило адреса для нового документа. */
  | { readonly kind: "adopt"; readonly video: ChosenVideo; readonly fileAddressRule: FileAddressRule }
  | { readonly kind: "release" };

const RELEASE: HeldVideoDecision = { kind: "release" };

/** Что умеет плеер этого сеанса (по договорённой версии протокола): от этого зависит, какие видео ему можно отдать. */
export interface PlayerAbilities {
  /** Плеер понимает `hls_source_added` и `fetch_resource` (протокол 1.1). */
  readonly nativeHls: boolean;
}

/** Согласие человека, данное при первой передаче, действует только на хост, про который спрашивали. */
function consentOf(rule: FileAddressRule): LocalAddressConsent {
  return rule.consentedHost === null ? NO_LOCAL_ADDRESS_CONSENT : { kind: "given_for", host: rule.consentedHost };
}

/**
 * @param frameId фрейм-отправитель: браузер назвал его сам.
 * @param rule правило адреса сеанса: настройка и согласие те же, хост страницы у нового фрейма свой.
 * @param player возможности плеера сеанса.
 */
export function judgeHeldVideo(frameId: number, report: HeldVideoReport, rule: FileAddressRule, player: PlayerAbilities): HeldVideoDecision {
  const entry = classifySurvey([{ frameId, candidates: report.candidates }]).find(({ candidate }) => candidate.element === report.element);
  if (entry === undefined) return RELEASE;
  const { verdict, candidate } = entry;
  if ((verdict !== "main" && verdict !== "ad") || candidate.visible_area_px === 0) return RELEASE;
  const kind = candidate.media.kind;
  if (kind !== "media_source" && kind !== "file" && kind !== "hls") return RELEASE;
  if (kind === "hls" && !player.nativeHls) return RELEASE;
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
