use super::*;

fn cache(limit: usize) -> ByteLruCache<u32, String> {
    ByteLruCache::new(limit)
}

#[test]
fn hit_returns_the_same_shared_value_and_miss_returns_none() {
    let mut cache = cache(100);
    assert!(cache.get(&1).is_none());
    let stored = Arc::new("one".to_owned());
    cache.insert(1, Arc::clone(&stored), 10);
    let found = cache.get(&1).expect("hit");
    assert!(Arc::ptr_eq(&found, &stored));
    let statistics = cache.statistics();
    assert_eq!((statistics.hits, statistics.misses), (1, 1));
    assert_eq!((statistics.entries, statistics.current_bytes), (1, 10));
}

#[test]
fn overflow_evicts_the_least_recently_used_entry_and_stays_within_limit() {
    let mut cache = cache(30);
    for key in 1..=3 {
        cache.insert(key, Arc::new(key.to_string()), 10);
    }
    // Свежим делаем ключ 1: вытеснен должен быть 2.
    assert!(cache.get(&1).is_some());
    cache.insert(4, Arc::new("4".to_owned()), 10);
    assert!(cache.get(&2).is_none(), "oldest untouched entry must go");
    assert!(cache.get(&1).is_some());
    assert!(cache.get(&3).is_some());
    assert!(cache.get(&4).is_some());
    let statistics = cache.statistics();
    assert_eq!(statistics.evictions, 1);
    assert_eq!(statistics.current_bytes, 30);
    assert_eq!(statistics.entries, 3);
}

#[test]
fn big_entry_evicts_several_small_ones() {
    let mut cache = cache(30);
    for key in 1..=3 {
        cache.insert(key, Arc::new(String::new()), 10);
    }
    cache.insert(9, Arc::new(String::new()), 25);
    let statistics = cache.statistics();
    assert_eq!(statistics.evictions, 3);
    assert_eq!(statistics.current_bytes, 25);
    assert!(statistics.current_bytes <= 30);
}

#[test]
fn entry_larger_than_the_whole_limit_is_not_cached_and_does_not_disturb_others() {
    let mut cache = cache(30);
    cache.insert(1, Arc::new(String::new()), 10);
    cache.insert(2, Arc::new(String::new()), 31);
    assert!(cache.get(&2).is_none());
    assert!(cache.get(&1).is_some());
    let statistics = cache.statistics();
    assert_eq!(statistics.rejected_oversized, 1);
    assert_eq!(statistics.evictions, 0);
    assert_eq!(statistics.current_bytes, 10);
}

#[test]
fn reinserting_a_key_replaces_it_without_double_counting_bytes() {
    let mut cache = cache(100);
    cache.insert(1, Arc::new("a".to_owned()), 40);
    cache.insert(1, Arc::new("b".to_owned()), 25);
    assert_eq!(cache.statistics().current_bytes, 25);
    assert_eq!(cache.statistics().entries, 1);
    assert_eq!(cache.get(&1).as_deref().map(String::as_str), Some("b"));
}

#[test]
fn clear_forgets_entries_but_keeps_counters() {
    let mut cache = cache(100);
    cache.insert(1, Arc::new(String::new()), 40);
    assert!(cache.get(&1).is_some());
    cache.clear();
    assert!(cache.get(&1).is_none());
    let statistics = cache.statistics();
    assert_eq!((statistics.current_bytes, statistics.entries), (0, 0));
    assert_eq!((statistics.hits, statistics.misses), (1, 1));
}

#[test]
fn zero_limit_caches_nothing() {
    let mut cache = cache(0);
    cache.insert(1, Arc::new(String::new()), 1);
    assert!(cache.get(&1).is_none());
    assert_eq!(cache.statistics().rejected_oversized, 1);
}

#[test]
fn deferred_inserts_may_exceed_the_limit_until_cut_trims_the_oldest() {
    let mut cache = cache(30);
    for key in 1..=5 {
        cache.insert_deferred(key, Arc::new(key.to_string()), 10);
    }
    // Вставка не вытесняет: кэш временно больше лимита.
    assert_eq!(cache.statistics().current_bytes, 50);
    assert_eq!(cache.statistics().evictions, 0);
    // Недавно использованная запись переживает вытеснение.
    assert!(cache.get(&1).is_some());
    cache.cut();
    let statistics = cache.statistics();
    assert_eq!((statistics.current_bytes, statistics.evictions), (30, 2));
    assert!(cache.get(&1).is_some());
    assert!(cache.get(&2).is_none());
    assert!(cache.get(&3).is_none());
    assert!(cache.get(&4).is_some());
    assert!(cache.get(&5).is_some());
}

