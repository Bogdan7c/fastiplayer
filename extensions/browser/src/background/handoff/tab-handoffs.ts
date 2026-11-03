// Команда «отдать вкладку N» (решение 175): опрос фреймов → выбор видео → сеанс.
//
// Владеет таблицей «вкладка → её передача». Повторная команда для вкладки, у которой передача
// уже идёт или стартует, получает исход ТОЙ ЖЕ передачи (кнопку попапа можно нажать дважды);
// после конца передачи вкладку можно отдать снова — с новым ключом сеанса.

import type { HandOffTabOutcome } from "../../shared/handoff-contract.ts";
import { surveyTabVideos, type ExecuteInAllFrames } from "./frame-survey.ts";
import { startHandoffSession, type HandoffSession, type HandoffSessionDependencies, type HandoffSessionEnd } from "./handoff-session.ts";
import { pageIdentityOf } from "./page-identity.ts";
import { chooseVideo, type FrameCandidates } from "./video-choice.ts";

/** Наибольший номер запроса `begin_handoff` (u32 договора); после него счёт идёт с 1. */
const MAX_BEGIN_REQUEST_ID = 0xffff_ffff;

/** Что фон знает о вкладке для подписи окна. */
export interface TabDescription {
  readonly title?: string | undefined;
  readonly url?: string | undefined;
}

export interface TabHandoffsDependencies extends HandoffSessionDependencies {
  readonly executeInAllFrames: ExecuteInAllFrames;
  /** `tabs.get`: заголовок и адрес; `undefined` — вкладки нет. */
  readonly describeTab: (tabId: number) => Promise<TabDescription | undefined>;
  /** Ключ сеанса для передачи этой вкладки (base64url, ≤ 64 байт). */
  readonly sessionKeyFor: (tabId: number, attempt: number) => string;
  /** Конец передачи — для диагностики и тестов. */
  readonly onSessionEnded?: (tabId: number, end: HandoffSessionEnd) => void;
}

export interface TabHandoffs {
  handOffTab(tabId: number): Promise<HandOffTabOutcome>;
  /** `tabs.onRemoved`. */
  tabRemoved(tabId: number): void;
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

  async function startForTab(tabId: number, entry: TabEntry): Promise<HandOffTabOutcome> {
    let frames: FrameCandidates[];
    try {
      frames = await surveyTabVideos(tabId, dependencies.executeInAllFrames);
    } catch (error) {
      // Служебная страница браузера, магазин расширений, закрытая вкладка: внедрение запрещено.
      return { kind: "failed", reason: `tab_not_scriptable:${String(error)}` };
    }
    const choice = chooseVideo(frames);
    if (choice.kind === "nothing") return { kind: "no_video" };
    // Неподдержанная форма: честный отказ без сеанса — видео остаётся играть в браузере.
    if (choice.kind === "not_supported_yet") return { kind: "not_supported_yet", form: choice.form };
    const chosen = choice.video;
    const tab = await dependencies.describeTab(tabId);
    if (tab === undefined || entry.removed) return { kind: "failed", reason: "tab_closed" };
    attempt += 1;
    const session = startHandoffSession(
      {
        tabId,
        frameId: chosen.frameId,
        document: chosen.document,
        element: chosen.element,
        sessionKey: dependencies.sessionKeyFor(tabId, attempt),
        page: pageIdentityOf(tab),
        beginRequestId: takeBeginRequestId(),
      },
      dependencies,
      (end) => {
        if (entries.get(tabId) === entry) entries.delete(tabId);
        dependencies.onSessionEnded?.(tabId, end);
      },
    );
    entry.session = session;
    return session.outcome;
  }

  function handOffTab(tabId: number): Promise<HandOffTabOutcome> {
    const existing = entries.get(tabId);
    if (existing !== undefined) return existing.outcome;
    const entry: TabEntry = { outcome: Promise.resolve({ kind: "no_video" }), session: undefined, removed: false };
    const outcome = startForTab(tabId, entry).then(
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
    activeTabIds: () => [...entries.keys()],
  };
}
