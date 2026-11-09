// Команда «отдать вкладку N» (решение 175): опрос фреймов → запись в модель вкладки → выбор видео
// (с отсевом рекламы, мусора и DRM) → проверка адреса файла (решение 199) → сеанс. Каждый отказ
// записывается в модель вкладки (09c). Рядом — «опроси вкладку N» (10b): тот же опрос без передачи.
//
// Владеет таблицей «вкладка → её передача». Повторная команда без выбранного видео для вкладки, у которой
// передача уже идёт или стартует, получает исход ТОЙ ЖЕ передачи (кнопку попапа можно нажать дважды);
// после конца передачи вкладку можно отдать снова — с новым ключом сеанса.
//
// Замена видео на той же вкладке (10b, решение 215): команда с выбранным видео при живом сеансе.
// 1. тот же элемент, что уже в плеере, — сразу `started`, ничего не трогаем;
// 2. опрос, выбор и проверка адреса идут при ещё нетронутом старом сеансе; любой отказ его не касается;
// 3. новый сеанс открывается с новым ключом (плеер заменяет окно, старому сокету шлёт `return_to_page`),
//    а страница нового сеанса получает `begin_handoff` только после конца старого (`AfterPreviousSession`).
// Старая запись на время замены уходит из `entries` в `replaced`: закрытие вкладки доходит и до неё,
// конец старого сеанса не теряется, а удаление старой записи не стирает новую. Если новый сеанс не
// состоялся, а старый ещё жив, старая запись возвращается на место.

import type { VariantWatchReport } from "../../shared/variant-watch-contract.ts";
import type { ReloadReason, HandOffTabOutcome, HeldVideoReply, HeldVideoReport, VideoRef } from "../../shared/handoff-contract.ts";
import type { SurveyTabReply, TabPlayerPresence } from "../../shared/survey-contract.ts";
import type { PageAdState } from "../../protocol/index.ts";
import type { TabModel } from "../tab-model/tab-model.ts";
import { sameRef, type TabRefusal } from "../tab-model/tab-snapshot.ts";
import type { LocalAddressSetting } from "../../shared/local-address-setting.ts";
import { DEFAULT_LOCAL_ADDRESS_SETTING } from "../../shared/local-address-setting.ts";
import { decideFileAddress, NO_LOCAL_ADDRESS_CONSENT, type FileAddressRule, type LocalAddressConsent } from "./file-address-guard.ts";
import { surveyTabVideos, type ExecuteInAllFrames } from "./frame-survey.ts";
import { refusalOf, refusalOfSessionEnd } from "./handoff-refusal.ts";
import { HANDOFF_END_REPLY_TIMEOUT_MS } from "./constants.ts";
import { startHandoffSession, type HandoffSession, type HandoffSessionDependencies, type HandoffSessionEnd } from "./handoff-session.ts";
import { returnablePageOf } from "./handoff-session-dependencies.ts";
import { pageIdentityOf } from "./page-identity.ts";
import { chooseRequestedVideo, chooseVideo, type ChosenVideo, type RequestedVideoChoice } from "./video-choice.ts";

/** Наибольший номер запроса `begin_handoff` (u32 договора); после него счёт идёт с 1. */
const MAX_BEGIN_REQUEST_ID = 0xffff_ffff;

