// Tests for physical mui removal (StarCastRib::remove_mui), the in-memory
// reclamation path used to stop dead (e.g. synthesized BMP) muis from leaking
// record slots. Exercised against the MemoryOnly strategy, which is what the
// remover targets (it does not touch the on-disk persist tree).

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use inetnum::addr::Prefix;
    use rotonda_store::{
        epoch,
        match_options::{IncludeHistory, MatchOptions, MatchType},
        prefix_record::{Record, RouteStatus},
        rib::{config::MemoryOnlyConfig, StarCastRib},
        test_types::PrefixAs,
    };

    fn exact_match() -> MatchOptions {
        MatchOptions {
            match_type: MatchType::ExactMatch,
            include_withdrawn: false,
            include_less_specifics: false,
            include_more_specifics: false,
            mui: None,
            include_history: IncludeHistory::None,
        }
    }

    fn active(mui: u32) -> Record<PrefixAs> {
        Record::new(mui, 0, RouteStatus::Active, PrefixAs::new_from_u32(mui))
    }

    #[test]
    fn remove_mui_reclaims_records_and_keeps_counts_in_sync(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let store = StarCastRib::<PrefixAs, MemoryOnlyConfig>::try_default()?;

        // mui 1 announces all three prefixes; mui 2 announces only the first
        // two. So `only_mui_1` (p_c) is the single prefix that empties out when
        // mui 1 is removed; p_a and p_b survive (still held by mui 2).
        let p_a = Prefix::from_str("10.0.0.0/8")?;
        let p_b = Prefix::from_str("10.1.0.0/16")?;
        let only_mui_1 = Prefix::from_str("10.2.0.0/16")?;

        store.insert(&p_a, active(1), None)?;
        store.insert(&p_b, active(1), None)?;
        store.insert(&only_mui_1, active(1), None)?;
        store.insert(&p_a, active(2), None)?;
        store.insert(&p_b, active(2), None)?;

        // 3 unique prefixes, 5 (prefix, mui) routes.
        assert_eq!(store.prefixes_count().total(), 3);
        assert_eq!(store.prefixes_count().in_memory(), 3);
        assert_eq!(store.routes_count().total(), 5);
        assert_eq!(store.routes_count().in_memory(), 5);

        // Mirror the real teardown order: a dead mui is marked withdrawn first,
        // then physically removed. remove_mui must still find its records.
        store.mark_mui_as_withdrawn_v4(1)?;
        assert!(store.mui_is_withdrawn_v4(1));

        let (records_removed, prefixes_emptied) = store.remove_mui(1)?;
        assert_eq!(records_removed, 3, "one mui-1 record per prefix");
        assert_eq!(prefixes_emptied, 1, "only p_c was mui-1-only");

        // The mui is gone, so it must no longer linger in the withdrawn index
        // (otherwise a reused id would be wrongly treated as withdrawn).
        assert!(!store.mui_is_withdrawn_v4(1));

        // Both counter instances (family-level `total` and prefix-CHT
        // `in_memory`) must track the removal identically — no drift.
        assert_eq!(store.routes_count().total(), 2);
        assert_eq!(store.routes_count().in_memory(), 2);
        assert_eq!(store.prefixes_count().total(), 2);
        assert_eq!(store.prefixes_count().in_memory(), 2);

        let guard = &epoch::pin();

        // The emptied prefix is gone from the tree entirely.
        let res = store.match_prefix(&only_mui_1, &exact_match(), guard)?;
        assert!(
            res.records.is_empty(),
            "p_c should have no records after removal"
        );
        assert!(!store
            .prefixes_iter(guard)
            .filter_map(|r| r.ok())
            .any(|r| r.prefix == only_mui_1));

        // No mui-1 record survives anywhere (even asking for withdrawn ones).
        assert!(store
            .iter_records_for_mui_v4(1, true, guard)
            .next()
            .is_none());

        // The shared prefix keeps its other mui untouched.
        let res = store.match_prefix(&p_a, &exact_match(), guard)?;
        assert_eq!(res.records.len(), 1);
        assert_eq!(res.records[0].multi_uniq_id, 2);

        // The id is fully reusable: re-inserting mui 1 works and is countable.
        store.insert(&only_mui_1, active(1), None)?;
        let res = store.match_prefix(&only_mui_1, &exact_match(), guard)?;
        assert_eq!(res.records.len(), 1);
        assert_eq!(res.records[0].multi_uniq_id, 1);
        assert_eq!(store.prefixes_count().total(), 3);
        assert_eq!(store.routes_count().total(), 3);

        Ok(())
    }

    // Removing a mui that the store never saw must be a harmless no-op.
    #[test]
    fn remove_unknown_mui_is_noop() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = StarCastRib::<PrefixAs, MemoryOnlyConfig>::try_default()?;
        store.insert(&Prefix::from_str("192.0.2.0/24")?, active(1), None)?;

        let (removed, emptied) = store.remove_mui(999)?;
        assert_eq!((removed, emptied), (0, 0));
        assert_eq!(store.routes_count().total(), 1);
        assert_eq!(store.prefixes_count().total(), 1);
        Ok(())
    }
}

