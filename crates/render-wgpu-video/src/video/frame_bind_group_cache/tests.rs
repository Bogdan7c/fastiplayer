//! Hermetic-тесты кэша bind group-ов и гейта uniform (без GPU).

use std::cell::Cell;
use std::sync::{Arc, Weak};

use super::*;

/// Ключ для тестов: владелец + слот.
fn key(owner: PassBindingId, uniform_slot: usize) -> BindGroupKey {
    BindGroupKey {
        owner,
        uniform_slot,
    }
}

#[test]
fn same_key_is_created_once_then_reused() {
    let cache = BindGroupCache::<u32>::default();
    let owner = PassBindingId::allocate();
    let factory_calls = Cell::new(0_u32);
    let create = || {
        factory_calls.set(factory_calls.get() + 1);
        7
    };

    let (first, first_source) = cache.get_or_create(key(owner, 0), create);
    let (second, second_source) = cache.get_or_create(key(owner, 0), create);

    assert_eq!((first, second), (7, 7));
    assert_eq!(first_source, BindGroupSource::Created);
    assert_eq!(second_source, BindGroupSource::Reused);
    assert_eq!(
        factory_calls.get(),
        1,
        "фабрика вызывается один раз на ключ"
    );
}

#[test]
fn different_slot_or_owner_gets_its_own_entry() {
    let cache = BindGroupCache::<u32>::default();
    let renderer_a = PassBindingId::allocate();
    let renderer_b = PassBindingId::allocate();

    let (main_slot, _) = cache.get_or_create(key(renderer_a, 0), || 10);
    let (overlay_slot, overlay_source) = cache.get_or_create(key(renderer_a, 1), || 11);
    let (other_renderer, other_source) = cache.get_or_create(key(renderer_b, 0), || 12);

    assert_eq!((main_slot, overlay_slot, other_renderer), (10, 11, 12));
    assert_eq!(overlay_source, BindGroupSource::Created);
    assert_eq!(other_source, BindGroupSource::Created);
    // Повторный запрос исходного ключа не затронут соседями.
    assert_eq!(
        cache.get_or_create(key(renderer_a, 0), || 99),
        (10, BindGroupSource::Reused)
    );
}

#[test]
fn capacity_is_bounded_and_evicts_oldest_entry() {
    let cache = BindGroupCache::<usize>::default();
    let owner = PassBindingId::allocate();
    for slot in 0..=FRAME_BIND_GROUP_CACHE_CAPACITY {
        cache.get_or_create(key(owner, slot), || slot);
    }

    assert_eq!(cache.len(), FRAME_BIND_GROUP_CACHE_CAPACITY);
    // Старейшая запись (slot 0) вытеснена, свежая (последняя) на месте.
    let (_, oldest_source) = cache.get_or_create(key(owner, 0), || 0);
    assert_eq!(oldest_source, BindGroupSource::Created);
    let (_, newest_source) = cache.get_or_create(key(owner, FRAME_BIND_GROUP_CACHE_CAPACITY), || 0);
    assert_eq!(newest_source, BindGroupSource::Reused);
}

#[test]
fn dropping_the_cache_owner_releases_cached_values() {
    let probe = Arc::new(());
    let weak_probe: Weak<()> = Arc::downgrade(&probe);
    let cache = BindGroupCache::<Arc<()>>::default();
    cache.get_or_create(key(PassBindingId::allocate(), 0), || probe.clone());
    drop(probe);
    assert!(
        weak_probe.upgrade().is_some(),
        "пока кэш жив, запись держит значение"
    );

    drop(cache);

    assert!(
        weak_probe.upgrade().is_none(),
        "смерть владельца кэша обязана освободить bind group (и через неё views)"
    );
}

#[test]
fn poisoned_cache_falls_back_to_creating_without_panic() {
    let cache = Arc::new(BindGroupCache::<u32>::default());
    let poisoning_cache = Arc::clone(&cache);
    // Поток падает, удерживая лок: лок отравлен.
    let _ = std::thread::spawn(move || {
        let _guard = poisoning_cache.entries.lock();
        panic!("намеренно отравляем лок в тесте");
    })
    .join();
    let owner = PassBindingId::allocate();

    let (first, first_source) = cache.get_or_create(key(owner, 0), || 1);
    let (second, second_source) = cache.get_or_create(key(owner, 0), || 2);

    assert_eq!(
        (first, second),
        (1, 2),
        "без кэша каждый раз создаётся заново"
    );
    assert_eq!(first_source, BindGroupSource::Created);
    assert_eq!(second_source, BindGroupSource::Created);
}

