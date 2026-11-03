// Ячейка с действующей политикой документа. Единственный владелец значения: переопределения читают
// её при каждом вызове страницы, рукопожатие с курьером записывает один раз, воркерная обёртка
// (пакет C) читает синхронно через `current()`.
import { TRANSPARENT_POLICY, type CodecPolicy } from "../../shared/codec-policy.ts";

export interface PolicyCell {
  /** Политика сейчас; до ответа курьера — прозрачная (HLS-отказ от неё не зависит). */
  current(): CodecPolicy;
  /** Принять политику от курьера. Принимается только один раз; `false` — уже принята ранее. */
  adoptFromCarrier(policy: CodecPolicy): boolean;
}

export function createPolicyCell(): PolicyCell {
  let policy: CodecPolicy = TRANSPARENT_POLICY;
  let adopted = false;
  return {
    current: () => policy,
    adoptFromCarrier(carrierPolicy) {
      if (adopted) return false;
      adopted = true;
      policy = carrierPolicy;
      return true;
    },
  };
}
