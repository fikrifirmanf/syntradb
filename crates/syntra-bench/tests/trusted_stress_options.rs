use syntra_bench::Options;

#[test]
fn trusted_stress_accepts_disabled_and_bounded_durations() {
    for seconds in [0, 5, 60, 300] {
        let options = Options::parse([
            "--pg-bin".to_owned(),
            "/pg/bin".to_owned(),
            "--trusted-stress-seconds".to_owned(),
            seconds.to_string(),
        ])
        .unwrap();
        assert_eq!(options.trusted_stress_seconds, seconds);
    }
    assert_eq!(
        Options::parse(["--pg-bin", "/pg/bin"].map(String::from))
            .unwrap()
            .trusted_stress_seconds,
        0
    );
}

#[test]
fn trusted_stress_rejects_short_unbounded_and_malformed_durations() {
    for seconds in ["1", "4", "301", "18446744073709551615", "-1", "no", "1.5"] {
        let options = Options::parse(
            ["--pg-bin", "/pg/bin", "--trusted-stress-seconds", seconds].map(String::from),
        );
        assert!(
            options.is_err(),
            "accepted invalid stress duration {seconds}"
        );
    }
}