export interface TabHandoffsDependencies extends HandoffSessionDependencies {
  readonly executeInAllFrames: ExecuteInAllFrames;
  /** Модель вкладки: сюда пишутся итог каждого опроса и каждый отказ. Её сбой передачу не ломает. */
  readonly tabModel: TabModel;
  /** Версия экстеншена: с ней сверяется скрипт страницы при опросе (сессия 10d). */
  readonly extensionVersion: string;
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
  /**
   * `consent` — согласие человека на локальный хост; ставит только страница экстеншена, без него — нет.
   * `video` — выбор человека из списка попапа; без него отдаётся главное видео вкладки.
   */
  handOffTab(tabId: number, consent?: LocalAddressConsent, video?: VideoRef): Promise<HandOffTabOutcome>;
  /** Опрос вкладки без передачи: плеер не будится, модель вкладки обновляется (10b). Не бросает. */
  surveyTab(tabId: number): Promise<SurveyTabReply>;
  /** `tabs.onRemoved`. */
  tabRemoved(tabId: number): void;
  /** Сторож задержал видео во фрейме вкладки. Нет сеанса — видео отпускается. */
  heldVideo(tabId: number, frameId: number, report: HeldVideoReport): HeldVideoReply;
  /** На экране плеера реклама; сеанса нет — `false` (решение 204, сессия 09d). */
  isAdOnScreen(tabId: number): boolean;
  /** Донесение фрейма о рекламе (09d). Нет сеанса у вкладки — игнорируется: страница вне передачи молчит. */
  adWatchReport(tabId: number, frameId: number, ad: PageAdState): void;
  /** Донесение фрейма о меню вариантов страницы (14b). Нет сеанса у вкладки — игнорируется. */
  variantWatchReport(tabId: number, frameId: number, report: VariantWatchReport): void;
  /** Вкладки, у которых передача стартует или идёт. */
  activeTabIds(): number[];
  /**
   * Подписаться на конец сеанса вкладки (входы 10c: вкладка по ссылке закрывается концом сеанса). Слушатель зовётся
   * после `onSessionEnded` зависимостей. Замена видео на той же вкладке тоже кончает старый сеанс: слушатель
   * сам смотрит `activeTabIds()` (вкладка в списке — сеанс продолжается). Сбой слушателя соседей не ломает.
   */
  onSessionEnded(listener: SessionEndedListener): void;
  /**
   * Подписаться на старт сеанса вкладки (плеер принял, страница начала тень). Нужен тем, кому надо знать
   * «передача на этой вкладке удалась», не зная, откуда её начали (попап, меню, кнопка).
   */
  onSessionStarted(listener: SessionStartedListener): void;
}

/** Слушатель старта сеанса вкладки. */
export type SessionStartedListener = (tabId: number) => void;

/** Слушатель конца сеанса вкладки. */
export type SessionEndedListener = (tabId: number, end: HandoffSessionEnd) => void;

interface TabEntry {
  /** Вкладка страницы сеанса; после возврата страницы (11b2) — новая вкладка. */
  tabId: number;
  /** Исход передачи; повторная команда получает его же. */
  outcome: Promise<HandOffTabOutcome>;
  session: HandoffSession | undefined;
  /** Исход `started` получен: «сеанс есть» для опроса вкладки считается только с этого момента. */
  started: boolean;
  removed: boolean;
  /** Сеанс этой записи кончился. Разрешается один раз; у записи без сеанса не разрешается никогда. */
  readonly ended: Promise<void>;
  markEnded(): void;
}

/** Видео уже в окне плеера (то же видео, повторная команда): новое окно не открывалось (решение 216). */
const STARTED_IN_OPEN_WINDOW: HandOffTabOutcome = { kind: "started", player_window: "already_open" };

/** Подготовка передачи: выбранное видео и правило адреса либо отказ (он уже записан в модель). */
type PreparedHandoff =
  | { readonly kind: "ready"; readonly video: ChosenVideo; readonly fileAddressRule: FileAddressRule }
  | { readonly kind: "refused"; readonly outcome: HandOffTabOutcome };

function createEntry(tabId: number): TabEntry {
  let markEnded: () => void = () => {};
  const ended = new Promise<void>((resolve) => (markEnded = resolve));
  return { tabId, outcome: Promise.resolve({ kind: "no_video" }), session: undefined, started: false, removed: false, ended, markEnded };
}

