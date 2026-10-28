use syntra_bench::Options;

#[test]
fn combined_maintenance_requires_explicit_opt_in() {
    assert!(
        !Options::parse(["--pg-bin", "/pg/bin"].map(String::from))
            .unwrap()
            .checkpoint_combined_maintenance
    );
    for (value, expected) in [("true", true), ("false", false)] {
        let options = Options::parse(
            [
                "--pg-bin",
                "/pg/bin",
                "--checkpoint-combined-maintenance",
                value,
            ]
            .map(String::from),
        )
        .unwrap();
        assert_eq!(options.checkpoint_combined_maintenance, expected);
        assert_eq!(options.checkpoint_worker_rate, 0);
    }
}

#[test]
fn combined_maintenance_rejects_non_boolean_values() {
    for value in ["1", "yes", "TRUE", "no"] {
        assert!(Options::parse(
            [
                "--pg-bin",
                "/pg/bin",
                "--checkpoint-combined-maintenance",
                value
            ]
            .map(String::from)
        )
        .is_err());
    }
}
