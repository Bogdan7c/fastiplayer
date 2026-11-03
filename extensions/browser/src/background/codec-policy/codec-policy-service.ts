// Владелец политики кодеков в фоне: держит регистрацию курьера в соответствии с настройкой и меткой плеера.
//
// Входы: настройка (`storage.local`, меняется со страницы настроек) и метка плеера (кэш последнего
// `hello_accepted`, пишется через `reportPlayerCheapCodecs`). Любое изменение входа, а также старт фона
// приводят к `resync()`. Пересинхронизации идут строго по очереди — две одновременные не должны гоняться
// за регистрацией.
import type { CodecPolicy, VideoCodecFamily } from "../../shared/codec-policy.ts";
import { CHEAP_CODECS_KEY, CODEC_SETTING_KEY, readCachedCheapCodecs, readCodecSetting, writeCachedCheapCodecs, type KeyValueStore } from "../../shared/codec-storage.ts";
import { syncCarrierRegistration, type CarrierScriptRegistry } from "./carrier-registration.ts";
import { computeEffectivePolicy } from "./effective-policy.ts";

export interface CodecPolicyServiceDeps {
  readonly store: KeyValueStore;
  readonly registry: CarrierScriptRegistry;
  /** Диагностика без содержимого страниц. */
  readonly warn: (message: string) => void;
}

export interface CodecPolicyService {
  /** Пересчитать политику из хранилища и привести регистрацию курьера к ней. Не бросает. */
  resync(): Promise<void>;
  /** Страница настроек или другой контекст изменил эти ключи `storage.local`: нужен пересчёт? */
  isPolicyInputKey(changedKey: string): boolean;
  /** Плеер сообщил свои «дешёвые» кодеки (hello_accepted): кэшируем и пересчитываем. Не бросает. */
  reportPlayerCheapCodecs(families: ReadonlySet<VideoCodecFamily>): Promise<void>;
  /** Политика, применённая последней синхронизацией (`undefined` — ещё не синхронизировали). */
  appliedPolicy(): CodecPolicy | undefined;
}

export function createCodecPolicyService(deps: CodecPolicyServiceDeps): CodecPolicyService {
  let queue: Promise<void> = Promise.resolve();
  let applied: CodecPolicy | undefined;

  async function resyncNow(): Promise<void> {
    try {
      const [setting, cheapCodecs] = await Promise.all([readCodecSetting(deps.store), readCachedCheapCodecs(deps.store)]);
      const policy = computeEffectivePolicy(setting, cheapCodecs);
      await syncCarrierRegistration(deps.registry, policy);
      applied = policy;
    } catch (error) {
      // Не молчим: без регистрации сайты видят родные ответы браузера — безопасный, но заметный сбой.
      deps.warn(`codec policy: синхронизация курьера не удалась: ${String(error)}`);
    }
  }

  const resync = (): Promise<void> => {
    queue = queue.then(resyncNow);
    return queue;
  };

  return {
    resync,
    isPolicyInputKey: (changedKey) => changedKey === CODEC_SETTING_KEY || changedKey === CHEAP_CODECS_KEY,
    async reportPlayerCheapCodecs(families) {
      try {
        const before = await readCachedCheapCodecs(deps.store);
        const reported = [...families];
        // Не пишем в хранилище, если метка не менялась (hello бывает часто).
        if (!sameFamilies(before, reported)) await writeCachedCheapCodecs(deps.store, reported);
      } catch (error) {
        deps.warn(`codec policy: метка плеера не сохранена: ${String(error)}`);
        return;
      }
      await resync();
    },
    appliedPolicy: () => applied,
  };
}

function sameFamilies(left: readonly VideoCodecFamily[], right: readonly VideoCodecFamily[]): boolean {
  return left.length === right.length && right.every((family) => left.includes(family));
}
