# A9b: Undo удаления после автоперехода

Статус: PASS.

## Решение владельца
Без shuffle Undo возвращает только удалённую строку на прежнее место; указатель «что играет» берётся из текущей очереди. С shuffle, при удалённой играющей строке и для Clear прежнее поведение: смена текущего сбрасывает Undo. Структурные изменения (добавление, перемещение, сортировка, повторное удаление) сбрасывают Undo по-прежнему.

## Корень
Undo сбрасывали две независимые вещи: (1) проверка смены active lineage; (2) `commit_dirty` при Installed двигал `undo_blocking_revision`, хотя Installed без structural-изменения меняет только указатель current. Плюс restore целиком возвращал current и shuffle-историю из snapshot, откатив бы указатель на уже не играющий трек.

## Что сделано
- playlist-core: `PlaylistQueue::restore_removed_rows_keeping_current` возвращает только строки, current остаётся из очереди (хранится по id строки, сдвиг индексов безопасен). Включённый shuffle отвергается (`RemovalSnapshotRestoreError::ShuffleActive`). Общая логика restore вынесена в приватный `restore_removal_snapshot_from`; прежний `restore_removal_snapshot` не изменился.
- app-egui, новый модуль `controller/removal/current_pointer.rs`: `undo_survives_current_change` (shuffle выключен и удалены только неиграющие строки), `restore_queue_for_undo`, `commit_install_dirty` (Installed без structural-изменения и без shuffle не двигает `undo_blocking_revision`; persistence получает сигнал как обычно).
- `removal_undo.rs`: проверка lineage в `availability` и при Installed учитывает `survives_current_change`.
- Формат persistence не менялся; `controller.rs` не трогался.

## Тесты
app-egui `removal_undo/tests/current_change.rs`: Undo после автоперехода возвращает строку, играет прежний трек без перезапуска; строка перед текущим и сдвиг индекса (указатель на том же треке); shuffle: Undo недоступен; удалена играющая: недоступен; без автоперехода: как раньше; вернувшаяся строка участвует в переходе (после B идёт восстановленная C, а не D); структурное изменение после автоперехода сбрасывает Undo. playlist-core: возврат строк при ушедшем вперёд current, отказ при shuffle без изменения очереди, отказ после чужого structural-изменения. Проверена мутация (отключение новой политики роняет 4 теста).

## Проверки
`cargo test -p app-egui -p playlist-core --locked` PASS; `cargo fmt --all --check` PASS; `cargo clippy -p app-egui -p playlist-core --all-targets --locked -- -D warnings` PASS; `scripts/check-refactor-guardrails.py` OK. Полный pre-pr не запускался.

## Ручная приёмка
1. Shuffle выключен, играет трек A; удалить B (не играющий); дождаться автоперехода на C; Отменить в пределах 8 с: B вернулся между A и C, C продолжает играть без перезапуска.
2. То же, но удалить строку выше играющей: после Отменить играет тот же трек.
3. Включить shuffle, удалить, дождаться перехода: Отменить пропала.
4. Удалить играющую строку, дождаться перехода: Отменить пропала.

## Риски
Undo теперь переживает и ручной выбор другой строки (без shuffle, при удалении неиграющих): это тот же случай «смена текущего без структурных изменений». Если владелец не хочет этого, сузить условие в `commit_install_dirty`.

## Memories
Обновлено `app-egui/playlist-controller-s12a`.