export function createTabHandoffs(dependencies: TabHandoffsDependencies): TabHandoffs {
  const entries = new Map<number, TabEntry>();
  // Записи, чей сеанс заменён новым той же вкладки, но ещё не кончился (см. шапку файла).
  const replaced = new Set<TabEntry>();
  const sessionEndedListeners: SessionEndedListener[] = [];
  const sessionStartedListeners: SessionStartedListener[] = [];
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

  /**
   * Вкладка старше скрипта, и поэтому передать нечего (сессия 10d). Если в свежем подфрейме нашлось
   * передаваемое видео, перезагрузка не нужна; `undefined` — это обычный случай.
   */
  function reloadOutcomeOf(choice: RequestedVideoChoice, scriptGap: ReloadReason | null): HandOffTabOutcome | undefined {
    if (choice.kind === "not_supported_yet" && choice.form === "started_before_script") return { kind: "reload_needed", reason: "stream_started_before_script" };
    return choice.kind === "nothing" && scriptGap !== null ? { kind: "reload_needed", reason: scriptGap } : undefined;
  }

  /** Исход для выбора без видео на передачу; `undefined` — видео выбрано. */
  function refusalOutcomeOf(choice: RequestedVideoChoice): HandOffTabOutcome | undefined {
    switch (choice.kind) {
      case "hand_off":
        return undefined;
      case "gone":
        return { kind: "video_gone" };
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

  /** Сообщить подписчикам; исключение одного не отменяет остальных и конца сеанса. */
  function notifySessionEnded(tabId: number, end: HandoffSessionEnd): void {
    for (const listener of sessionEndedListeners) {
      try {
        listener(tabId, end);
      } catch (error) {
        dependencies.noteForDeveloper(`a session-ended listener threw: ${String(error)}`);
      }
    }
  }

  /** Шаг «открыть сеанс плееру и странице» для выбранного видео. */
  async function openSessionFor(
    tabId: number,
    entry: TabEntry,
    chosen: ChosenVideo,
    fileAddressRule: FileAddressRule,
    replacing: TabEntry | undefined,
  ): Promise<HandOffTabOutcome> {
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
        pagePlace: returnablePageOf(tab),
        movePageToTab: (newTabId) => movePageToTab(entry, newTabId),
        nextBeginRequestId: takeBeginRequestId,
        ...(replacing === undefined ? {} : { afterPreviousSession: () => waitForSessionEnd(replacing) }),
      },
      dependencies,
      (end) => {
        // Вкладка записи — текущая: страница сеанса могла вернуться в другую вкладку (11b2).
        const endedTabId = entry.tabId;
        if (entries.get(endedTabId) === entry) entries.delete(endedTabId);
        replaced.delete(entry);
        entry.markEnded();
        // Сеанс кончился по вине страницы: причина — в модель, попап покажет её честно.
        const refusal = refusalOfSessionEnd(end);
        if (refusal !== undefined && !entry.removed) void dependencies.tabModel.noteHandoffRefused(endedTabId, refusal);
        dependencies.onSessionEnded?.(endedTabId, end);
        notifySessionEnded(endedTabId, end);
      },
    );
    entry.session = session;
    const outcome = await session.outcome;
    if (outcome.kind === "started") {
      entry.started = true;
      for (const listener of sessionStartedListeners) {
        try {
          listener(tabId);
        } catch (error) {
          dependencies.noteForDeveloper(`a session-started listener threw: ${String(error)}`);
        }
      }
      return outcome;
    }
    // Сеанс не состоялся (плеер отказал, элемент стал защищённым или пропал): причина — в модель.
    return refuse(tabId, entry, outcome);
  }

  /**
   * Ждать конец прежнего сеанса вкладки, но не дольше предела снятия тени. Не отклоняется: зависший
   * старый сеанс нового не держит (он кончится сам по своим таймерам).
   */
  function waitForSessionEnd(previous: TabEntry): Promise<void> {
    return new Promise((resolve) => {
      const timer = dependencies.scheduler.setTimeout(resolve, HANDOFF_END_REPLY_TIMEOUT_MS);
      void previous.ended.then(() => {
        dependencies.scheduler.clearTimeout(timer);
        resolve();
      });
    });
  }

  /**
   * Всё, что решается ДО сеанса: опрос, запись в модель, выбор (главное видео или выбор человека),
   * проверка адреса. Сеансов не трогает: отказ здесь оставляет всё как было. `entry` — запись, чьё
   * закрытие вкладки обрывает подготовку (при замене это ещё старая запись).
   */
  async function prepareHandoff(tabId: number, entry: TabEntry, consent: LocalAddressConsent, requested: VideoRef | undefined): Promise<PreparedHandoff> {
    const refused = async (outcome: HandOffTabOutcome, refusal?: TabRefusal): Promise<PreparedHandoff> => ({
      kind: "refused",
      outcome: await (refusal === undefined ? refuse(tabId, entry, outcome) : refuse(tabId, entry, outcome, refusal)),
    });
    const survey = await surveyTabVideos(tabId, dependencies.executeInAllFrames, dependencies.scheduler, dependencies.extensionVersion);
    if (entry.removed) return { kind: "refused", outcome: { kind: "failed", reason: "tab_closed" } };
    if (survey.kind === "not_scriptable") {
      // Служебная страница браузера, магазин расширений, закрытая вкладка: внедрение запрещено.
      const outcome: HandOffTabOutcome = { kind: "failed", reason: `tab_not_scriptable:${survey.error}` };
      // Вкладки уже нет: `tabs.onRemoved` мог сработать в прошлой жизни фона, и запись осталась бы навсегда.
      return (await dependencies.tabExists(tabId)) ? refused(outcome, { kind: "tab_not_scriptable" }) : { kind: "refused", outcome };
    }
    if (survey.kind === "timed_out") return refused({ kind: "failed", reason: "survey_timed_out" }, { kind: "survey_timed_out" });
    await dependencies.tabModel.recordSurvey(tabId, survey.frames);
    const choice = requested === undefined ? chooseVideo(survey.frames) : chooseRequestedVideo(survey.frames, requested);
    // Нет видео, неподдержанная форма, DRM, одна реклама или устаревшая ссылка: честный отказ БЕЗ сеанса —
    // плеер не будится, видео остаётся играть в браузере.
    const refusalOutcome = reloadOutcomeOf(choice, survey.scriptGap) ?? refusalOutcomeOf(choice);
    if (refusalOutcome !== undefined) return refused(refusalOutcome);
    if (choice.kind !== "hand_off") return refused({ kind: "no_video" });
    const addressCheck = await fileAddressRuleFor(choice.video, consent);
    if ("kind" in addressCheck) return refused(addressCheck);
    return { kind: "ready", video: choice.video, fileAddressRule: addressCheck };
  }

  async function startForTab(tabId: number, entry: TabEntry, consent: LocalAddressConsent, requested: VideoRef | undefined): Promise<HandOffTabOutcome> {
    const prepared = await prepareHandoff(tabId, entry, consent, requested);
    if (prepared.kind === "refused") return prepared.outcome;
    return openSessionFor(tabId, entry, prepared.video, prepared.fileAddressRule, undefined);
  }

  /** Записать запись в таблицу и связать с ней исход передачи. Исход не отклоняется. */
  function register(tabId: number, entry: TabEntry, run: () => Promise<HandOffTabOutcome>): Promise<HandOffTabOutcome> {
    const outcome = run().then(
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

  /** Видео, которое сеанс записи держит в плеере, — именно это? */
  function isInPlayer(entry: TabEntry, video: VideoRef): boolean {
    const shown = entry.session?.videoInPlayer() ?? null;
    return shown !== null && sameRef(shown, video);
  }

  /**
   * Команда с выбранным видео, когда у вкладки уже есть запись. Передача ещё стартует — сначала ждём её
   * исход (решать можно только по настоящему положению дел), потом либо `started` (то же видео), либо замена.
   */
  async function handOffRequestedVideo(tabId: number, existing: TabEntry, consent: LocalAddressConsent, video: VideoRef): Promise<HandOffTabOutcome> {
    const startOutcome = await existing.outcome;
    // Пока ждали, сеанс кончился или заменён: решаем заново по новому положению дел.
    if (entries.get(tabId) !== existing) return handOffTab(tabId, consent, video);
    // Запись жива, но передача не состоялась (сеанс ещё закрывается): исход той же передачи.
    if (!existing.started) return startOutcome;
    if (isInPlayer(existing, video)) return STARTED_IN_OPEN_WINDOW;
    return replaceSession(tabId, existing, consent, video);
  }

  /** Замена видео на той же вкладке (шапка файла, решение 215). */
  async function replaceSession(tabId: number, previous: TabEntry, consent: LocalAddressConsent, video: VideoRef): Promise<HandOffTabOutcome> {
    const prepared = await prepareHandoff(tabId, previous, consent, video);
    if (prepared.kind === "refused") return prepared.outcome;
    if (previous.removed) return { kind: "failed", reason: "tab_closed" };
    // Пока готовились, сеанс кончился или его заменила другая команда: решаем заново.
    if (entries.get(tabId) !== previous) return handOffTab(tabId, consent, video);
    // Сеанс сам переехал на выбранное видео (реклама кончилась), пока готовились.
    if (isInPlayer(previous, video)) return STARTED_IN_OPEN_WINDOW;
    const entry = createEntry(tabId);
    replaced.add(previous);
    const outcome = await register(tabId, entry, () => openSessionFor(tabId, entry, prepared.video, prepared.fileAddressRule, previous));
    if (outcome.kind !== "started") giveBackPreviousEntry(tabId, entry, previous);
    return outcome;
  }

  /** Новый сеанс не состоялся: если старый ещё жив, он снова текущий (его реклама и сторож — снова его). */
  function giveBackPreviousEntry(tabId: number, failed: TabEntry, previous: TabEntry): void {
    if (!replaced.delete(previous)) return;
    const current = entries.get(tabId);
    if (current === undefined || current === failed) entries.set(tabId, previous);
  }

  /** Состояние вкладки с плеером для ответа на опрос. */
  function presenceOf(tabId: number): TabPlayerPresence {
    const entry = entries.get(tabId);
    if (entry === undefined || !entry.started || entry.session === undefined) return { kind: "no_session" };
    return { kind: "in_player", video: entry.session.videoInPlayer() };
  }

  async function surveyTab(tabId: number): Promise<SurveyTabReply> {
    try {
      const survey = await surveyTabVideos(tabId, dependencies.executeInAllFrames, dependencies.scheduler, dependencies.extensionVersion);
      if (survey.kind === "not_scriptable") return { kind: "unavailable", reason: "tab_not_scriptable" };
      if (survey.kind === "timed_out") return { kind: "unavailable", reason: "survey_timed_out" };
      await dependencies.tabModel.recordSurvey(tabId, survey.frames);
      const [videos, main] = await Promise.all([dependencies.tabModel.videosFoundIn(tabId), dependencies.tabModel.mainVideoOf(tabId)]);
      return { kind: "surveyed", videos, main, in_player: presenceOf(tabId) };
    } catch {
      return { kind: "unavailable", reason: "survey_failed" };
    }
  }

  function handOffTab(tabId: number, consent: LocalAddressConsent = NO_LOCAL_ADDRESS_CONSENT, video?: VideoRef): Promise<HandOffTabOutcome> {
    const existing = entries.get(tabId);
    if (existing === undefined) {
      const entry = createEntry(tabId);
      return register(tabId, entry, () => startForTab(tabId, entry, consent, video));
    }
    if (video !== undefined) return handOffRequestedVideo(tabId, existing, consent, video);
    // Без выбранного видео: передача уже идёт — окно плеера открыто (решение 216); ещё стартует — исход той же
    // передачи (кнопку можно нажать дважды).
    return existing.started ? Promise.resolve(STARTED_IN_OPEN_WINDOW) : existing.outcome;
  }

  /**
   * Страница сеанса вернулась в другую вкладку (11b2): запись переезжает на её номер, чтобы донесения
   * сторожа, реклама и закрытие вкладки шли этому сеансу. Прежняя вкладка закрыта или показывает другой
   * адрес, её номер свободен. В новой вкладке уже есть своя передача — её запись не трогаем: такой сеанс
   * просто не получит донесений и откажет по сроку.
   */
  function movePageToTab(entry: TabEntry, newTabId: number): void {
    if (entries.get(entry.tabId) === entry) entries.delete(entry.tabId);
    entry.tabId = newTabId;
    entry.removed = false;
    if (entries.has(newTabId)) {
      dependencies.noteForDeveloper(`page return: tab ${newTabId} already has a handoff`);
      return;
    }
    entries.set(newTabId, entry);
  }

  /** Закрытие вкладки доходит и до текущей записи, и до заменённых, чей сеанс ещё не кончился. */
  function tabRemoved(tabId: number): void {
    const affected = [entries.get(tabId), ...replaced].filter((entry): entry is TabEntry => entry !== undefined && entry.tabId === tabId);
    for (const entry of affected) {
      entry.removed = true;
      entry.session?.notifyTabRemoved();
    }
  }

  return {
    handOffTab,
    surveyTab,
    tabRemoved,
    heldVideo: (tabId, frameId, report) => entries.get(tabId)?.session?.heldVideo(frameId, report) ?? "release",
    isAdOnScreen: (tabId) => entries.get(tabId)?.session?.adOnScreen() ?? false,
    adWatchReport: (tabId, frameId, ad) => entries.get(tabId)?.session?.adWatchReport(frameId, ad),
    variantWatchReport: (tabId, frameId, report) => entries.get(tabId)?.session?.variantWatchReport(frameId, report),
    activeTabIds: () => [...entries.keys()],
    onSessionEnded: (listener) => void sessionEndedListeners.push(listener),
    onSessionStarted: (listener) => void sessionStartedListeners.push(listener),
  };
}
