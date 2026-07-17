# Переиспользуемые UI-области и сохранение UX

Эти правила применяются к любым будущим сущностям внутри sidebar, inspector, drawer, dock, overlay, bottom sheet, tool panel и других составных UI-областей — не только к текущим Playlist/Settings/URL/Info.

## Host и сущность

- Сначала определить, является новая вещь новой **областью** окна или новой **сущностью внутри существующей области**. Если меняется только назначение/контент при той же геометрии и UX, это новая сущность существующего host, а не новый Panel/Window/Area.
- Host единолично владеет геометрией и UX-инвариантами: egui container, stable host ID, rect, live width/height, resize state, persisted size policy, min/max range, open/close, displacement video viewport, clipping, z-order, animation и repaint.
- Toolkit remembered-state не должен становиться вторым владельцем размера. Если container внутренне persist-ит geometry, host обязан нейтрализовать/синхронизировать её так, чтобы authoritative live state оставался один.
- Сущность владеет только своим domain/UI state, read-only model/snapshot, content-specific scroll/focus IDs и typed actions. Сущности запрещено создавать container того же уровня, хранить копию host width/open state или менять viewport displacement.
- Добавление сущности должно расширять typed section/entity enum и единый content renderer/registry. Нельзя добавлять новый `Panel::left`, `Window`, `Area` или section-derived host ID ради нового содержимого.

## Stable ID и размер

- У одной визуальной области ровно один stable host ID на всех сущностях и animation phases. Dynamic ID из title/section создаёт независимое persisted state и считается архитектурной ошибкой.
- Общий пользовательский размер сохраняется при переключении сущностей, close/reopen и process restart. Min/max/default policy задаётся один раз владельцем config/host и одинакова для всех сущностей, если отдельное UX-решение явно не согласовано.
- Live resize передаётся наружу только typed output/event boundary. Внешний код не читает внутренние поля host; animation/intermediate clipped size не считается пользовательским resize. Geometry читается из собственного rect container-а до render content, а не из content-dependent response rect: translated/clipped children могут расширять или сжимать response во время transition. После render host обязан сохранить свой rect как occupied area parent layout, чтобы toolkit cursor и persisted container state не зависели от видимого содержимого.
- Persisted resize обязан иметь явные debounce, wake scheduling, forced lifecycle flush и rollback-to-committed при persistence failure.
- Контент не должен определять размер host. Длинные строки, metadata, списки и ошибки обязаны wrap/scroll/clip внутри available rect.
- Fixed `UiBuilder::max_rect` допустим для clipped animation copies. В fully-open resizable состоянии контент рендерится прямо в host UI; fixed child rect там может превратить текущий размер в content minimum и заблокировать resize handle.

## Lifecycle и input

- Переключение сущности не должно неявно уничтожать её draft, selection, validation, preview или domain lifecycle. Hide, Cancel, Apply, OK и fatal error остаются разными typed outcomes.
- Outgoing/incoming animation copies получают разные stable content/ScrollArea IDs, общий clip rect и не принимают input до завершения перехода.
- Активная open/close/content animation явно запрашивает repaint, включая paused playback.
- Host-level close скрывает область. Если конкретная сущность требует rollback (например Settings Cancel), это отдельный explicit entity action, а не общая семантика host close.

## UX parity для расширений

- Новая сущность обязана наследовать существующие: положение, размер и resize handle; способ вытеснения/перекрытия соседнего контента; скорость/easing анимации; close/reopen; titlebar hit-testing; одновременную видимость независимых областей; remembered size.
- Нельзя исправлять layout сущности изменением host geometry, если причина находится в content layout.
- Если новая сущность действительно требует другой геометрии, поведения resize или lifecycle области, это важное архитектурное/UX-решение: остановиться и согласовать создание отдельного host с пользователем.

## Обязательные regression tests

- Guardrail: у переиспользуемой области ровно один site создания host container и отсутствуют entity-specific host IDs.
- Переключение каждой пары сущностей сохраняет тот же host rect/remembered size и не создаёт второй container.
- Реальный headless resize работает после открытия каждой сущности и после нескольких переключений; animation size не публикует resize event.
- Persist debounce coalesces latest value, same rounded value не пишет повторно, failure восстанавливает committed host size, suspend/shutdown force-flush pending.
- Длинный content не увеличивает host size; применяется wrap/scroll/clip.
- Open/close/content transition сохраняют direction, duration, clip, repaint и input exclusion.
- Проверяются разные lifecycle semantics сущностей: hide сохраняет state, explicit Cancel откатывает, Apply/OK/error не смешиваются.
- Изменение host architecture требует обновить эту memory и focused UI/layout guardrails.

Текущая реализация и конкретные sidebar-инварианты: `mem:app-egui/sidebar-controller`. Painter boundary: `mem:app-egui/artwork-boundary`.