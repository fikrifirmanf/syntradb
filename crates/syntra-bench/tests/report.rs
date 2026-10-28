use serde_json::json;
use syntra_bench::{summarize_samples, Options, Sample};

#[test]
fn rejects_unbounded_or_incomplete_options() {
    for tail in [
        vec!["--rows", "1000001"],
        vec!["--rows", "0"],
        vec!["--repetitions", "2"],
        vec!["--unknown", "1"],
        vec!["--rows"],
    ] {
        let args = [vec!["--pg-bin", "/pg/bin"], tail]
            .concat()
            .into_iter()
            .map(String::from);
        assert!(Options::parse(args).is_err());
    }
    assert!(Options::parse(Vec::<String>::new()).is_err());
}

#[test]
fn accepts_declared_limits_and_paths() {
    let parsed = Options::parse(
        [
            "--pg-bin",
            "/pg/bin",
            "--rows",
            "1000000",
            "--repetitions",
            "101",
            "--output",
            "/tmp/report.json",
        ]
        .map(String::from),
    )
    .unwrap();
    assert_eq!(
        (parsed.rows, parsed.repetitions, parsed.output.to_str()),
        (1_000_000, 101, Some("/tmp/report.json"))
    );
}

#[test]
fn extracts_server_timing_and_root_buffers_without_summing_children() {
    let sample = Sample::from_explain(&json!([{
        "Execution Time": 2.5, "Planning Time": 0.2,
        "Plan": { "Shared Hit Blocks": 12, "Shared Read Blocks": 3,
            "Plans": [{ "Shared Hit Blocks": 12, "Shared Read Blocks": 3 }] }
    }]))
    .unwrap();
    assert_eq!(
        sample,
        Sample {
            execution_ms: 2.5,
            planning_ms: 0.2,
            shared_hit_blocks: 12,
            shared_read_blocks: 3
        }
    );
    assert!(Sample::from_explain(&json!([{"Execution Time": 2.5}])).is_err());
    assert!(Sample::from_explain(&json!([])).is_err());
}

#[test]
fn reports_median_nearest_rank_p95_and_raw_samples() {
    let samples: Vec<_> = [9., 1., 7., 3., 5.]
        .into_iter()
        .map(|n| Sample {
            execution_ms: n,
            planning_ms: 0.1,
            shared_hit_blocks: 1,
            shared_read_blocks: 0,
        })
        .collect();
    let report = summarize_samples(&samples).unwrap();
    assert_eq!(report["median_ms"], 5.0);
    assert_eq!(report["p95_ms"], 9.0);
    assert_eq!(report["samples"].as_array().unwrap().len(), 5);
    assert_eq!(report["samples"][0]["execution_ms"], 9.0);
    assert_eq!(summarize_samples(&samples[..2]).unwrap()["median_ms"], 5.0);
    assert!(summarize_samples(&[]).is_err());
    assert!(summarize_samples(&[Sample {
        execution_ms: f64::NAN,
        ..samples[0]
    }])
    .is_err());
}
