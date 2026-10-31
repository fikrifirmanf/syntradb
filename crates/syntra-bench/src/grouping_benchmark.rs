//! Matched warm-heap grouped aggregates. No writers or capture maintenance run.
use crate::Cluster;
use postgres::{Client, Row};
use serde_json::{json, Value};
use std::time::Instant;
use syntra_bench::{summarize_samples, Options, Result, Sample};

macro_rules! native_sql {
    () => {
        "SELECT k::bigint AS group_key,count(*) AS rows,count(v) AS non_nulls, sum(v::bigint)::text AS sum,min(v)::bigint AS min,max(v)::bigint AS max FROM group_benchmark GROUP BY k ORDER BY k NULLS FIRST"
    };
}
macro_rules! rust_sql {
    () => {
        "SELECT group_key,rows,non_nulls,sum::text,min,max FROM syntra_heap_group_integer_summary('group_benchmark','k','v',$1::int4) ORDER BY group_key NULLS FIRST"
    };
}
const NATIVE: &str = native_sql!();
const RUST: &str = rust_sql!();
const NATIVE_PLAN: &str = concat!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) ", native_sql!());
const RUST_PLAN: &str = concat!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) ", rust_sql!());
// 29 is coprime with all three non-NULL key-cycle lengths (15, 1023, 65535).
// A NULL cadence sharing their factors would permanently remove some keys.
const INSERT: &str = "INSERT INTO group_benchmark(k,v) SELECT CASE WHEN g%29=0 THEN NULL ELSE (g-1)%($2::bigint-1) END,CASE WHEN g%17=0 THEN NULL ELSE (g%2001-1000)*$3::bigint END FROM generate_series(1,$1::bigint) AS g";

#[derive(Debug, PartialEq, Eq)]
struct Group {
    key: Option<i64>,
    rows: i64,
    non_nulls: i64,
    sum: Option<String>,
    min: Option<i64>,
    max: Option<i64>,
}

fn groups(rows: &[Row]) -> Result<Vec<Group>> {
    rows.iter()
        .map(|row| {
            Ok(Group {
                key: row.try_get(0)?,
                rows: row.try_get(1)?,
                non_nulls: row.try_get(2)?,
                sum: row.try_get(3)?,
                min: row.try_get(4)?,
                max: row.try_get(5)?,
            })
        })
        .collect()
}

fn measure(
    client: &mut Client,
    repetitions: usize,
    input_rows: i32,
    group_limit: i32,
) -> Result<Value> {
    let expected = groups(&client.query(NATIVE, &[])?)?;
    if expected.iter().map(|g| g.rows).sum::<i64>() != i64::from(input_rows) {
        return Err("grouped benchmark dataset has the wrong input row count".into());
    }
    let statements = [client.prepare(NATIVE)?, client.prepare(RUST)?];
    let mut requests: [Vec<f64>; 2] = Default::default();
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each iteration measures a complete prepared request; batching changes the measured operation.
    for round in 0..=repetitions {
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Rotate the first route to reduce consistent order bias.
        for step in 0..2 {
            let route = (round + step) % 2;
            let start = Instant::now();
            let rows = if route == 0 {
                client.query(&statements[route], &[])?
            } else {
                client.query(&statements[route], &[&group_limit])?
            };
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if groups(&rows)? != expected {
                return Err("grouped benchmark result mismatch; refusing to report timings".into());
            }
            if round != 0 {
                requests[route].push(elapsed);
            }
        }
    }
    let mut samples: [Vec<Sample>; 2] = Default::default();
    let mut plans: [Value; 2] = Default::default();
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Independent EXPLAIN requests measure server execution and buffers for each route.
    for round in 0..=repetitions {
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each route has one warmup and its own repeated complete query.
        for step in 0..2 {
            let route = (round + step) % 2;
            let sql = [NATIVE_PLAN, RUST_PLAN][route];
            let plan: Value = if route == 0 {
                client.query_one(sql, &[])?
            } else {
                client.query_one(sql, &[&group_limit])?
            }
            .try_get(0)?;
            if round != 0 {
                samples[route].push(Sample::from_explain(&plan)?);
            }
            plans[route] = plan;
        }
    }
    Ok(json!({
        "actual_groups": expected.len(),
        "result_rows": expected.iter().map(|g| g.rows).sum::<i64>(),
        "results_match": true,
        "native": {"prepared_request_ms": requests[0], "explain": summarize_samples(&samples[0])?, "last_plan": plans[0], "sql": NATIVE},
        "rust_heap": {"prepared_request_ms": requests[1], "explain": summarize_samples(&samples[1])?, "last_plan": plans[1], "sql": RUST},
    }))
}

