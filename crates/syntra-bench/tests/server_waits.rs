use serde_json::json;
use syntra_bench::{ActivityObservation, ActivitySample, Options, WaitSamples};

fn sample(sent: u64, received: u64, pid: i32, event: &str) -> ActivitySample {
    ActivitySample {
        sent_us: sent,
        received_us: received,
        observations: vec![ActivityObservation {
            pid,
            state: Some("active".to_owned()),
            wait_type: Some("IO".to_owned()),
            wait_event: Some(event.to_owned()),
        }],
    }
}

fn traces() -> serde_json::Value {
    json!({ "slowest_dispatch": [{ "backend_pid": 42, "scheduled_us": 20000, "completed_us": 40000 }],
        "slowest_request": [{ "backend_pid": 42, "scheduled_us": 20000, "completed_us": 40000 }],
        "slowest_scheduled": [] })
}

#[test]
fn retains_only_matching_pid_and_overlapping_query_windows() {
    let mut samples = WaitSamples::new(10).unwrap();
    samples.record(sample(5000, 15000, 42, "WALSync")).unwrap();
    samples
        .record(sample(25000, 26000, 99, "DataFileRead"))
        .unwrap();
    samples
        .record(sample(60000, 61000, 42, "WALWrite"))
        .unwrap();
    let report = samples.report(&traces(), 10).unwrap();
    assert_eq!(report["query_samples"], 3);
    assert_eq!(report["backend_observations"], 3);
    assert_eq!(
        report["samples_near_retained_requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        report["samples_near_retained_requests"][0]["wait_event"],
        "WALSync"
    );
    assert_eq!(report["state_observation_counts"]["active:IO:WALSync"], 1);
    assert_eq!(
        report["state_observation_counts"]["active:IO:DataFileRead"],
        1
    );
}

#[test]
fn capacity_and_skipped_ticks_are_explicit_and_invalid_input_is_rejected() {
    let mut samples = WaitSamples::new(1).unwrap();
    assert!(samples.record(sample(20, 10, 42, "WALSync")).is_err());
    samples.record(sample(10000, 11000, 42, "WALSync")).unwrap();
    samples
        .record(sample(20000, 21000, 42, "WALWrite"))
        .unwrap();
    samples.skip_ticks(2);
    let report = samples.report(&traces(), 10).unwrap();
    assert_eq!(report["query_samples"], 1);
    assert_eq!(report["skipped_ticks_or_capacity_drops"], 3);
    assert!(samples.report(&traces(), 0).is_err());
    assert!(samples.report(&json!({}), 10).is_err());
    assert!(WaitSamples::new(7001).is_err());
}

#[test]
fn validates_opt_in_sampler_interval() {
    let tracking = Options::parse(
        [
            "--pg-bin",
            "/pg/bin",
            "--track-source",
            "true",
            "--recovery-checks",
            "true",
        ]
        .map(String::from),
    )
    .unwrap();
    assert!(tracking.track_source);
    assert!(tracking.recovery_checks);
    for value in ["1", "4", "1001"] {
        assert!(Options::parse(
            ["--pg-bin", "/pg/bin", "--wait-sample-ms", value].map(String::from)
        )
        .is_err());
    }
    assert_eq!(
        Options::parse(["--pg-bin", "/pg/bin", "--wait-sample-ms", "10"].map(String::from))
            .unwrap()
            .wait_sample_ms,
        10
    );
    assert!(
        !Options::parse(["--pg-bin", "/pg/bin", "--wal-init-zero", "false"].map(String::from))
            .unwrap()
            .wal_init_zero
    );
}
