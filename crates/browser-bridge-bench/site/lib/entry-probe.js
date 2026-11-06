// Зонд входов (сессия 10c): страница стенда сообщает роботу, где на ЭКРАНЕ её элементы.
//
// Зачем. Робот кликает настоящей мышью по экрану, а страница знает свои элементы только в
// координатах окна страницы (клиентских). Где окно браузера стоит на экране, странице неизвестно
// (Wayland прячет это от неё). Поэтому калибровка мышью: робот ставит курсор в известную точку
// экрана, страница сообщает, какими `clientX/clientY` это пришло, смещение = экран − клиент.
// Каждая строка о движении мыши заодно несёт свежие прямоугольники целей: смещение и
// прямоугольники сняты в один момент, прокрутка и перекладка не успевают их рассинхронизировать.
//
// Формат строк журнала (разбирает робот, `entries/page_geometry.rs`):
//   entry-probe pointer <frame> client=<x>,<y> inner=<w>x<h> scroll=<w>x<h> rects=<имя>:<l>,<t>,<w>,<h>;...
// `scroll` — размер прокручиваемой области документа: по нему робот видит, что кнопка поверх
// видео не раздвинула страницу. Цель `viewport` — весь видимый документ (для правого клика мимо видео).
//   entry-probe contextmenu <frame> client=<x>,<y> target=<тег#id> prevented=<true|false>
// `<frame>` — `top` для страницы и `inner` для фрейма внутри неё: робот калибруется по `top`.
// Страница сама НИЧЕГО не решает за экстеншен: слушатели пассивные и ничего не отменяют.

/** Цели, которые страница назвала сама: имя -> функция, возвращающая элемент. */
const namedTargets = new Map();
let installed = false;

/**
 * Страница называет цель правого клика (например, `<video>` в закрытом shadow, который снаружи
 * не найти). Имена `video`, `video:<id>` и `iframe` робот понимает без регистрации.
 */
export function registerEntryTarget(name, getElement) {
  namedTargets.set(name, getElement);
}

/** Все цели: названные страницей и найденные в светлом DOM по умолчанию. */
function collectTargets() {
  const targets = new Map(namedTargets);
  const videos = [...document.querySelectorAll('video')];
  videos.forEach((video, index) => {
    const name = video.id ? `video:${video.id}` : `video:${index}`;
    if (!targets.has(name)) targets.set(name, () => video);
  });
  if (videos[0] && !targets.has('video')) targets.set('video', () => videos[0]);
  // Невидимый iframe счётчика кадров (aria-hidden, 0x0) целью не бывает.
  const frame = document.querySelector('iframe:not([aria-hidden="true"])');
  if (frame && !targets.has('iframe')) targets.set('iframe', () => frame);
  return targets;
}

const round = (value) => Math.round(value * 10) / 10;

/** Прямоугольники целей в клиентских координатах; цель без элемента пропускается. */
function describeRects() {
  const parts = [`viewport:0,0,${window.innerWidth},${window.innerHeight}`];
  for (const [name, getElement] of collectTargets()) {
    const element = getElement();
    if (!element) continue;
    const rect = element.getBoundingClientRect();
    parts.push(`${name}:${round(rect.left)},${round(rect.top)},${round(rect.width)},${round(rect.height)}`);
  }
  return parts.join(';');
}

/** Короткое имя элемента для журнала (кто принял событие). */
function describeElement(element) {
  if (!element || !element.tagName) return 'none';
  return `${element.tagName.toLowerCase()}${element.id ? `#${element.id}` : ''}`;
}

/** Период пульса страницы (`origin_beat=1`), мс. */
const ORIGIN_BEAT_PERIOD_MS = 500;

/**
 * Пульс страницы для строк позднего входа (10d): раз в полсекунды страница пишет момент своего
 * рождения (`performance.timeOrigin`) и позицию каждого `<video>`. Робот по нему видит три вещи:
 * страницу НЕ перезагружали (момент рождения не менялся), страницу перезагрузили (появился другой),
 * и с какой секунды ролика это случилось (последняя позиция перед сменой).
 * Формат: `entry-probe origin <timeOrigin> videos=<имя>:<секунды>:<пауза 0|1>;...`
 * Включается параметром адреса `origin_beat=1`: остальным строкам пульс не нужен.
 */
function startOriginBeat(context) {
  const describeVideos = () => [...document.querySelectorAll('video')]
    .map((video, index) => `${video.id || index}:${video.currentTime.toFixed(2)}:${video.paused ? 1 : 0}`)
    .join(';');
  const beat = () => context.log(`entry-probe origin ${performance.timeOrigin} videos=${describeVideos()}`);
  beat();
  setInterval(beat, ORIGIN_BEAT_PERIOD_MS);
}

/** Ставит слушатели один раз на страницу. */
export function installEntryProbe(context) {
  if (installed) return;
  installed = true;
  const frame = window === window.top ? 'top' : 'inner';
  if (frame === 'top' && context.params.raw.get('origin_beat') === '1') startOriginBeat(context);
  window.addEventListener('mousemove', (event) => {
    context.log(`entry-probe pointer ${frame} client=${event.clientX},${event.clientY} inner=${window.innerWidth}x${window.innerHeight} scroll=${document.documentElement.scrollWidth}x${document.documentElement.scrollHeight} rects=${describeRects()}`);
  }, { capture: true, passive: true });
  window.addEventListener('contextmenu', (event) => {
    // Слушатель в capture на window видит событие ДО страницы; `defaultPrevented` проверяем
    // позже, когда страница свой обработчик уже отработала.
    setTimeout(() => {
      context.log(`entry-probe contextmenu ${frame} client=${event.clientX},${event.clientY} target=${describeElement(event.target)} prevented=${event.defaultPrevented}`);
    }, 0);
  }, { capture: true, passive: true });
}
