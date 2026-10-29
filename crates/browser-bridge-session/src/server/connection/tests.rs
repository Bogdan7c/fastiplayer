use super::*;

#[test]
fn drop_log_is_rate_limited_to_first_and_every_hundredth() {
    let logged: Vec<u64> = (1..=450)
        .filter(|total| should_log_dropped_message(*total))
        .collect();
    assert_eq!(logged, vec![1, 100, 200, 300, 400]);
}