#[test]
fn batch_removal_covers_defaults_both_families_and_is_idempotent() {
    use rotonda_store::{
        prefix_record::{Record, RouteStatus},
        rib::{config::MemoryOnlyConfig, StarCastRib},
        test_types::PrefixAs,
    };
    let store =
        StarCastRib::<PrefixAs, MemoryOnlyConfig>::try_default().unwrap();
    let prefixes: Vec<inetnum::addr::Prefix> =
        ["0.0.0.0/0", "10.1.0.0/16", "::/0", "2001:db8::/32"]
            .iter()
            .map(|p| p.parse().unwrap())
            .collect();
    for p in &prefixes {
        for mui in [1, 2, 3] {
            store
                .insert(
                    p,
                    Record::new(
                        mui,
                        0,
                        RouteStatus::Active,
                        PrefixAs::new_from_u32(mui),
                    ),
                    None,
                )
                .unwrap();
        }
    }
    assert_eq!(store.remove_muis(&[1, 2, 1]).unwrap(), (8, 0));
    assert_eq!(store.remove_muis(&[1, 2]).unwrap(), (0, 0));
    assert_eq!(store.routes_count().total(), 4);
    assert_eq!(store.routes_count().in_memory(), 4);
    assert_eq!(store.remove_muis(&[3]).unwrap(), (4, 4));
    assert_eq!(store.remove_muis(&[3]).unwrap(), (0, 0));
    assert_eq!(store.prefixes_count().total(), 0);
    assert_eq!(store.prefixes_count().in_memory(), 0);
    for p in &prefixes {
        store
            .insert(
                p,
                Record::new(
                    4,
                    0,
                    RouteStatus::Active,
                    PrefixAs::new_from_u32(4),
                ),
                None,
            )
            .unwrap();
    }
    assert_eq!(store.prefixes_count().total(), 4);
    assert_eq!(store.prefixes_count().in_memory(), 4);
}