pub(super) fn run(cluster: &Cluster, options: &Options) -> Result<Value> {
    let mut client = cluster.connect("syntra_enabled")?;
    client
        .batch_execute("SET statement_timeout='120s'; SET application_name='grouping_benchmark'")?;
    let parallel_default: String = client
        .query_one(
            "SELECT boot_val FROM pg_settings WHERE name='max_parallel_workers_per_gather'",
            &[],
        )?
        .try_get(0)?;
    let default_workers: i32 = parallel_default.parse()?;
    let parallel_settings = if default_workers == 0 {
        vec![0]
    } else {
        vec![0, default_workers]
    };
    let settings: Value = client.query_one("SELECT json_object_agg(name,setting) FROM pg_settings WHERE name IN('work_mem','jit','max_parallel_workers','max_parallel_workers_per_gather','parallel_setup_cost','parallel_tuple_cost')", &[])?.try_get(0)?;
    let mut cases = Vec::new();
    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Fixed benchmark matrix, not per-record lookups. Each dataset is bulk inserted once.
    for (layout, create, multiplier) in [
        (
            "int4/int4",
            "CREATE TABLE group_benchmark(k int4,v int4)",
            1_i64,
        ),
        (
            "int8/int8",
            "CREATE TABLE group_benchmark(k int8,v int8)",
            1_000_000_000_000_i64,
        ),
    ] {
        // unslop-ignore N_PLUS_ONE_IN_LOOP -- Three fixed cardinalities form independent benchmark datasets.
        for group_budget in [16_i64, 1024, 65536] {
            client.batch_execute(create)?;
            client.execute(
                INSERT,
                &[&i64::from(options.rows), &group_budget, &multiplier],
            )?;
            client.batch_execute("ANALYZE group_benchmark")?;
            let heap_bytes: i64 = client
                .query_one("SELECT pg_total_relation_size('group_benchmark')", &[])?
                .try_get(0)?;
            // unslop-ignore N_PLUS_ONE_IN_LOOP -- The fixed memory controls distinguish spill costs from in-memory aggregation costs.
            for (memory_kib, memory) in [(4096_i64, "4MB"), (65536, "64MB")] {
                client.query_one("SELECT set_config('work_mem',$1,false)", &[&memory])?;
                let actual_memory: i64 = client
                    .query_one(
                        "SELECT setting::bigint FROM pg_settings WHERE name='work_mem'",
                        &[],
                    )?
                    .try_get(0)?;
                if actual_memory != memory_kib {
                    return Err("grouped benchmark work_mem control did not take effect".into());
                }
                // unslop-ignore N_PLUS_ONE_IN_LOOP -- Compare serial with PostgreSQL's built-in default; the baseline harness configures zero globally.
                for workers in &parallel_settings {
                    client.query_one(
                        "SELECT set_config('max_parallel_workers_per_gather',$1,false)",
                        &[&workers.to_string()],
                    )?;
                    eprintln!("Grouping benchmark: {layout}, {} rows, {group_budget} group budget, {memory} work_mem, {workers} parallel workers allowed", options.rows);
                    cases.push(json!({"layout": layout, "input_rows": options.rows, "group_budget": group_budget,
                        "heap_bytes": heap_bytes, "work_mem_kib": actual_memory, "rust_max_groups": group_budget,
                        "max_parallel_workers_per_gather": workers,
                        "measurement": measure(&mut client, options.repetitions, options.rows, i32::try_from(group_budget)?)?}));
                }
            }
            client.batch_execute("DROP TABLE group_benchmark")?;
        }
    }
    Ok(
        json!({"measurement_version": 2, "cases": cases, "work_mem_kib_settings": [4096,65536], "settings_before": settings, "parallel_workers_default": default_workers, "repetitions": options.repetitions,
        "scope": "isolated warm heap; two integer layouts, three group budgets, deterministic interleaved keys and NULLs; no writers, capture, spill guarantee or planner routing; compares serial with PostgreSQL's built-in parallel-worker default; JIT stays off in both routes",
        "timing": "raw client prepared-request milliseconds include all result rows; separate EXPLAIN ANALYZE samples include server instrumentation; no speedup inferred from EXPLAIN alone",
        "memory_policy": "work_mem controls PostgreSQL plan nodes and the output tuplestore; Rust hash state is bounded by the recorded group cap, not work_mem or an RSS quota; inspect saved plans to establish whether native execution stayed in memory",
        "execution_order": "one warmup per route and timing method, then alternating first route each repetition; native and Rust queries share one table and backend; each dataset uses 4 MiB then 64 MiB work_mem; Rust's group cap equals the dataset budget",
        "dataset": "keys cycle across budget minus one with every 29th key NULL; every 17th value NULL; signed values repeat over 2001 integers; bigint values multiply by 10^12"}),
    )
}
