// Команда «отдать вкладку N» (решение 175): опрос фреймов → запись в модель вкладки → выбор видео
// (с отсевом рекламы, мусора и DRM) → проверка адреса файла (решение 199) → сеанс. Каждый отказ
// записывается в модель вкладки (09c).
//
// Владеет таблицей «вкладка → её передача». Повторная команда для вкладки, у которой передача
// уже идёт или стартует, получает исход ТОЙ ЖЕ передачи (кнопку попапа можно нажать дважды);
// после конца передачи вкладку можно отдать снова — с новым ключом сеанса.

import type { HandOffTabOutcome, HeldVideoReply, HeldVideoReport } from "../../shared/handoff-contract.ts";
import type { PageAdState } from "../../protocol/index.ts";
import type { TabModel } from "../tab-model/tab-model.ts";
import type { TabRefusal } from "../tab-model/tab-snapshot.ts";
import type { LocalAddressSetting } from "../../shared/local-address-setting.ts";
import { DEFAULT_LOCAL_ADDRESS_SETTING } from "../../shared/local-address-setting.ts";
import { decideFileAddress, NO_LOCAL_ADDRESS_CONSENT, type FileAddressRule, type LocalAddressConsent } from "./file-address-guard.ts";
import { surveyTabVideos, type ExecuteInAllFrames } from "./frame-survey.ts";
import { refusalOf, refusalOfSessionEnd } from "./handoff-refusal.ts";
import { startHandoffSession, type HandoffSession, type HandoffSessionDependencies, type HandoffSessionEnd } from "./handoff-session.ts";
import { pageIdentityOf } from "./page-identity.ts";
import { chooseVideo, type ChosenVideo, type VideoChoice } from "./video-choice.ts";

/** Наибольший номер запроса `begin_handoff` (u32 договора); после него счёт идёт с 1. */
const MAX_BEGIN_REQUEST_ID = 0xffff_ffff;

export interface TabHandoffsDependencies extends HandoffSessionDependencies {
  readonly executeInAllFrames: ExecuteInAllFrames;
  /** Модель вкладки: сюда пишутся итог каждого опроса и каждый отказ. Её сбой передачу не ломает. */
  readonly tabModel: TabModel;
  /** Ключ сеанса для передачи этой вкладки (base64url, ≤ 64 байт). */
  readonly sessionKeyFor: (tabId: number, attempt: number) => string;
  /** Настройка адреса файла (решение 199). Читается на каждую команду: человек мог её сменить. */
  readonly readLocalAddressSetting: () => Promise<LocalAddressSetting>;
  /** Сбой чтения настройки: действует безопасное значение по умолчанию, передача не ломается. */
  readonly reportSettingFailure: (error: unknown) => void;
  /** Конец передачи — для диагностики и тестов. */
  readonly onSessionEnded?: (tabId: number, end: HandoffSessionEnd) => void;
}

export interface TabHandoffs {
  /** `consent` — согласие человека на локальный хост; ставит только страница экстеншена, без него — нет. */
  handOffTab(tabId: number, consent?: LocalAddressConsent): Promise<HandOffTabOutcome>;
  /** `tabs.onRemoved`. */
  tabRemoved(tabId: number): void;
  /** Сторож задержал видео во фрейме вкладки. Нет сеанса — видео отпускается. */
  heldVideo(tabId: number, frameId: number, report: HeldVideoReport): HeldVideoReply;
  /** На экране плеера реклама; сеанса нет — `false` (решение 204, сессия 09d). */
  isAdOnScreen(tabId: number): boolean;
  /** Донесение фрейма о рекламе (09d). Нет сеанса у вкладки — игнорируется: страница вне передачи молчит. */
  adWatchReport(tabId: number, frameId: number, ad: PageAdState): void;
  /** Вкладки, у которых передача стартует или идёт. */
  activeTabIds(): number[];
}

interface TabEntry {
  /** Исход передачи; повторная команда получает его же. */
  outcome: Promise<HandOffTabOutcome>;
  session: HandoffSession | undefined;
  removed: boolean;
}

