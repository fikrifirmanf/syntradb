//! Experimental PostgreSQL adapter. No live-table scan substitution yet.
use pgrx::prelude::*;

/// A captured key or value column: `int4` (`i32`) or `int8` (`i64`). Catalogs store
/// both as `bigint`; reading one back into a narrower layout checks its range.
pub(crate) trait Column:
    syntra_storage::Int + TryFrom<i64> + Into<i64> + FromDatum + IntoDatum
{
}
impl Column for i32 {}
impl Column for i64 {}

/// The stored width of a capture column whose catalog width is `width`. An int2
/// column (catalog width 2) is stored at 4 bytes, so it uses the int4 encodings.
pub(crate) fn stored_width(width: i16) -> i16 {
    if width == 2 {
        4
    } else {
        width
    }
}

/// Calls `$f::<K, V>(args)` for a feed's key and value widths, in bytes. Any
/// other pair evaluates `$invalid`, so a damaged binding never picks a layout.
macro_rules! with_layout {
    ($key:expr, $value:expr, $invalid:expr, $($f:ident)::+($($arg:expr),* $(,)?)) => {
        match ($crate::stored_width($key), $crate::stored_width($value)) {
            (4, 4) => $($f)::+::<i32, i32>($($arg),*),
            (4, 8) => $($f)::+::<i32, i64>($($arg),*),
            (8, 4) => $($f)::+::<i64, i32>($($arg),*),
            (8, 8) => $($f)::+::<i64, i64>($($arg),*),
            _ => $invalid,
        }
    };
}
pub(crate) use with_layout;

mod capture;
mod checkpoint;
mod ddl;
mod live;
mod maintenance;
mod native_catalog;
mod numeric;
mod planner;
mod segment;
mod source;
mod tracking;

pgrx::pg_module_magic!();

pgrx::extension_sql_file!("../sql/fixed_projections.sql", finalize);

#[allow(non_snake_case)]
#[pg_guard]
/// # Safety
/// Called by PostgreSQL on the backend main thread during module loading.
pub unsafe extern "C-unwind" fn _PG_init() {
    // SAFETY: PostgreSQL calls module initialization on the backend main thread.
    unsafe { planner::install() };
    maintenance::install();
}

#[pg_extern(immutable, parallel_safe)]
fn syntra_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pg_extern(immutable, parallel_safe)]
fn syntra_build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

#[pg_extern(immutable, parallel_safe)]
fn syntra_source_fingerprint() -> &'static str {
    env!("SYNTRA_SOURCE_FINGERPRINT")
}

/// Explicit array input keeps source snapshots and permissions with PostgreSQL.
/// This adapter spike is not columnar storage and makes no speedup claim.
// pgrx needs the concrete named SQL columns in the exported function signature.
#[allow(clippy::type_complexity)]
#[pg_extern(immutable, parallel_safe)]
fn syntra_int4_summary(
    input: Array<'_, i32>,
) -> TableIterator<
    'static,
    (
        name!(rows, i64),
        name!(non_nulls, i64),
        name!(sum, Option<i64>),
        name!(min, Option<i32>),
        name!(max, Option<i32>),
    ),
> {
    let summary = syntra_exec::summarize_int4(input.iter(), || {
        pgrx::check_for_interrupts!();
        true
    })
    .unwrap_or_else(|error| pgrx::error!("{}", error));
    TableIterator::new(std::iter::once((
        summary.rows(),
        summary.non_nulls(),
        summary.sum(),
        summary.min(),
        summary.max(),
    )))
}

