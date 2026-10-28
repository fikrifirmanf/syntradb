use syntra_bench::Options;

#[test]
fn checkpoint_stress_is_opt_in_and_accepts_bounded_durations() {
    assert_eq!(
        Options::parse(["--pg-bin", "/pg/bin"].map(String::from))
            .unwrap()
            .checkpoint_stress_seconds,
        0
    );
    for seconds in [0, 5, 60, 300] {
        let options = Options::parse([
            "--pg-bin".to_owned(),
            "/pg/bin".to_owned(),
            "--checkpoint-stress-seconds".to_owned(),
            seconds.to_string(),
        ])
        .unwrap();
        assert_eq!(options.checkpoint_stress_seconds, seconds);
        assert_eq!(options.trusted_stress_seconds, 0);
    }
}

#[test]
fn checkpoint_stress_rejects_short_excessive_and_malformed_durations() {
    for seconds in ["1", "4", "301", "18446744073709551615", "-1", "no", "1.5"] {
        assert!(
            Options::parse(
                [
                    "--pg-bin",
                    "/pg/bin",
                    "--checkpoint-stress-seconds",
                    seconds
                ]
                .map(String::from)
            )
            .is_err(),
            "accepted {seconds}"
        );
    }
}
