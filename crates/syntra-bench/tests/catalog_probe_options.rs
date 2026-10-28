use syntra_bench::Options;
#[test]
fn catalog_probe_is_disabled_unless_explicitly_enabled() {
    assert!(
        !Options::parse(["--pg-bin", "/pg/bin"].map(String::from))
            .unwrap()
            .catalog_probe_checks
    );
    let enabled =
        Options::parse(["--pg-bin", "/pg/bin", "--catalog-probe-checks", "true"].map(String::from))
            .unwrap();
    assert!(enabled.catalog_probe_checks);
    assert!(
        !Options::parse(
            ["--pg-bin", "/pg/bin", "--catalog-probe-checks", "false"].map(String::from)
        )
        .unwrap()
        .catalog_probe_checks
    );
}
#[test]
fn catalog_probe_rejects_malformed_boolean_inputs() {
    for value in ["1", "yes", "TRUE", "no"] {
        assert!(
            Options::parse(
                ["--pg-bin", "/pg/bin", "--catalog-probe-checks", value].map(String::from)
            )
            .is_err(),
            "accepted {value}"
        );
    }
}