#[test]
fn concurrent_delete_and_insert_never_hides_survivor_or_drifts_counts() {
    use rotonda_store::{
        prefix_record::{Record, RouteStatus},
        rib::{config::MemoryOnlyConfig, StarCastRib},
        test_types::PrefixAs,
    };
    use std::sync::{Arc, Barrier};
    let store = Arc::new(
        StarCastRib::<PrefixAs, MemoryOnlyConfig>::try_default().unwrap(),
    );
    for round in 0..256u32 {
        let p: inetnum::addr::Prefix =
            format!("10.{}.{}.0/24", round / 256, round % 256)
                .parse()
                .unwrap();
        store
            .insert(
                &p,
                Record::new(
                    1,
                    0,
                    RouteStatus::Active,
                    PrefixAs::new_from_u32(1),
                ),
                None,
            )
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let b = barrier.clone();
            let s = &store;
            scope.spawn(move || {
                b.wait();
                s.remove_muis_for_prefix(&p, &[1].into_iter().collect())
                    .unwrap();
            });
            barrier.wait();
            store
                .insert(
                    &p,
                    Record::new(
                        2,
                        0,
                        RouteStatus::Active,
                        PrefixAs::new_from_u32(2),
                    ),
                    None,
                )
                .unwrap();
        });
        assert!(store.prefixes_keys_iter().any(|key| key == p));
        let records = store
            .get_records_for_prefix(&p, None, true)
            .unwrap()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].multi_uniq_id, 2);
        assert_eq!(store.routes_count().total(), (round + 1) as usize);
        assert_eq!(store.routes_count().in_memory(), (round + 1) as usize);
        assert_eq!(store.prefixes_count().total(), (round + 1) as usize);
        assert_eq!(store.prefixes_count().in_memory(), (round + 1) as usize);
    }
}

#[test]
fn concurrent_bitmap_writers_preserve_every_operation() {
    use rotonda_store::{
        rib::{config::MemoryOnlyConfig, StarCastRib},
        test_types::PrefixAs,
    };
    let store =
        StarCastRib::<PrefixAs, MemoryOnlyConfig>::try_default().unwrap();
    std::thread::scope(|scope| {
        for mui in 1..65 {
            let store = &store;
            scope.spawn(move || {
                store.mark_mui_as_withdrawn(mui).unwrap();
            });
        }
    });
    for mui in 1..65 {
        assert!(store.mui_is_withdrawn_v4(mui));
        assert!(store.mui_is_withdrawn_v6(mui));
    }
    std::thread::scope(|scope| {
        for mui in 1..65 {
            let store = &store;
            scope.spawn(move || {
                if mui % 2 == 0 {
                    store.mark_mui_as_active_v4(mui).unwrap();
                } else {
                    store.mark_mui_as_withdrawn_v4(mui + 100).unwrap();
                }
            });
        }
    });
    for mui in 1..65 {
        assert_eq!(store.mui_is_withdrawn_v4(mui), mui % 2 == 1);
        assert!(store.mui_is_withdrawn_v6(mui));
        if mui % 2 == 1 {
            assert!(store.mui_is_withdrawn_v4(mui + 100));
        }
    }
}

#[test]
#[ignore = "manual performance comparison; run with --release --ignored --nocapture"]
fn batch_reclamation_workload() {
    use rotonda_store::{
        prefix_record::{Record, RouteStatus},
        rib::{config::MemoryOnlyConfig, StarCastRib},
        test_types::PrefixAs,
    };
    use std::time::Instant;
    fn load() -> StarCastRib<PrefixAs, MemoryOnlyConfig> {
        let store = StarCastRib::try_default().unwrap();
        for n in 0..2048u32 {
            let p: inetnum::addr::Prefix =
                format!("10.{}.{}.0/24", n / 256, n % 256).parse().unwrap();
            for mui in 0..128 {
                store
                    .insert(
                        &p,
                        Record::new(
                            mui,
                            0,
                            RouteStatus::Active,
                            PrefixAs::new_from_u32(mui),
                        ),
                        None,
                    )
                    .unwrap();
            }
        }
        store
    }
    let single = load();
    let batch = load();
    let start = Instant::now();
    for mui in 0..127 {
        single.remove_mui(mui).unwrap();
    }
    let repeated = start.elapsed();
    let start = Instant::now();
    assert_eq!(
        batch.remove_muis(&(0..127).collect::<Vec<_>>()).unwrap(),
        (2048 * 127, 0)
    );
    let batched = start.elapsed();
    assert_eq!(batch.routes_count().total(), single.routes_count().total());
    println!("2048 prefixes, 128 peers: repeated single-ID scans {repeated:?}; one batch {batched:?}");
}
