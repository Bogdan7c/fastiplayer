// Общее тело страниц-атак (09c): страница играет обычный MSE-ролик, робот отдаёт вкладку плееру,
// а по команде `hook:attack` страница НАЧИНАЕТ ВРЕДИТЬ изнутри «тени»: гигантские сегменты,
// шквал событий, мусор вместо fMP4, шквал сообщений воркера. Экстеншен обязан не упасть и не
// взять с собой плеер: сеанс закрывается с причиной, а следующая честная страница играет (это
// проверяет сценарий `attacks`, а не страница).
//
// Что страница НЕ проверяет: «часы идут под тенью», «буфер докачивается» и прочие итоговые
// проверки живой тени — после успешной атаки тени больше нет. Они отключены поимённо
// (`ATTACK_SKIPPED_FINISH_CHECKS`), метрики окна остаются. Сама атака не бросает исключений
// наружу: любая ошибка внутри атаки записывается в журнал и в метрику `attack_errors`
// (иначе `unhandledrejection` испортил бы отчёт страницы, который сценарий требует `pass`).

import { runScenario, UnsupportedError } from './bench-report.js';
import { checkPlaying, sleep } from './bench-checks.js';
import { startAsset, observeHandoff } from './scenario-kit.js';

/** Итоговые проверки живой тени, которые после атаки теряют смысл. */
const ATTACK_SKIPPED_FINISH_CHECKS = [
  'page-alive-under-shadow',
  'page-clock-advances',
  'page-buffer-keeps-filling',
  'page-sees-playback-not-pause',
  'no-unexpected-media-source-reload',
];
/** Имя действия страницы (`hook:attack` от робота). */
const ATTACK_HOOK = 'attack';
/** Буфер наперёд до атаки, с (как у mse-fmp4 в режиме передачи). */
const BUFFER_AHEAD_S = 4;
/** Сколько ждём `updateend`/`error` SourceBuffer-а после одного вредного `appendBuffer`, мс. */
const APPEND_SETTLE_MS = 3000;

/** Ждёт, пока SourceBuffer закончит операцию (или ошибётся, или выйдет срок). */
export async function settleSourceBuffer(sourceBuffer) {
  await new Promise((resolve) => {
    const finish = () => {
      clearTimeout(timer);
      sourceBuffer.removeEventListener('updateend', finish);
      sourceBuffer.removeEventListener('error', finish);
      resolve();
    };
    const timer = setTimeout(finish, APPEND_SETTLE_MS);
    sourceBuffer.addEventListener('updateend', finish);
    sourceBuffer.addEventListener('error', finish);
    if (!sourceBuffer.updating) finish();
  });
}

/**
 * Запускает страницу-атаку.
 * @param {string} scenarioName имя страницы
 * @param {(attack: {ctx: object, session: object, sourceBuffer: SourceBuffer|null}) => Promise<void>} runAttack
 *   сама атака; вызывается по `hook:attack`
 * @param {{worker?: 'module'|'classic'}} [options] `worker` — MSE в воркере (SourceBuffer-а на странице нет)
 */
export async function runAttackPage(scenarioName, runAttack, options = {}) {
  await runScenario(scenarioName, async (ctx) => {
    if (options.worker && !MediaSource.canConstructInDedicatedWorker) {
      throw new UnsupportedError(`MediaSource.canConstructInDedicatedWorker=${MediaSource.canConstructInDedicatedWorker}`);
    }
    const playerOptions = {
      ...(options.worker ? { worker: options.worker } : {}),
      ...(ctx.params.handoff ? { bufferAheadS: BUFFER_AHEAD_S } : {}),
    };
    const session = await startAsset(ctx, 'h264-aac', playerOptions);
    await checkPlaying(ctx, session.video, 'playing');
    if (!ctx.params.handoff) {
      // Без робота (`pages`) атаки нет: настоящий браузер без тени не трогаем.
      await session.player.destroy();
      return;
    }
    const attack = async (hookCtx) => {
      // Собственный движок страницы замолкает: иначе его честные дозагрузки перемешались бы с атакой.
      await session.player.engine?.stop();
      const sourceBuffer = session.player.engine?.lanes?.[0]?.sourceBuffer ?? null;
      let errors = 0;
      try {
        await runAttack({ ctx: hookCtx, session, sourceBuffer });
      } catch (error) {
        errors += 1;
        hookCtx.log(`attack step threw: ${error?.name}: ${error?.message}`);
      }
      hookCtx.metric('attack_errors', errors);
      await sleep(0);
    };
    await observeHandoff(ctx, session, { [ATTACK_HOOK]: attack }, { skipFinishChecks: ATTACK_SKIPPED_FINISH_CHECKS });
    await session.player.destroy();
  });
}
