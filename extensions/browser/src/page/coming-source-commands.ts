// Команды тени для источника, который плеер ещё не знает (сессия 16a3, находка 3 живой проверки 16a2).
//
// Зачем. Страница ставит переданному элементу новый MediaSource (следующая серия, реклама, смена
// качества) и тут же, тем же синхронным кодом, зовёт `play()` и `currentTime = 0`. Так делает PlayerJS
// Tortuga (hls.js) в конце серии: `load()` → новый `src` → `play()`. Тень честно повторяет браузер и
// шлёт `page_playback{play}` и `page_seek{0}`. Но новый MediaSource плеер узнаёт только по первому его
// событию, а оно бывает после `sourceopen` — следующей задачей браузера, когда синхронный код страницы
// уже кончился. Команды без адреса попадали в ПРОШЛОЕ поколение: доигравшая серия перематывалась на 0 и
// около 0,4 с играла сначала, пока не загрузится новая.
//
// Правило. Команда тени уходит плееру, только когда текущий источник элемента — поток, уже названный
// плееру. Иначе она ждёт и уходит сразу за первым событием нового потока, то есть после того, как фон
// объявил плееру новое поколение. Браузер ведёт себя так же: `play()` элемента, чей MediaSource ещё не
// открылся, ничего не играет до первых данных.
//
// Память ограничена по построению (страница — враг): ждут не больше трёх команд, по одной на слот
// «перемотка», «play/pause», «скорость». Новая команда слота заменяет прежнюю: адресат один, и важна
// только последняя воля страницы. Порядок последних подач сохраняется.
//
// Чем модуль НЕ владеет: решением «назван ли источник» (это знает контроллер передачи по своему
// учёту отправленных потоков) и судьбой ждущих команд при конце передачи (они уходят вместе с ней:
// элемент вернулся странице, и адресата у команд больше нет).

import type { PageEvent } from "../shared/handoff-contract.ts";

/** Команда тени плееру, у которой есть адресат — текущий источник элемента. */
export type SourceCommand = Extract<PageEvent, { readonly kind: "page_seek" }> | Extract<PageEvent, { readonly kind: "page_playback" }>;

/** Знает ли плеер текущий источник элемента. */
export type CurrentSourceNaming =
  /** По потоку этого источника плееру уже ушло событие: фон объявил его поколение. */
  | "named"
  /** Источника нет или плеер его ещё не знает (новый MediaSource до `sourceopen`). */
  | "not_yet_named";

/** Слот ждущей команды: в каждом живёт только последняя команда. */
type CommandSlot = "seek" | "transport" | "rate";

/** Это событие тени — команда для текущего источника? */
export function isSourceCommand(event: PageEvent): event is SourceCommand {
  return event.kind === "page_seek" || event.kind === "page_playback";
}

/** К какому слоту относится команда. */
function slotOf(command: SourceCommand): CommandSlot {
  if (command.kind === "page_seek") return "seek";
  return command.request.kind === "set_rate" ? "rate" : "transport";
}

/** Ждущие команды одной передачи. */
export class ComingSourceCommands {
  /** Ждущие команды в порядке их последней подачи; не больше одной на слот. */
  #waiting: SourceCommand[] = [];

  /**
   * Новая команда тени. Возвращает, что отправить плееру сейчас и в каком порядке: при названном
   * источнике — ждавшие и новую, иначе ничего (новая встала ждать).
   */
  submit(command: SourceCommand, naming: CurrentSourceNaming): readonly SourceCommand[] {
    this.#hold(command);
    return this.release(naming);
  }

  /** Источник мог стать названным: отдать ждавшие команды (пустой список — ещё рано или ждать нечего). */
  release(naming: CurrentSourceNaming): readonly SourceCommand[] {
    if (naming !== "named" || this.#waiting.length === 0) return [];
    const released = this.#waiting;
    this.#waiting = [];
    return released;
  }

  /** Есть ли ждущие команды (контроллеру — чтобы не спрашивать об источнике зря). */
  get hasWaiting(): boolean {
    return this.#waiting.length > 0;
  }

  /** Поставить команду ждать: прежняя команда того же слота уходит, новая — в конец. */
  #hold(command: SourceCommand): void {
    const slot = slotOf(command);
    this.#waiting = this.#waiting.filter((waiting) => slotOf(waiting) !== slot);
    this.#waiting.push(command);
  }
}
