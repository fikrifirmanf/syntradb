use serde_json::json;
use syntra_bench::{Options, WalUsage};
#[test]
fn statement_wal_uses_root_totals_without_double_counting_children() {
    let input = json!([{"Plan":{"WAL Records":3,"WAL FPI":1,"WAL Bytes":9000,
        "Plans":[{"WAL Records":2,"WAL FPI":1,"WAL Bytes":8000}]}}]);
    assert_eq!(
        WalUsage::from_explain(&input).unwrap(),
        WalUsage {
            records: 3,
            full_page_images: 1,
            bytes: 9000
        }
    );
    assert_eq!(
        WalUsage::from_explain(&json!([{"Plan":{"WAL Records":0,"WAL FPI":0,"WAL Bytes":0}}]))
            .unwrap()
            .bytes,
        0
    );
}
#[test]
fn missing_malformed_and_negative_wal_counters_are_rejected() {
    for input in [
        json!([]),
        json!([{"Plan":{"WAL Records":0,"WAL FPI":0}}]),
        json!([{"Plan":{"WAL Records":0,"WAL FPI":0,"WAL Bytes":-1}}]),
        json!([{"Plan":{"WAL Records":0,"WAL FPI":0,"WAL Bytes":"12"}}]),
    ] {
        assert!(WalUsage::from_explain(&input).is_err());
    }
}
#[test]
fn refresh_cost_checks_are_explicitly_opted_in() {
    assert!(
        !Options::parse(["--pg-bin", "/pg/bin"].map(String::from))
            .unwrap()
            .refresh_cost_checks
    );
    assert!(
        Options::parse(["--pg-bin", "/pg/bin", "--refresh-cost-checks", "true"].map(String::from))
            .unwrap()
            .refresh_cost_checks
    );
    assert!(Options::parse(
        ["--pg-bin", "/pg/bin", "--refresh-cost-checks", "yes"].map(String::from)
    )
    .is_err());
}
