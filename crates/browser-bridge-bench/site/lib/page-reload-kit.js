// Общие куски страниц сценария `page-reload` (сессия 16c, решения 59, 304, 305): страница сайта, которая
// сама перезагружается или переходит по адресу в ТОЙ ЖЕ вкладке, пока её видео играет в окне плеера.
//
// Две страницы делят этот модуль: `page-reload.html` (вкладка) и `page-reload-frame.html` (плеер в
// iframe чужого origin). Журнал страницы (POST /bench/log, судит Rust, префикс `page-reload:`):
//   `load doc=<id> second_load=<bool> view=<v> ep=<n> origin=<origin>` — документ загрузился;
//   `handoff-ready` — первый документ играет (робот может отдавать вкладку);
//   `state doc=<id> second_load=<bool> ct=<с> paused=<bool> real_frames=<n> probe=<bool>` — раз в секунду;
//     `real_frames` — настоящие кадры браузера мимо тени (свежий about:blank-iframe);
//   `ack hook:navigate` и `nav-start target=<вид> url=<адрес>` — страница уходит прямо сейчас.

import { pageParams } from './bench-report.js';
import { loadCatalog, BenchPlayer, standardRefs } from './bench-player.js';
import { mountVideo } from './scenario-kit.js';
import { createRealFrameProbe } from './handoff-mode.js';
import { waitFor } from './bench-checks.js';

/** Ролик MSE (30 с) — «фильм» всех документов; серия 2 играет тот же ролик с меткой `ep=2`. */
export const FILM_ASSET = 'h264-aac';
/** Шаг строки состояния, мс. */
const STATE_PERIOD_MS = 1000;
/** Период опроса команд робота, мс. */
const COMMAND_POLL_MS = 250;
/** Сколько ждём, что первый документ заиграл, мс. */
const FIRST_PLAY_WAIT_MS = 15000;
/** «Играет»: часы ушли хотя бы на столько, с. */
const PLAYING_ADVANCE_S = 0.5;
/** Имя команды робота «уходи» и вид перехода в параметре адреса `nav`. */
export const NAVIGATE_HOOK = 'hook:navigate';

export const params = pageParams();
export const documentId = Math.random().toString(36).slice(2, 10);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Строка в журнал стенда; возвращает обещание, чтобы уходящая страница успела отправить. */
export function log(line) {
  console.log(`[page-reload] ${line}`);
  return fetch(`${params.reportOrigin}/bench/log`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ source: 'page', run: params.run, line: `page-reload: ${line}` }),
    keepalive: true,
  }).catch((error) => console.error('page-reload: log failed', error));
}

/** Команды робота этому прогону с `from` по порядку (пустой список при сбое опроса). */
async function fetchCommands(from) {
  try {
    const response = await fetch(`${params.reportOrigin}/bench/commands/${params.run}/${from}`);
    if (!response.ok) return [];
    const commands = await response.json();
    return Array.isArray(commands) ? commands : [];
  } catch (error) {
    console.error('page-reload: command poll failed', error);
    return [];
  }
}

/**
 * Второй ли это документ прогона: робот уже говорил `handed-off`. Возвращает также, сколько команд
 * к этому часу накопилось: второй документ НЕ должен проигрывать старые команды (повторный уход).
 */
export async function detectSecondLoad() {
  const commands = await fetchCommands(0);
  return { secondLoad: commands.includes('handed-off'), commandsSeen: commands.length };
}

/** Ждёт команду `hook:navigate` от робота (первый документ). Старые команды пропускает. */
export async function waitForNavigateHook() {
  let received = 0;
  for (;;) {
    const fresh = await fetchCommands(received);
    received += fresh.length;
    if (fresh.includes(NAVIGATE_HOOK)) return;
    await sleep(COMMAND_POLL_MS);
  }
}

/** Играет «фильм» MSE своим плеером страницы; чужую перемотку элемента плеер подхватывает. */
async function startFilm(video, catalog) {
  const player = new BenchPlayer(video, catalog, { log: (line) => log(`player ${line}`) });
  player.followPlayerSeeks();
  await player.start({ refs: standardRefs(catalog, FILM_ASSET), autoplay: true });
  return player;
}

/**
 * Документ с фильмом: ставит элемент, пишет строки состояния и (у первого документа) `handoff-ready`.
 * Второй документ стартует сам, как обычный сайт после перезагрузки или перехода.
 */
export async function runFilmDocument({ stage, secondLoad, view, episode }) {
  const catalog = await loadCatalog();
  const video = mountVideo(stage);
  const probe = await createRealFrameProbe();
  setInterval(() => {
    const frames = probe.available ? probe.framesOf(video) : 0;
    log(`state doc=${documentId} second_load=${secondLoad} ct=${video.currentTime.toFixed(2)} paused=${video.paused} real_frames=${frames} probe=${probe.available}`);
  }, STATE_PERIOD_MS);
  startFilm(video, catalog).catch((error) => log(`start failed: ${error}`));
  if (!secondLoad) {
    const startedAt = video.currentTime;
    const playing = await waitFor(() => !video.paused && video.currentTime >= startedAt + PLAYING_ADVANCE_S, FIRST_PLAY_WAIT_MS);
    log(playing ? 'handoff-ready' : 'first document did not start playing');
  }
  log(`film-started doc=${documentId} view=${view} ep=${episode}`);
}

/** Строка «документ загрузился» — первая строка каждого документа. */
export function logLoad({ secondLoad, view, episode }) {
  return log(`load doc=${documentId} second_load=${secondLoad} view=${view} ep=${episode} origin=${location.origin}`);
}

/** Уходит из этой страницы: подтверждает команду, пишет цель и переходит (настоящая навигация вкладки). */
export async function navigateAway(target, url) {
  await log(`ack ${NAVIGATE_HOOK}`);
  await log(`nav-start target=${target} url=${url}`);
  if (target === 'reload') location.reload();
  else location.assign(url);
}
