use serde_json::json;
use syntra_bench::{
    arrival_indices, arrival_offset, mixed_comparisons, production_guard, LatencyHistogram, Options,
};

#[test]
fn production_guard_rejects_assertions_debug_rust_and_reduced_durability() {
    let settings = json!({ "debug_assertions": "off", "fsync": "on", "synchronous_commit": "on", "full_page_writes": "on" });
    assert!(production_guard(&settings, "release", true).is_ok());
    for (key, value) in [
        ("debug_assertions", "on"),
        ("fsync", "off"),
        ("synchronous_commit", "off"),
        ("full_page_writes", "off"),
    ] {
        let mut bad = settings.clone();
        bad[key] = json!(value);
        assert!(production_guard(&bad, "release", true).is_err());
    }
    assert!(production_guard(&settings, "debug", true).is_err());
    assert!(production_guard(&settings, "release", false).is_err());
}

#[test]
fn validates_bounded_mixed_options() {
    for tail in [
        vec!["--mixed-seconds", "1"],
        vec!["--mixed-seconds", "31"],
        vec!["--mixed-rounds", "0"],
        vec!["--oltp-clients", "17"],
        vec!["--olap-clients", "0"],
        vec!["--offered-rate", "100001"],
        vec!["--mixed-seconds", "2", "--rows", "1"],
    ] {
        assert!(Options::parse(
            [vec!["--pg-bin", "/pg/bin"], tail]
                .concat()
                .into_iter()
                .map(String::from)
        )
        .is_err());
    }
    let good = Options::parse(
        [
            "--pg-bin",
            "/pg/bin",
            "--mixed-seconds",
            "3",
            "--require-production",
            "true",
        ]
        .map(String::from),
    )
    .unwrap();
    assert_eq!(
        (
            good.mixed_seconds,
            good.oltp_clients,
            good.offered_rate,
            good.require_production
        ),
        (3, 4, 1000, true)
    );
}

#[test]
fn arrival_schedule_is_absolute_and_partitions_without_omissions() {
    let mut seen = Vec::new();
    for client in 0..4 {
        for index in arrival_indices(client, 4, 10).unwrap() {
            seen.push(index);
        }
    }
    seen.sort();
    assert_eq!(seen, (0..10).collect::<Vec<_>>());
    assert_eq!(arrival_offset(0, 1000).unwrap().as_nanos(), 0);
    assert_eq!(arrival_offset(3, 1000).unwrap().as_micros(), 3000);
    assert_eq!(arrival_offset(999, 1000).unwrap().as_micros(), 999000);
    assert!(arrival_offset(1, 0).is_err());
    assert!(arrival_offset(3_000_001, 10).is_err());
    assert!(arrival_indices(4, 4, 10).is_err());
    assert!(arrival_indices(0, 0, 10).is_err());
}

#[test]
fn histograms_merge_counts_and_quantiles_without_unbounded_samples() {
    let mut first = LatencyHistogram::new().unwrap();
    let mut second = LatencyHistogram::new().unwrap();
    for n in 1..=50 {
        first.record_us(n * 100).unwrap();
    }
    for n in 51..=100 {
        second.record_us(n * 100).unwrap();
    }
    first.merge(&second).unwrap();
    let report = first.report();
    assert_eq!(report["count"], 100);
    let p50 = report["p50_ms"].as_f64().unwrap();
    let p99 = report["p99_ms"].as_f64().unwrap();
    assert!((5.0..5.01).contains(&p50));
    assert!((9.9..9.91).contains(&p99));
    let count: u64 = report["buckets_us"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b[1].as_u64().unwrap())
        .sum();
    assert_eq!(count, 100);
    assert!(first.record_us(120_000_001).is_err());
}

fn reports() -> Vec<serde_json::Value> {
    ["postgresql_oltp_only", "enabled_oltp_only", "postgresql_fresh_heap", "enabled_fresh_heap", "enabled_frozen_projection", "enabled_fresh_capture"]
        .into_iter().map(|name| json!({ "round": 1, "phase": name, "latency_complete": true,
            "offered_transactions": 3000, "window_seconds": 3, "oltp_clients": 4,
            "scheduled_to_commit": { "p99_ms": if name == "enabled_fresh_heap" { 6.0 } else { 2.0 } } })).collect()
}

#[test]
fn p99_comparisons_require_complete_equal_offered_load() {
    let mut input = reports();
    let good = mixed_comparisons(&input).unwrap();
    assert_eq!(good[2]["p99_ratio"], 3.0);
    assert_eq!(good[3]["frozen_projection_not_fresh"], true);
    let mut incomplete_base = reports();
    incomplete_base[1]["latency_complete"] = json!(false);
    assert_eq!(
        mixed_comparisons(&incomplete_base).unwrap()[2]["p99_ratio"],
        serde_json::Value::Null
    );
    input[3]["latency_complete"] = json!(false);
    assert_eq!(
        mixed_comparisons(&input).unwrap()[2]["p99_ratio"],
        serde_json::Value::Null
    );
    input[3]["latency_complete"] = json!(true);
    input[3]["offered_transactions"] = json!(2999);
    assert_eq!(
        mixed_comparisons(&input).unwrap()[2]["p99_ratio"],
        serde_json::Value::Null
    );
    input.pop();
    assert!(mixed_comparisons(&input).is_err());
    let mut missing = reports();
    missing[0]
        .as_object_mut()
        .unwrap()
        .remove("offered_transactions");
    assert!(mixed_comparisons(&missing).is_err());
    let mut duplicate = reports();
    duplicate.push(duplicate[0].clone());
    assert!(mixed_comparisons(&duplicate).is_err());
}

#[test]
fn fresh_rust_comparison_preserves_old_reports_and_requires_complete_new_rounds() {
    let mut input = reports();
    assert_eq!(
        mixed_comparisons(&input).unwrap().as_array().unwrap().len(),
        5
    );
    let mut fresh = input[2].clone();
    fresh["phase"] = json!("enabled_fresh_rust_heap");
    fresh["scheduled_to_commit"]["p99_ms"] = json!(4.0);
    input.push(fresh);
    let comparisons = mixed_comparisons(&input).unwrap();
    assert_eq!(comparisons[5]["p99_ratio"], 2.0);
    assert_eq!(comparisons[5]["baseline"], "postgresql_fresh_heap");
    let mut incomplete = input.clone();
    incomplete[6]["latency_complete"] = json!(false);
    assert_eq!(
        mixed_comparisons(&incomplete).unwrap()[5]["p99_ratio"],
        serde_json::Value::Null
    );
    input.extend(reports().into_iter().map(|mut row| {
        row["round"] = json!(2);
        row
    }));
    assert!(mixed_comparisons(&input).is_err());
}