#[test]
fn probation_entries_are_evicted_before_used_ones_and_hits_promote_them() {
    let mut cache = cache(30);
    cache.insert(1, Arc::new("used".to_owned()), 10);
    cache.insert(2, Arc::new("used".to_owned()), 10);
    // Поток «одноразовых» записей не вытесняет проверенные.
    cache.insert_on_probation(10, Arc::new("a".to_owned()), 10);
    cache.insert_on_probation(11, Arc::new("b".to_owned()), 10);
    assert!(
        cache.get(&10).is_none(),
        "the older probation entry went first"
    );
    assert!(cache.get(&1).is_some());
    assert!(cache.get(&2).is_some());
    assert!(cache.get(&11).is_some(), "the hit promotes the entry");
    // Теперь 11 - обычная запись; новая проба вытесняет самую старую обычную (1), а не себя.
    cache.insert_on_probation(12, Arc::new("c".to_owned()), 10);
    assert!(cache.get(&1).is_none());
    assert!(cache.get(&2).is_some());
    assert!(cache.get(&11).is_some());
    assert!(cache.get(&12).is_some());
    assert!(cache.statistics().current_bytes <= 30);
}

#[test]
fn many_hits_on_few_entries_do_not_grow_the_recency_queues_without_bound() {
    let mut cache = cache(1000);
    for key in 1..=3 {
        cache.insert(key, Arc::new(key.to_string()), 10);
    }
    for round in 0..10_000_u32 {
        assert!(cache.get(&(round % 3 + 1)).is_some());
    }
    // 3 записи: очередь убирает устаревшие элементы и остаётся в пределах запаса.
    let allowed = 3 * QUEUE_SLACK_FACTOR + QUEUE_SLACK_BASE;
    assert!(
        cache.warm.len() <= allowed + 1,
        "queue: {}",
        cache.warm.len()
    );
    // Порядок вытеснения при уборке очереди не страдает: ключ 1 использован 1000 раз подряд
    // и самый свежий, поэтому при нехватке места уходит ключ 2.
    let mut small = self::cache(20);
    small.insert(1, Arc::new(String::new()), 10);
    small.insert(2, Arc::new(String::new()), 10);
    for _ in 0..1000 {
        assert!(small.get(&1).is_some());
    }
    small.insert(3, Arc::new(String::new()), 10);
    assert!(small.get(&2).is_none(), "2 is the least recently used");
    assert!(small.get(&1).is_some());
    assert!(small.get(&3).is_some());
}

#[test]
fn reused_slot_of_an_evicted_entry_is_not_evicted_by_its_stale_ticket() {
    let mut cache = cache(20);
    cache.insert(1, Arc::new("one".to_owned()), 10);
    cache.insert(2, Arc::new("two".to_owned()), 10);
    // 1 вытесняется, его ячейка достаётся ключу 3; устаревший элемент очереди ключа 1 ещё жив
    // только если не был снят - он снят при вытеснении, но проверим и обновление через get.
    assert!(cache.get(&1).is_some());
    assert!(cache.get(&2).is_some());
    cache.insert(3, Arc::new("three".to_owned()), 10);
    assert!(cache.get(&1).is_none(), "1 was the older of the two");
    cache.insert(4, Arc::new("four".to_owned()), 10);
    // Теперь вытесняется 2 (старше 3); запись 3 с переиспользованной ячейкой цела.
    assert!(cache.get(&2).is_none());
    assert_eq!(cache.get(&3).as_deref().map(String::as_str), Some("three"));
    assert_eq!(cache.get(&4).as_deref().map(String::as_str), Some("four"));
    assert_eq!(cache.statistics().current_bytes, 20);
}

#[test]
fn replacing_a_key_leaves_one_live_entry_and_correct_eviction_order() {
    let mut cache = cache(20);
    cache.insert(1, Arc::new("old".to_owned()), 10);
    cache.insert(2, Arc::new("two".to_owned()), 10);
    // Замена ключа 1: он становится самым свежим, его прежний элемент очереди устарел.
    cache.insert(1, Arc::new("new".to_owned()), 10);
    cache.insert(3, Arc::new("three".to_owned()), 10);
    assert!(cache.get(&2).is_none(), "2 is now the oldest");
    assert_eq!(cache.get(&1).as_deref().map(String::as_str), Some("new"));
    assert_eq!(cache.statistics().entries, 2);
}

#[test]
fn stale_ticket_of_a_replaced_key_does_not_evict_the_newer_entry_in_the_reused_slot() {
    let mut cache = cache(20);
    cache.insert(1, Arc::new("one".to_owned()), 10);
    cache.insert(2, Arc::new("two".to_owned()), 10);
    // Использование оставляет в очереди старые элементы ключа 1 (они устаревают).
    assert!(cache.get(&1).is_some());
    // Замена ключа 1: ячейка освобождается и тут же достаётся новой записи с новой отметкой,
    // а устаревшие элементы очереди продолжают указывать на эту ячейку.
    cache.insert(1, Arc::new("one again".to_owned()), 10);
    // Порядок давности: 2 старее, чем заново вставленный 1.
    cache.insert(3, Arc::new("three".to_owned()), 10);
    assert!(
        cache.get(&2).is_none(),
        "2 is the oldest; a stale ticket must not evict key 1 instead"
    );
    assert_eq!(
        cache.get(&1).as_deref().map(String::as_str),
        Some("one again")
    );
    assert_eq!(cache.statistics().evictions, 1);
}