/// Per-backend observation counter for the pass-through planner hook.
#[pg_extern(volatile, parallel_unsafe)]
fn syntra_planner_calls() -> i64 {
    planner::calls()
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;
    include!("source_tests.rs");
    include!("projection_tests.rs");
    include!("tracking_tests.rs");
    include!("ddl_tests.rs");
    include!("heap_tests.rs");
    include!("live_tests.rs");
    include!("capture_tests.rs");
    include!("checkpoint_tests.rs");
    include!("int8_tests.rs");
    include!("int2_tests.rs");
    include!("numeric_tests.rs");
    include!("delta_tests.rs");

    #[pg_test]
    fn maintenance_database_list_is_split_trimmed_and_validated() {
        use crate::maintenance::parse_databases;
        assert_eq!(parse_databases(""), Ok(vec![]));
        assert_eq!(parse_databases("  "), Ok(vec![]));
        assert_eq!(parse_databases("app"), Ok(vec!["app"]));
        assert_eq!(
            parse_databases("app, reports ,x"),
            Ok(vec!["app", "reports", "x"])
        );
        assert_eq!(parse_databases("app,,x"), Err("empty database name"));
        assert_eq!(parse_databases("app,"), Err("empty database name"));
        assert_eq!(parse_databases("app,app"), Err("database listed twice"));
        assert_eq!(parse_databases(&"d".repeat(63)).map(|v| v.len()), Ok(1));
        assert_eq!(
            parse_databases(&"d".repeat(64)),
            Err("database name longer than 63 bytes")
        );
        assert_eq!(parse_databases("a,b,c,d,e,f,g,h").map(|v| v.len()), Ok(8));
        assert_eq!(
            parse_databases("a,b,c,d,e,f,g,h,i"),
            Err("more than 8 databases")
        );
    }

    #[pg_test]
    fn persisted_segment_matches_postgres_after_source_changes() {
        Spi::run("CREATE TEMP TABLE syntra_segment_source (v int4)").unwrap();
        Spi::run("INSERT INTO syntra_segment_source (v) VALUES (NULL), (-4), (0), (7), (2147483647), (-2147483648)").unwrap();
        Spi::run(
            "CREATE TEMP TABLE syntra_projection (name text PRIMARY KEY, segment bytea NOT NULL)",
        )
        .unwrap();
        Spi::run("INSERT INTO syntra_projection (name, segment) SELECT 'initial', syntra_int4_segment(array_agg(v)) FROM syntra_segment_source").unwrap();
        assert_eq!(Spi::get_one::<bool>(r#"
            WITH expected AS (SELECT count(*) AS rows, count(v) AS non_nulls, sum(v), min(v), max(v) FROM syntra_segment_source)
            SELECT ROW(s.rows, s.non_nulls, s.sum, s.min, s.max)
                IS NOT DISTINCT FROM ROW(e.rows, e.non_nulls, e.sum, e.min, e.max)
            FROM syntra_projection AS p, LATERAL syntra_segment_summary(p.segment) AS s, expected AS e
            WHERE p.name = 'initial' LIMIT 1
        "#).unwrap(), Some(true));
        Spi::run("UPDATE syntra_segment_source SET v = 100 WHERE v = 7").unwrap();
        assert_eq!(
            Spi::get_one::<i64>(
                r#"
            SELECT s.sum FROM syntra_projection AS p, LATERAL syntra_segment_summary(p.segment) AS s
            WHERE p.name = 'initial' LIMIT 1
        "#
            )
            .unwrap(),
            Some(2)
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT sum(v) FROM syntra_segment_source").unwrap(),
            Some(95)
        );
    }

    #[pg_test]
    fn segment_range_matches_sql_and_reports_pruning() {
        assert_eq!(Spi::get_one::<bool>(r#"
            WITH input AS (SELECT array_agg(n ORDER BY n) AS a FROM generate_series(0, 3071) AS n)
            SELECT rows = 101 AND non_nulls = 101 AND sum = 116150 AND min = 1100 AND max = 1200
                AND groups_scanned = 1 AND groups_pruned = 2 AND rows_examined = 1024
                AND scan_bytes = 4248 AND validation_bytes > scan_bytes
            FROM input, LATERAL syntra_segment_range_summary(syntra_int4_segment(a), 1100, 1200) LIMIT 1
        "#).unwrap(), Some(true));
    }

    #[pg_test]
    fn segment_empty_all_null_and_inverted_range_semantics() {
        assert_eq!(Spi::get_one::<bool>(r#"
            SELECT rows = 0 AND non_nulls = 0 AND sum IS NULL AND groups_scanned = 0 AND validation_bytes = 40
            FROM syntra_segment_summary(syntra_int4_segment(ARRAY[]::int4[])) LIMIT 1
        "#).unwrap(), Some(true));
        assert_eq!(
            Spi::get_one::<bool>(
                r#"
            SELECT rows = 2 AND non_nulls = 0 AND sum IS NULL AND min IS NULL AND max IS NULL
            FROM syntra_segment_summary(syntra_int4_segment(ARRAY[NULL, NULL]::int4[])) LIMIT 1
        "#
            )
            .unwrap(),
            Some(true)
        );
        assert_eq!(Spi::get_one::<bool>(r#"
            SELECT rows = 0 AND sum IS NULL AND groups_pruned = 1 AND scan_bytes = 0
            FROM syntra_segment_range_summary(syntra_int4_segment(ARRAY[1, NULL, 5]::int4[]), 5, 1) LIMIT 1
        "#).unwrap(), Some(true));
    }

    #[pg_test]
    fn segment_null_arguments_remain_strict() {
        assert_eq!(
            Spi::get_one::<bool>("SELECT syntra_int4_segment(NULL::int4[]) IS NULL").unwrap(),
            Some(true)
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM syntra_segment_summary(NULL::bytea)")
                .unwrap(),
            Some(0)
        );
        assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM syntra_segment_range_summary(syntra_int4_segment(ARRAY[1]), NULL::int4, 5)").unwrap(), Some(0));
    }

    #[pg_test(error = "segment checksum mismatch")]
    fn damaged_segment_is_a_sql_error() {
        Spi::run(
            r#"
            SELECT rows FROM syntra_segment_summary(
                set_byte(syntra_int4_segment(ARRAY[1, 2, 3]), 65, 255)
            ) LIMIT 1
        "#,
        )
        .unwrap();
    }

    #[pg_test]
    fn summary_matches_postgres_for_nullable_int4() {
        let matches = Spi::get_one::<bool>(
            r#"
            WITH input AS (SELECT ARRAY[NULL, -4, 0, 7, 2147483647, -2147483648]::int4[] AS a),
            expected AS (
                SELECT count(*) AS rows, count(v) AS non_nulls, sum(v), min(v), max(v)
                FROM input, unnest(a) AS v
            )
            SELECT ROW(s.rows, s.non_nulls, s.sum, s.min, s.max)
                IS NOT DISTINCT FROM ROW(e.rows, e.non_nulls, e.sum, e.min, e.max)
            FROM input, LATERAL syntra_int4_summary(a) AS s, expected AS e LIMIT 1
        "#,
        )
        .unwrap();
        assert_eq!(matches, Some(true));
    }

    #[pg_test]
    fn summary_handles_empty_and_all_null_inputs() {
        assert_eq!(
            Spi::get_one::<bool>(
                r#"
            SELECT rows = 0 AND non_nulls = 0 AND sum IS NULL AND min IS NULL AND max IS NULL
            FROM syntra_int4_summary(ARRAY[]::int4[]) LIMIT 1
        "#
            )
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            Spi::get_one::<bool>(
                r#"
            SELECT rows = 2 AND non_nulls = 0 AND sum IS NULL AND min IS NULL AND max IS NULL
            FROM syntra_int4_summary(ARRAY[NULL, NULL]::int4[]) LIMIT 1
        "#
            )
            .unwrap(),
            Some(true)
        );
    }

    #[pg_test]
    fn null_array_is_strict_and_returns_no_summary() {
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM syntra_int4_summary(NULL::int4[])").unwrap(),
            Some(0)
        );
    }

    #[pg_test]
    fn multidimensional_array_uses_all_elements() {
        assert_eq!(
            Spi::get_one::<bool>(
                r#"
            SELECT rows = 4 AND non_nulls = 3 AND sum = 6 AND min = 1 AND max = 3
            FROM syntra_int4_summary(ARRAY[[1, NULL], [2, 3]]::int4[]) LIMIT 1
        "#
            )
            .unwrap(),
            Some(true)
        );
    }

    #[pg_test]
    fn summary_matches_sql_across_interrupt_boundaries() {
        assert_eq!(Spi::get_one::<bool>(r#"
            WITH input_values AS (
                SELECT CASE WHEN n % 7 = 0 THEN NULL ELSE n - 2000 END AS v
                FROM generate_series(1, 4097) AS n
            ), expected AS (
                SELECT count(*) AS rows, count(v) AS non_nulls, sum(v), min(v), max(v), array_agg(v) AS a
                FROM input_values
            )
            SELECT ROW(s.rows, s.non_nulls, s.sum, s.min, s.max)
                IS NOT DISTINCT FROM ROW(e.rows, e.non_nulls, e.sum, e.min, e.max)
            FROM expected AS e, LATERAL syntra_int4_summary(e.a) AS s LIMIT 1
        "#).unwrap(), Some(true));
    }

    #[pg_test]
    fn hook_keeps_heap_queries_and_read_your_writes_working() {
        Spi::run(
            "CREATE TEMP TABLE syntra_hook_probe (id int4 PRIMARY KEY, balance int4 NOT NULL)",
        )
        .unwrap();
        Spi::run("INSERT INTO syntra_hook_probe (id, balance) VALUES (1, 100), (2, 100)").unwrap();
        let before = crate::syntra_planner_calls();
        Spi::run("UPDATE syntra_hook_probe SET balance = balance - 10 WHERE id = 1").unwrap();
        Spi::run("UPDATE syntra_hook_probe SET balance = balance + 10 WHERE id = 2").unwrap();
        assert_eq!(
            Spi::get_one::<i64>("SELECT sum(balance) FROM syntra_hook_probe").unwrap(),
            Some(200)
        );
        assert_eq!(
            Spi::get_one::<i32>("SELECT balance FROM syntra_hook_probe WHERE id = 1 LIMIT 1")
                .unwrap(),
            Some(90)
        );
        assert!(crate::syntra_planner_calls() > before);
    }

    #[pg_test]
    fn prepared_queries_work_after_hook_installation() {
        Spi::run("CREATE TEMP TABLE syntra_prepared_probe (value int4)").unwrap();
        Spi::run("INSERT INTO syntra_prepared_probe (value) VALUES (3), (NULL), (8)").unwrap();
        Spi::run("PREPARE syntra_probe_plan AS SELECT sum(value) FROM syntra_prepared_probe")
            .unwrap();
        assert_eq!(
            Spi::get_one::<i64>("EXECUTE syntra_probe_plan").unwrap(),
            Some(11)
        );
        Spi::run("INSERT INTO syntra_prepared_probe (value) VALUES (2)").unwrap();
        assert_eq!(
            Spi::get_one::<i64>("EXECUTE syntra_probe_plan").unwrap(),
            Some(13)
        );
        Spi::run("DEALLOCATE syntra_probe_plan").unwrap();
    }
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
