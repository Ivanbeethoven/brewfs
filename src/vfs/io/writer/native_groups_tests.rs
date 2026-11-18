use super::Groups;
use std::time::{Duration, Instant};

fn at(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
}

fn normalize(mut groups: Vec<Vec<i32>>) -> Vec<Vec<i32>> {
    for group in &mut groups {
        group.sort_unstable();
    }
    groups.sort_unstable();
    groups
}

#[test]
fn zero_window_emits_each_push_as_a_singleton() {
    let start = Instant::now();
    let mut groups = Groups::new(Duration::ZERO);

    assert_eq!(groups.push(1, start, 10), vec![vec![10]]);
    assert_eq!(groups.next_deadline(), None);

    assert_eq!(groups.push(1, at(start, 1), 11), vec![vec![11]]);
    assert_eq!(groups.next_deadline(), None);
    assert!(groups.seal_due(at(start, 100)).is_empty());
    assert_eq!(groups.seal_all(), Vec::<Vec<i32>>::new());
}

#[test]
fn same_key_pushes_do_not_extend_the_first_deadline() {
    let start = Instant::now();
    let window = Duration::from_secs(10);
    let deadline = start + window;
    let mut groups = Groups::new(window);

    assert!(groups.push(7, start, 1).is_empty());
    assert_eq!(groups.next_deadline(), Some(deadline));

    assert!(groups.push(7, at(start, 5_000), 2).is_empty());
    assert_eq!(groups.next_deadline(), Some(deadline));

    assert!(
        groups
            .seal_due(deadline - Duration::from_nanos(1))
            .is_empty()
    );
    assert_eq!(groups.seal_due(deadline), vec![vec![1, 2]]);
    assert_eq!(groups.next_deadline(), None);
}

#[test]
fn thirty_second_value_seals_the_group_immediately() {
    let start = Instant::now();
    let mut groups = Groups::new(Duration::from_secs(60));

    for value in 0..31 {
        assert!(groups.push(4, at(start, value as u64), value).is_empty());
    }

    let emitted = groups.push(4, at(start, 31), 31);
    let expected: Vec<i32> = (0..32).collect();
    assert_eq!(emitted, vec![expected]);
    assert_eq!(groups.next_deadline(), None);

    assert!(groups.push(4, at(start, 32), 32).is_empty());
    assert_eq!(
        groups.next_deadline(),
        Some(at(start, 32) + Duration::from_secs(60))
    );
}

#[test]
fn push_at_deadline_seals_old_group_before_opening_new_one() {
    let start = Instant::now();
    let window = Duration::from_secs(3);
    let deadline = start + window;
    let mut groups = Groups::new(window);

    assert!(groups.push(2, start, 100).is_empty());

    let emitted = groups.push(2, deadline, 200);
    assert_eq!(emitted, vec![vec![100]]);
    assert_eq!(groups.next_deadline(), Some(deadline + window));

    assert!(groups.seal_due(deadline).is_empty());
    assert_eq!(groups.seal_all(), vec![vec![200]]);
    assert_eq!(groups.next_deadline(), None);
}

#[test]
fn different_keys_remain_separate_and_seal_all_flushes_every_group() {
    let start = Instant::now();
    let mut groups = Groups::new(Duration::from_secs(20));

    assert!(groups.push(1, start, 10).is_empty());
    assert!(groups.push(2, at(start, 1), 20).is_empty());
    assert!(groups.push(1, at(start, 2), 11).is_empty());

    assert_eq!(normalize(groups.seal_all()), vec![vec![10, 11], vec![20]]);
    assert_eq!(groups.next_deadline(), None);
    assert!(groups.seal_all().is_empty());
}

#[test]
fn independent_early_seals_start_fresh_per_key_windows() {
    let start = Instant::now();
    let window = Duration::from_secs(10);
    let mut groups = Groups::new(window);

    for value in 0..31 {
        assert!(groups.push(1, start, value).is_empty());
    }
    let first_a: Vec<i32> = (0..32).collect();
    assert_eq!(groups.push(1, start, 31), vec![first_a]);

    for value in 100..131 {
        assert!(groups.push(2, start, value).is_empty());
    }
    let first_b: Vec<i32> = (100..132).collect();
    assert_eq!(groups.push(2, start, 131), vec![first_b]);
    assert_eq!(groups.next_deadline(), None);

    let a_start = at(start, 1_000);
    let b_start = at(start, 2_000);
    assert!(groups.push(1, a_start, 200).is_empty());
    assert_eq!(groups.next_deadline(), Some(a_start + window));

    assert!(groups.push(2, b_start, 300).is_empty());
    assert_eq!(groups.next_deadline(), Some(a_start + window));

    assert_eq!(groups.seal_due(a_start + window), vec![vec![200]]);
    assert_eq!(groups.next_deadline(), Some(b_start + window));

    let a_second_start = a_start + window;
    assert!(groups.push(1, a_second_start, 201).is_empty());
    assert_eq!(groups.next_deadline(), Some(b_start + window));

    assert_eq!(groups.seal_due(b_start + window), vec![vec![300]]);
    assert_eq!(groups.next_deadline(), Some(a_second_start + window));

    assert_eq!(groups.seal_due(a_second_start + window), vec![vec![201]]);
    assert_eq!(groups.next_deadline(), None);
}
