use std::time::Duration;
use syntra_bench::{checkpoint_due_slot, Options};

#[test]
fn pacing_keeps_absolute_slots_and_drops_overdue_arrivals() {
    assert_eq!(
        checkpoint_due_slot(0, Duration::ZERO, 100, 6000).unwrap(),
        0
    );
    assert_eq!(
        checkpoint_due_slot(1, Duration::from_millis(9), 100, 6000).unwrap(),
        1
    );
    assert_eq!(
        checkpoint_due_slot(1, Duration::from_millis(10), 100, 6000).unwrap(),
        1
    );
    assert_eq!(
        checkpoint_due_slot(1, Duration::from_millis(57), 100, 6000).unwrap(),
        5
    );
    assert_eq!(
        checkpoint_due_slot(6, Duration::from_millis(60), 100, 6000).unwrap(),
        6
    );
}

#[test]
fn pacing_stops_at_the_finite_offer_even_after_long_delays() {
    assert_eq!(
        checkpoint_due_slot(7, Duration::from_secs(60), 100, 6000).unwrap(),
        6000
    );
    assert_eq!(
        checkpoint_due_slot(6000, Duration::from_secs(61), 100, 6000).unwrap(),
        6000
    );
    assert_eq!(
        checkpoint_due_slot(0, Duration::MAX, 1000, 300000).unwrap(),
        300000
    );
    for (next, rate, total) in [
        (0, 0, 10),
        (0, 9, 10),
        (0, 1001, 10),
        (11, 10, 10),
        (0, 1000, 300001),
    ] {
        assert!(checkpoint_due_slot(next, Duration::ZERO, rate, total).is_err());
    }
}

#[test]
fn checkpoint_pacing_and_replay_options_preserve_defaults_and_accept_bounds() {
    let defaults = Options::parse(["--pg-bin", "/pg/bin"].map(String::from)).unwrap();
    assert_eq!(defaults.checkpoint_worker_rate, 0);
    assert!(defaults.checkpoint_reader_replay);
    for rate in [0, 10, 100, 1000] {
        let options = Options::parse(
            [
                "--pg-bin",
                "/pg/bin",
                "--checkpoint-stress-seconds",
                "30",
                "--checkpoint-worker-rate",
                &rate.to_string(),
                "--checkpoint-reader-replay",
                "false",
            ]
            .map(String::from),
        )
        .unwrap();
        assert_eq!(options.checkpoint_worker_rate, rate);
        assert!(!options.checkpoint_reader_replay);
    }
}

#[test]
fn checkpoint_pacing_rejects_invalid_rates_disabled_stress_and_non_boolean_replay() {
    for rate in ["1", "9", "1001", "-1", "1.5", "no"] {
        assert!(Options::parse(
            [
                "--pg-bin",
                "/pg/bin",
                "--checkpoint-stress-seconds",
                "30",
                "--checkpoint-worker-rate",
                rate
            ]
            .map(String::from)
        )
        .is_err());
    }
    assert!(Options::parse(
        ["--pg-bin", "/pg/bin", "--checkpoint-worker-rate", "100"].map(String::from)
    )
    .is_err());
    for replay in ["1", "yes", "FALSE", "no"] {
        assert!(Options::parse(
            ["--pg-bin", "/pg/bin", "--checkpoint-reader-replay", replay].map(String::from)
        )
        .is_err());
    }
}