#[test]
fn uniform_gate_skips_identical_bytes_and_writes_changed_ones() {
    let mut gate = UniformWriteGate::default();

    assert_eq!(gate.decide(0, &[1, 2, 3]), UniformWriteDecision::Write);
    assert_eq!(gate.decide(0, &[1, 2, 3]), UniformWriteDecision::Skip);
    assert_eq!(gate.decide(0, &[1, 2, 4]), UniformWriteDecision::Write);
    assert_eq!(gate.decide(0, &[1, 2, 4]), UniformWriteDecision::Skip);
    // Возврат к прежним байтам — это снова изменение относительно последней записи.
    assert_eq!(gate.decide(0, &[1, 2, 3]), UniformWriteDecision::Write);
}

#[test]
fn uniform_gate_slots_are_independent() {
    let mut gate = UniformWriteGate::default();
    assert_eq!(gate.decide(0, &[5]), UniformWriteDecision::Write);

    // Overlay-слот ещё не писали: байты main-слота ему не засчитываются.
    assert_eq!(gate.decide(1, &[5]), UniformWriteDecision::Write);
    assert_eq!(gate.decide(1, &[6]), UniformWriteDecision::Write);
    assert_eq!(gate.decide(0, &[5]), UniformWriteDecision::Skip);
}

#[test]
fn uniform_gate_always_writes_for_unknown_slot() {
    let mut gate = UniformWriteGate::default();
    let unknown_slot = VideoRenderTargetLoad::UNIFORM_SLOT_COUNT;

    assert_eq!(gate.decide(unknown_slot, &[1]), UniformWriteDecision::Write);
    assert_eq!(gate.decide(unknown_slot, &[1]), UniformWriteDecision::Write);
}

#[test]
fn new_pass_binding_starts_empty_so_first_write_is_mandatory() {
    let mut first_instance = PassBinding::new();
    assert_eq!(
        first_instance.plan_uniform_write(0, &[9]),
        UniformWriteDecision::Write
    );
    assert_eq!(
        first_instance.plan_uniform_write(0, &[9]),
        UniformWriteDecision::Skip
    );

    // Пересозданный рендерер: тот же байтовый uniform, но буфер новый — писать надо.
    let mut recreated_instance = PassBinding::new();
    assert_eq!(
        recreated_instance.plan_uniform_write(0, &[9]),
        UniformWriteDecision::Write
    );
}

#[test]
fn pass_binding_counts_creations_reuses_and_uniform_writes() {
    let cache = BindGroupCache::<u32>::default();
    let mut binding = PassBinding::new();

    binding.plan_uniform_write(0, &[1]);
    binding.plan_uniform_write(0, &[1]);
    binding.plan_uniform_write(0, &[1]);
    binding.resolve_bind_group(0, Some(&cache), || 1);
    binding.resolve_bind_group(0, Some(&cache), || 1);
    binding.resolve_bind_group(1, Some(&cache), || 2);

    assert_eq!(
        binding.stats(),
        VideoHotPathStats {
            bind_groups_created: 2,
            bind_groups_reused: 1,
            uniform_writes: 1,
            uniform_writes_skipped: 2,
        }
    );
}

#[test]
fn frame_without_cache_creates_bind_group_every_time_and_touches_no_cache() {
    let mut binding = PassBinding::new();
    let other_cache = BindGroupCache::<u32>::default();

    binding.resolve_bind_group::<u32>(0, None, || 1);
    binding.resolve_bind_group::<u32>(0, None, || 1);

    assert_eq!(binding.stats().bind_groups_created, 2);
    assert_eq!(binding.stats().bind_groups_reused, 0);
    assert_eq!(other_cache.len(), 0, "чужой кэш не затронут");
}

#[test]
fn recreated_renderer_does_not_reuse_entries_of_previous_instance() {
    let cache = BindGroupCache::<u32>::default();
    let mut old_instance = PassBinding::new();
    old_instance.resolve_bind_group(0, Some(&cache), || 1);

    let mut new_instance = PassBinding::new();
    let bind_group = new_instance.resolve_bind_group(0, Some(&cache), || 2);

    assert_eq!(
        bind_group, 2,
        "bind group старого экземпляра (другой uniform/layout) не подходит"
    );
    assert_eq!(new_instance.stats().bind_groups_created, 1);
}

#[test]
fn hot_path_stats_merge_adds_every_counter() {
    let left = VideoHotPathStats {
        bind_groups_created: 1,
        bind_groups_reused: 2,
        uniform_writes: 3,
        uniform_writes_skipped: 4,
    };
    let right = VideoHotPathStats {
        bind_groups_created: 10,
        bind_groups_reused: 20,
        uniform_writes: 30,
        uniform_writes_skipped: 40,
    };

    assert_eq!(
        left.merged_with(right),
        VideoHotPathStats {
            bind_groups_created: 11,
            bind_groups_reused: 22,
            uniform_writes: 33,
            uniform_writes_skipped: 44,
        }
    );
}
