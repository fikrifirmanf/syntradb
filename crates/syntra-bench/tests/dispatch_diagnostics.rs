use serde_json::Value;
use syntra_bench::{DispatchDiagnostics, RequestTiming};

fn event(worker: usize, index: u64, ready: u64, dispatched: u64, completed: u64) -> RequestTiming {
    RequestTiming {
        worker,
        arrival_index: index,
        backend_pid: 123 + worker as i32,
        scheduled_us: 1000,
        ready_us: ready,
        dispatched_us: dispatched,
        completed_us: completed,
        previous_completed_us: Some(ready.saturating_sub(1)),
        sqlstate: None,
    }
}

#[test]
fn correlates_dispatch_backlog_and_service_for_the_same_request() {
    let mut diagnostics = DispatchDiagnostics::new(4).unwrap();
    diagnostics.record(event(0, 1, 4000, 4500, 4700)).unwrap();
    let report = diagnostics.report();
    let trace = &report["slowest_scheduled"][0];
    assert_eq!(
        (
            trace["dispatch_lag_us"].as_u64(),
            trace["backlog_us"].as_u64(),
            trace["after_ready_us"].as_u64()
        ),
        (Some(3500), Some(3000), Some(500))
    );
    assert_eq!(
        (
            trace["request_to_commit_us"].as_u64(),
            trace["scheduled_to_commit_us"].as_u64()
        ),
        (Some(200), Some(3700))
    );
    assert_eq!(trace["backend_pid"], 123);
    assert_eq!(report["dispatch_lag_at_least_1ms"], 1);
    assert_eq!(report["dispatch_lag_at_least_5ms"], 0);
    assert_eq!(report["dispatch_lag"]["count"], 1);
}

#[test]
fn retains_distinct_worst_requests_for_each_component_with_a_fixed_limit() {
    let mut diagnostics = DispatchDiagnostics::new(2).unwrap();
    diagnostics.record(event(0, 1, 0, 1000, 1100)).unwrap();
    diagnostics.record(event(0, 2, 0, 1000, 21000)).unwrap(); // request duration dominates
    diagnostics.record(event(0, 3, 0, 15000, 15100)).unwrap(); // dispatch lag dominates
    diagnostics.record(event(0, 4, 0, 2000, 2300)).unwrap();
    let report = diagnostics.report();
    assert_eq!(report["attempts_seen"], 4);
    for name in ["slowest_dispatch", "slowest_request", "slowest_scheduled"] {
        assert_eq!(report[name].as_array().unwrap().len(), 2);
    }
    assert_eq!(report["slowest_dispatch"][0]["arrival_index"], 3);
    assert_eq!(report["slowest_dispatch"][1]["arrival_index"], 4);
    assert_eq!(report["slowest_request"][0]["arrival_index"], 2);
    assert_eq!(report["slowest_request"][1]["arrival_index"], 4);
    assert_eq!(report["slowest_scheduled"][0]["arrival_index"], 2);
    assert_eq!(report["slowest_scheduled"][1]["arrival_index"], 3);
    assert_eq!(report["dispatch_lag_at_least_10ms"], 1);
}

#[test]
fn merging_worker_traces_preserves_global_tails_without_duplicate_events() {
    let mut first = DispatchDiagnostics::new(2).unwrap();
    let mut second = DispatchDiagnostics::new(2).unwrap();
    first.record(event(0, 0, 0, 2000, 2200)).unwrap();
    first.record(event(0, 4, 0, 3000, 3200)).unwrap();
    second.record(event(1, 1, 0, 10000, 10500)).unwrap();
    second.record(event(1, 5, 0, 1000, 20000)).unwrap();
    first.merge(&second).unwrap();
    let report = first.report();
    assert_eq!(report["attempts_seen"], 4);
    assert_eq!(report["dispatch_lag"]["count"], 4);
    assert_eq!(report["slowest_dispatch"][0]["arrival_index"], 1);
    assert_eq!(report["slowest_dispatch"][1]["arrival_index"], 4);
    assert_eq!(report["slowest_request"][0]["arrival_index"], 5);
    for name in ["slowest_dispatch", "slowest_request", "slowest_scheduled"] {
        let traces = report[name].as_array().unwrap();
        assert_ne!(traces[0]["arrival_index"], traces[1]["arrival_index"]);
    }
}

#[test]
fn invalid_event_does_not_modify_counters_and_failures_remain_identifiable() {
    let mut diagnostics = DispatchDiagnostics::new(2).unwrap();
    assert!(diagnostics.record(event(0, 1, 2000, 1500, 1800)).is_err());
    assert!(diagnostics.record(event(0, 1, 0, 2000, 1900)).is_err());
    assert_eq!(diagnostics.report()["attempts_seen"], 0);
    let mut failed = event(0, 2, 0, 7000, 8000);
    failed.sqlstate = Some("57014".to_owned());
    diagnostics.record(failed).unwrap();
    let report = diagnostics.report();
    assert_eq!(report["slowest_scheduled"][0]["sqlstate"], "57014");
    assert_eq!(report["dispatch_lag_at_least_5ms"], 1);
    assert_eq!(report["after_ready_dispatch"]["count"], 1);
    assert!(DispatchDiagnostics::new(0).is_err());
    assert!(diagnostics
        .merge(&DispatchDiagnostics::new(1).unwrap())
        .is_err());
}

#[test]
fn a_ready_worker_with_late_dispatch_has_no_previous_request_backlog() {
    let mut diagnostics = DispatchDiagnostics::new(1).unwrap();
    diagnostics.record(event(0, 1, 500, 9000, 9150)).unwrap();
    let trace = diagnostics.report()["slowest_dispatch"][0].clone();
    assert_eq!(trace["backlog_us"], 0);
    assert_eq!(trace["after_ready_us"], 8000);
    assert_eq!(trace["previous_completed_us"], 499);
    assert_eq!(trace["sqlstate"], Value::Null);
}
