# S42 Wave C3 — playlist runtime shell boundary decomposition (2026-08-27)

## Архитектура и ownership

- Изменение behavior-neutral: `crates/app-egui/src/playlist_runtime.rs` остаётся владельцем данных `PlaylistRuntime`, constructors, load/install gate, settings orchestration и полного media-open protocol-а.
- Новый приватный child `crates/app-egui/src/playlist_runtime/shell_boundary.rs` владеет только shell-facing lifecycle vocabulary и orchestration: lifecycle/binding generations, exact `PlaylistRuntimeBinding`, renderer attachment/view projection, owner mailbox/admission, bounded shutdown report и соответствующие inherent methods `PlaylistRuntime`.
- Старые crate paths `crate::playlist_runtime::{PlaylistRuntimeBinding, PlaylistTerminalShutdownOutcome, ...}` сохранены re-export-ами parent-а. Новых public/crate-wide API нет.
- Поля generation и owner ports имеют `pub(super)` ровно для прежней visibility области: когда определения жили в parent-е, их private fields уже были доступны parent-у и sibling descendants. Это не расширяет visibility за `playlist_runtime`.
- `controller`, `media_reset`, `prepared_next`, `persistence`, `suspend_resume`, `settings` и S42 baseline не изменялись.

## Инварианты

- Binding/lifecycle generations увеличиваются в прежнем exact порядке; stale/suspended/shutting-down outcomes не слиты.
- Suspend сохраняет process owners/load gate, отменяет preload и suspension media-open binding в прежнем порядке.
- Mailbox drain сохраняет прежний owner → media-open → dialogs/import/url/export порядок; shutdown сначала закрывает admission, затем завершает owners под одним absolute deadline.
- `PlaylistShutdownReport::requires_process_exit` по-прежнему отличает live/timed-out owner от завершившейся terminal failure; idempotent shutdown остаётся `AlreadyCompleted`.
- Media-open construction, staging/authorization/Installed/cancel/supersede/terminal ownership остались в parent-е без изменения.

## Тестовая topology и проверки

- Бывшие inline tests перенесены без изменения имён в `crates/app-egui/src/playlist_runtime/tests.rs`; test paths остаются `playlist_runtime::tests::*`.
- Production sizes: parent `playlist_runtime.rs` 648 lines, child `shell_boundary.rs` 444 lines; test child 166 lines. Предыдущий checked-in S42 baseline для parent-а равен 1196 и намеренно не обновлён.
- Focused PASS: shell runtime 5/5; exact controller Ready→authorization→Installed commit; installed-media suspend/resume; renderer Presented submit accounting.
- Full PASS: `app-egui` no-default 1002/1002 и all-features 1002/1002; strict no-default/all-features all-target Clippy; no-default check; rustfmt; diff check; refactor guardrails; S42 final acceptance 24/24; Serena diagnostics clean для parent/production child/test child.
- `scripts/check_s42_guardrails.py` остаётся ожидаемо red из-за repository-wide stale/legacy snapshot inventory; затронутый parent сообщает только ожидаемое уменьшение baseline 1196→648. `scripts/module-size-baseline.json` не менялся.