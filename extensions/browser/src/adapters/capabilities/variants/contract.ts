// Контракт способности «варианты страницы» (сессия 14b, решения 17, 19, 279, 282): что плеер сайта
// предлагает — озвучки, качества, следующая серия — и как это переключить на странице.
//
// Адаптер — пульт от плеера сайта. Качает по-прежнему страница: адаптер только читает её меню и жмёт её
// кнопки. Позицию после смены варианта возвращает НАШ плеер (решение 280), адаптеру это уметь не нужно.
//
// Две «руки» (решение 282):
// - `dom` — DOM фрейма в изолированном мире: читать разметку и нажимать кнопки;
// - `player` — функции плеера сайта в MAIN-мире по белому списку адаптера (`registry/page-world.ts`):
//   для сайтов, которые не верят кликам из кода.
// Больше адаптер не видит ничего: ни портов, ни хранилища, ни прав фона.
//
// Инвариант 1 (страница — враг): описание — сырой текст страницы. Его проверяет и обрезает фон
// (`background/handoff/variant-menu-normalize.ts`) до отправки плееру.

import type { AdapterHandOutcome } from "../../../shared/adapter-hand-contract.ts";
import type { AdElement, AdFramePlace, AdPageView } from "../ad-organs/contract.ts";

/** Вид варианта, который адаптер показывает в нашем меню. Субтитры приходят дорожками (13b), не вариантами. */
export type PageVariantKind = "audio" | "quality";

/** Пункт меню сайта: непрозрачный ключ адаптера, подпись страницы и «выбран ли сейчас». */
export interface PageVariantOption {
  /**
   * Ключ, по которому адаптер найдёт пункт снова (`select`). Плеер видит его как непрозрачный id, поэтому
   * правило протокола: 1–64 символа `A-Z a-z 0-9 - _`. Пункт с другим ключом фон не отправит (подпись
   * сайта на кириллице — это `label`, а ключом может быть номер пункта).
   */
  readonly id: string;
  /** Подпись, как её пишет сайт: «Dub B», «720p». */
  readonly label: string;
  readonly selected: boolean;
}

/** Что плеер сайта предлагает прямо сейчас. */
export interface PageVariantDescription {
  /**
   * Ключ сериала, по которому экстеншен запомнит выбор (решения 44, 281). Строка сайта: в плеер не уходит
   * (решение 283), в хранилище — только отпечаток. `null` — у страницы нет понятия сериала.
   */
  readonly seriesKey: string | null;
  /** Ключ серии внутри сериала (сезон и номер, если сайт их знает); `null` — неизвестно. */
  readonly episodeKey: string | null;
  readonly audio: readonly PageVariantOption[];
  readonly quality: readonly PageVariantOption[];
  /** Есть ли следующая серия, которую сайт откроет по `next()`. */
  readonly nextEpisode: boolean;
}

/**
 * Чем кончилось действие адаптера:
 * - `done` — кнопка нажата или функция сайта отработала;
 * - `no_such_variant` — такого пункта (или следующей серии) на странице уже нет;
 * - `person_only` — сайт принимает только настоящую мышь (как YouTube, решение 207): нажать может человек;
 * - `failed` — страница повела себя не так, как адаптер ожидал (функция бросила, рука не ответила).
 */
export type VariantActionOutcome = "done" | "no_such_variant" | "person_only" | "failed";

/** Взгляд на DOM для вариантов: тот же, что у читателей рекламы, плюс чтение атрибута. */
export interface VariantPageView extends AdPageView {
  /** Значение атрибута элемента; `null` — атрибута нет. */
  attributeOf(element: AdElement, name: string): string | null;
}

/** Итог вызова руки-функции: ответ MAIN-мира или `no_answer` — он не ответил в срок (или порта нет). */
export type PlayerHandResult = AdapterHandOutcome | { readonly kind: "no_answer" };

/** Рука-функция одного адаптера: имя сайта уже вшито, адаптер называет только действие своего белого списка. */
export interface PlayerHand {
  call(action: string, argument: string | null): Promise<PlayerHandResult>;
}

/** Обе руки адаптера. */
export interface VariantHands {
  readonly dom: VariantPageView;
  readonly player: PlayerHand;
}

/** Адаптер вариантов одного сайта. Любой метод может бросить: это ловит наблюдатель фрейма. */
export interface VariantAdapter {
  /** Адаптер возможен в этом фрейме? Дёшево; зовётся при каждом продлении аренды. */
  matches(place: AdFramePlace): boolean;
  /** За каким деревом следить мутациями: корень плеера сайта или весь документ. */
  observationRoot(): AdElement;
  /** Что сайт предлагает сейчас; `null` — сейчас ничего (плеер ещё не построен, другая страница). */
  describe(): Promise<PageVariantDescription | null>;
  /** Выбрать пункт `id` вида `kind` на странице. */
  select(kind: PageVariantKind, id: string): Promise<VariantActionOutcome>;
  /** Открыть следующую серию на странице. */
  next(): Promise<VariantActionOutcome>;
}