export function createTabHandoffs(dependencies: TabHandoffsDependencies): TabHandoffs {
  const entries = new Map<number, TabEntry>();
  let nextBeginRequestId = 1;
  let attempt = 0;

  function takeBeginRequestId(): number {
    const requestId = nextBeginRequestId;
    nextBeginRequestId = nextBeginRequestId >= MAX_BEGIN_REQUEST_ID ? 1 : nextBeginRequestId + 1;
    return requestId;
  }

  /** Отказ: записать причину в модель вкладки и вернуть исход. Причина по умолчанию выводится из исхода. */
  async function refuse(
    tabId: number,
    entry: TabEntry,
    outcome: HandOffTabOutcome,
    refusal: TabRefusal | undefined = refusalOf(outcome),
  ): Promise<HandOffTabOutcome> {
    // Закрытой вкладке снимок не пишем: `forgetTab` его уже стёр.
    if (refusal !== undefined && !entry.removed) await dependencies.tabModel.noteHandoffRefused(tabId, refusal);
    return outcome;
  }

  /** Исход для выбора без видео на передачу; `undefined` — видео выбрано. */
  function refusalOutcomeOf(choice: VideoChoice): HandOffTabOutcome | undefined {
    switch (choice.kind) {
      case "hand_off":
        return undefined;
      case "nothing":
        return { kind: "no_video" };
      case "not_supported_yet":
        return { kind: "not_supported_yet", form: choice.form };
      case "protected":
        return { kind: "protected" };
    }
  }

  async function localAddressSetting(): Promise<LocalAddressSetting> {
    try {
      return await dependencies.readLocalAddressSetting();
    } catch (error) {
      dependencies.reportSettingFailure(error);
      return DEFAULT_LOCAL_ADDRESS_SETTING;
    }
  }

  /**
   * Проверка адреса выбранного файла ДО сеанса: плеер не будится, запроса к адресу нет. Возвращает
   * правило для сеанса (он проверяет им `file_source` и перенаправления) или исход-отказ.
   */
  async function fileAddressRuleFor(chosen: ChosenVideo, consent: LocalAddressConsent): Promise<FileAddressRule | HandOffTabOutcome> {
    const setting = await localAddressSetting();
    const rule: FileAddressRule = { pageHost: chosen.frameHost, setting, consentedHost: null };
    if (chosen.fileHost === null) return rule;
    const decision = decideFileAddress(chosen.frameHost, chosen.fileHost, setting, consent);
    switch (decision.kind) {
      case "allowed":
        // В правило сеанса согласие попадает, только если оно про этот файл: иначе оно ничего не значит.
        return consent.kind === "given_for" && consent.host === chosen.fileHost ? { ...rule, consentedHost: chosen.fileHost } : rule;
      case "refused":
        return { kind: "local_address_refused" };
      case "needs_consent":
        return { kind: "local_address_needs_consent", host: chosen.fileHost };
    }
  }

  /** Шаг «открыть сеанс плееру и странице» для выбранного видео. */
  async function openSessionFor(tabId: number, entry: TabEntry, chosen: ChosenVideo, fileAddressRule: FileAddressRule): Promise<HandOffTabOutcome> {
    const tab = await dependencies.describeTab(tabId);
    if (tab === undefined || entry.removed) return { kind: "failed", reason: "tab_closed" };
    attempt += 1;
    const session = startHandoffSession(
      {
        tabId,
        tabUrl: tab.url,
        frameId: chosen.frameId,
        document: chosen.document,
        element: chosen.element,
        sessionKey: dependencies.sessionKeyFor(tabId, attempt),
        page: pageIdentityOf(tab),
        beginRequestId: takeBeginRequestId(),
        fileAddressRule,
        video: chosen,
        nextBeginRequestId: takeBeginRequestId,
      },
      dependencies,
      (end) => {
        if (entries.get(tabId) === entry) entries.delete(tabId);
        // Сеанс кончился по вине страницы: причина — в модель, попап покажет её честно.
        const refusal = refusalOfSessionEnd(end);
        if (refusal !== undefined && !entry.removed) void dependencies.tabModel.noteHandoffRefused(tabId, refusal);
        dependencies.onSessionEnded?.(tabId, end);
      },
    );
    entry.session = session;
    const outcome = await session.outcome;
    // Сеанс не состоялся (плеер отказал, элемент стал защищённым или пропал): причина — в модель.
    return outcome.kind === "started" ? outcome : refuse(tabId, entry, outcome);
  }

  async function startForTab(tabId: number, entry: TabEntry, consent: LocalAddressConsent): Promise<HandOffTabOutcome> {
    const survey = await surveyTabVideos(tabId, dependencies.executeInAllFrames, dependencies.scheduler);
    if (entry.removed) return { kind: "failed", reason: "tab_closed" };
    if (survey.kind === "not_scriptable") {
      // Служебная страница браузера, магазин расширений, закрытая вкладка: внедрение запрещено.
      const outcome: HandOffTabOutcome = { kind: "failed", reason: `tab_not_scriptable:${survey.error}` };
      // Вкладки уже нет: `tabs.onRemoved` мог сработать в прошлой жизни фона, и запись осталась бы навсегда.
      return (await dependencies.tabExists(tabId)) ? refuse(tabId, entry, outcome, { kind: "tab_not_scriptable" }) : outcome;
    }
    if (survey.kind === "timed_out") {
      return refuse(tabId, entry, { kind: "failed", reason: "survey_timed_out" }, { kind: "survey_timed_out" });
    }
    await dependencies.tabModel.recordSurvey(tabId, survey.frames);
    const choice = chooseVideo(survey.frames);
    // Нет видео, неподдержанная форма, DRM или одна реклама: честный отказ БЕЗ сеанса — плеер не
    // будится, видео остаётся играть в браузере.
    const refusalOutcome = refusalOutcomeOf(choice);
    if (refusalOutcome !== undefined) return refuse(tabId, entry, refusalOutcome);
    if (choice.kind !== "hand_off") return refuse(tabId, entry, { kind: "no_video" });
    const addressCheck = await fileAddressRuleFor(choice.video, consent);
    if ("kind" in addressCheck) return refuse(tabId, entry, addressCheck);
    return openSessionFor(tabId, entry, choice.video, addressCheck);
  }

  function handOffTab(tabId: number, consent: LocalAddressConsent = NO_LOCAL_ADDRESS_CONSENT): Promise<HandOffTabOutcome> {
    const existing = entries.get(tabId);
    if (existing !== undefined) return existing.outcome;
    const entry: TabEntry = { outcome: Promise.resolve({ kind: "no_video" }), session: undefined, removed: false };
    const outcome = startForTab(tabId, entry, consent).then(
      (result) => {
        // Без сеанса (видео нет, вкладка недоступна) запись больше не нужна.
        if (entry.session === undefined && entries.get(tabId) === entry) entries.delete(tabId);
        return result;
      },
      (error: unknown): HandOffTabOutcome => {
        if (entries.get(tabId) === entry) entries.delete(tabId);
        return { kind: "failed", reason: `internal:${String(error)}` };
      },
    );
    entry.outcome = outcome;
    entries.set(tabId, entry);
    return outcome;
  }

  return {
    handOffTab,
    tabRemoved(tabId) {
      const entry = entries.get(tabId);
      if (entry === undefined) return;
      entry.removed = true;
      entry.session?.notifyTabRemoved();
    },
    heldVideo: (tabId, frameId, report) => entries.get(tabId)?.session?.heldVideo(frameId, report) ?? "release",
    isAdOnScreen: (tabId) => entries.get(tabId)?.session?.adOnScreen() ?? false,
    adWatchReport: (tabId, frameId, ad) => entries.get(tabId)?.session?.adWatchReport(frameId, ad),
    activeTabIds: () => [...entries.keys()],
  };
}
