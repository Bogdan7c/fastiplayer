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
