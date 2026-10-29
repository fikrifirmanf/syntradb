//! Saturated capture/apply/prune correctness stress, including a retained snapshot.
use crate::Cluster;
use postgres::{types::FromSql, Client, Statement};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use syntra_bench::{
    arrival_offset, checkpoint_due_slot, CheckpointColumn, LatencyHistogram, Options, Result,
};
use syntra_storage::chunks::{chunk_binding, digest, partition_of, Manifest, PARTITIONS};
use syntra_storage::delta::{encoded_delta_len, Delta};
use syntra_storage::replay::{Batch, Change, Limits, ReplayState, Row};
use syntra_storage::{Int, Segment};

/// The read statement for a feed. Every layout returns bigint sum, min and max so
/// one comparison type serves all; the int8 sum is numeric in SQL and fits bigint
/// at these row counts. A numeric(18,2) feed is compared in hundredths, its stored
/// unit.
fn read_sql(column: CheckpointColumn) -> String {
    let (function, columns) = match column {
        CheckpointColumn::Int4 => ("int4", "s.sum,s.min::int8,s.max::int8"),
        CheckpointColumn::Int8 => ("int8", "s.sum::int8,s.min,s.max"),
        CheckpointColumn::Numeric => (
            "numeric",
            "(s.sum*100)::int8,(s.min*100)::int8,(s.max*100)::int8",
        ),
    };
    // unslop-ignore NO_SQL_INTERPOLATION -- interpolates only the fixed literals chosen above
    format!("SELECT s.rows,s.non_nulls,{columns},s.route,s.reason, ROW(s.rows,s.non_nulls,s.sum,s.min,s.max) IS NOT DISTINCT FROM ROW(n.rows,n.non_nulls,n.sum,n.min,n.max), pg_current_xact_id_if_assigned() IS NULL FROM syntra_checkpoint_{function}_summary('checkpoint_stress_source','v',$1) AS s,(SELECT count(*) AS rows,count(v) AS non_nulls,sum(v),min(v),max(v) FROM checkpoint_stress_source) AS n LIMIT 1")
}
type Wide = (i64, i64, Option<i64>, Option<i64>, Option<i64>);
fn wide_values(row: &postgres::Row) -> Result<Wide> {
    Ok((
        row.try_get(0)?,
        row.try_get(1)?,
        row.try_get(2)?,
        row.try_get(3)?,
        row.try_get(4)?,
    ))
}
const APPLY: &str = "SELECT syntra_apply_capture('checkpoint_stress_source')";
const COMBINED: &str =
    "SELECT applied,pruned FROM syntra_apply_prune_capture('checkpoint_stress_source')";
const PRUNE: &str = "SELECT syntra_prune_capture('checkpoint_stress_source')";
/// SQL for each layout's writer: (transfer one stored unit between two rows, the
/// value a moved key gets, which is 7 stored units, and the keyed scan in stored
/// units).
fn layout_sql(column: CheckpointColumn) -> (&'static str, &'static str, &'static str) {
    match column {
        CheckpointColumn::Int4 | CheckpointColumn::Int8 => (
            "UPDATE checkpoint_stress_source SET v=v+CASE WHEN id=$1::int4 THEN -1 ELSE 1 END WHERE id IN($1::int4,$2::int4)",
            "UPDATE checkpoint_stress_source SET id=$2::int4,v=7 WHERE id=$1::int4",
            "SELECT id,v FROM checkpoint_stress_source ORDER BY id LIMIT 100003",
        ),
        CheckpointColumn::Numeric => (
            "UPDATE checkpoint_stress_source SET v=v+CASE WHEN id=$1::int4 THEN -0.01 ELSE 0.01 END WHERE id IN($1::int4,$2::int4)",
            "UPDATE checkpoint_stress_source SET id=$2::int4,v=0.07 WHERE id=$1::int4",
            "SELECT id,(v*100)::int8 FROM checkpoint_stress_source ORDER BY id LIMIT 100003",
        ),
    }
}
const PROGRESS: &str = "SELECT h.epoch,h.emitted,h.pruned,h.live_rows,c.applied,c.manifest FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='checkpoint_stress_source'::regclass LIMIT 1";
// Soak vacuum runs on its own clock, independent of the offered worker rate.
const VACUUM_INTERVAL: Duration = Duration::from_secs(5);
const VACUUM: &str = "VACUUM (ANALYZE) syntra.capture_events, syntra.capture_marks, syntra.capture_chunks, syntra.capture_chunk_deltas, syntra.capture_checkpoints, syntra.capture_heads, checkpoint_stress_source";
const LIMITS: Limits = Limits {
    rows: 1_000_000,
    changes: 4096,
    buffer_bytes: 32 * 1024 * 1024,
};

struct Stats {
    name: &'static str,
    attempts: u64,
    commits: u64,
    rollbacks: u64,
    checks: u64,
    keyed_checks: u64,
    pruned: u64,
    max_pending: i64,
    max_retained: i64,
    max_deltas: usize,
    vacuums: u64,
    rejections: BTreeMap<&'static str, u64>,
    routes: BTreeMap<String, u64>,
    latency: LatencyHistogram,
    rejected_latency: LatencyHistogram,
    start_skew_ms: f64,
    planned: u64,
    scheduled_latency: LatencyHistogram,
    scheduled_rejected_latency: LatencyHistogram,
    // Writer statements timed one by one, to locate where a slow cycle spends time.
    steps: BTreeMap<&'static str, LatencyHistogram>,
}
impl Stats {
    fn new(name: &'static str, start: Instant) -> Result<Self> {
        Ok(Self {
            name,
            attempts: 0,
            commits: 0,
            rollbacks: 0,
            checks: 0,
            keyed_checks: 0,
            pruned: 0,
            max_pending: 0,
            max_retained: 0,
            max_deltas: 0,
            vacuums: 0,
            rejections: BTreeMap::new(),
            routes: BTreeMap::new(),
            latency: LatencyHistogram::new()?,
            rejected_latency: LatencyHistogram::new()?,
            planned: 0,
            scheduled_latency: LatencyHistogram::new()?,
            scheduled_rejected_latency: LatencyHistogram::new()?,
            steps: BTreeMap::new(),
            start_skew_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
    fn report(&self) -> Value {
        json!({"name":self.name,"attempts":self.attempts,"commits":self.commits,"intentional_rollbacks":self.rollbacks,"aggregate_checks":self.checks,"keyed_checks":self.keyed_checks,"events_pruned":self.pruned,"sampled_max_pending_events":self.max_pending,"sampled_max_retained_events":self.max_retained,"sampled_max_partition_deltas":self.max_deltas,"vacuum_operations":self.vacuums,"expected_rejections":self.rejections,"read_routes_and_reasons":self.routes,"successful_cycle_latency":self.latency.report(),"rejected_cycle_latency":self.rejected_latency.report(),"start_skew_ms":self.start_skew_ms,"planned_arrivals":self.planned,"missed_arrivals":self.planned.saturating_sub(self.attempts),"successful_scheduled_latency":self.scheduled_latency.report(),"rejected_scheduled_latency":self.scheduled_rejected_latency.report(),"writer_step_latency":self.steps.iter().map(|(k,v)|(*k,v.report())).collect::<BTreeMap<_,_>>()})
    }
    fn step(&mut self, name: &'static str, since: Instant) -> Result<()> {
        let elapsed = u64::try_from(since.elapsed().as_micros())?;
        match self.steps.entry(name) {
            std::collections::btree_map::Entry::Occupied(mut e) => e.get_mut().record_us(elapsed),
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert(LatencyHistogram::new()?).record_us(elapsed)
            }
        }
    }
}
fn require(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn read(
    client: &mut Client,
    statement: &Statement,
    stats: &mut Stats,
    own: bool,
    total: i64,
    replay: bool,
) -> Result<Wide> {
    let row = client.query_one(statement, &[&replay])?;
    let values = wide_values(&row)?;
    let route: &str = row.try_get(5)?;
    let reason: &str = row.try_get(6)?;
    require(
        row.try_get::<_, bool>(7)? && row.try_get::<_, bool>(8)? != own && values.2 == Some(total),
        "checkpoint stress aggregate or snapshot invariant failed",
    )?;
    require(
        matches!(route, "checkpoint" | "replay" | "heap")
            && (replay || route != "replay")
            && (route != "replay" || reason == "bounded_tail")
            && (!own || (route == "heap" && reason == "transaction_has_xid")),
        "checkpoint stress route invariant failed",
    )?;
    *stats.routes.entry(format!("{route}/{reason}")).or_default() += 1;
    stats.checks += 1;
    Ok(values)
}
fn keyed_check<K, V>(
    client: &mut Client,
    stats: &mut Stats,
    rows: i32,
    column: CheckpointColumn,
) -> Result<()>
where
    K: Int + TryFrom<i64> + for<'a> FromSql<'a>,
    V: Int + TryFrom<i64> + for<'a> FromSql<'a>,
{
    client.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    let head = client.query_one(PROGRESS, &[])?;
    let epoch: i64 = head.try_get(0)?;
    let emitted: i64 = head.try_get(1)?;
    let pruned: i64 = head.try_get(2)?;
    let live: i64 = head.try_get(3)?;
    let applied: i64 = head.try_get(4)?;
    require(
        epoch > 0
            && 0 <= pruned
            && pruned <= applied
            && applied <= emitted
            && emitted - pruned <= 4096
            && live == i64::from(rows),
        "checkpoint stress head, retention or row-count invariant failed",
    )?;
    stats.max_pending = stats.max_pending.max(emitted - applied);
    stats.max_retained = stats.max_retained.max(emitted - pruned);
    let bytes: Vec<u8> = head.try_get(5)?;
    let manifest = Manifest::decode(&bytes)?;
    require(
        manifest.applied == applied as u64
            && manifest.binding.epoch == epoch as u64
            && manifest.rows()? == rows as u32,
        "checkpoint stress durable cursor disagreed with manifest",
    )?;
    // A partition's delta is either inline in its chunk row, as 0.6.0 stored it,
    // or the one row of capture_chunk_deltas at the manifest revision.
    let chunks=client.query("SELECT c.partition,c.epoch,c.revision,c.row_count,c.state,c.segment,coalesce(d.delta,c.delta),d.revision,d.row_count,c.delta IS NOT NULL FROM syntra.capture_chunks AS c LEFT JOIN syntra.capture_chunk_deltas AS d ON d.source_relation=c.source_relation AND d.epoch=c.epoch AND d.partition=c.partition WHERE c.source_relation='checkpoint_stress_source'::regclass ORDER BY c.partition LIMIT 65",&[])?;
    require(
        chunks.len() == PARTITIONS,
        "checkpoint stress missing chunk",
    )?;
    let mut assembled = Vec::new();
    let mut deltas = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let reference = &manifest.chunks[index];
        let state_bytes: Vec<u8> = chunk.try_get(4)?;
        let segment: Vec<u8> = chunk.try_get(5)?;
        let delta_bytes: Option<Vec<u8>> = chunk.try_get(6)?;
        let appended: Option<i64> = chunk.try_get(7)?;
        let inline: bool = chunk.try_get(9)?;
        // An appended delta leaves the chunk row at its base revision.
        let (revision, rows) = match appended {
            Some(_) => (reference.base_revision(), reference.base_rows()),
            None => (reference.revision, reference.rows),
        };
        require(
            appended.is_none_or(|r| {
                !inline
                    && r == reference.revision as i64
                    && chunk.try_get::<_, Option<i64>>(8).ok().flatten()
                        == Some(i64::from(reference.rows))
            }),
            "checkpoint stress appended delta mismatch",
        )?;
        require(
            chunk.try_get::<_, i16>(0)? == index as i16
                && chunk.try_get::<_, i64>(1)? == epoch
                && chunk.try_get::<_, i64>(2)? == revision as i64
                && chunk.try_get::<_, i64>(3)? == i64::from(rows)
                && digest(&state_bytes, || true)? == reference.state_digest
                && digest(&segment, || true)? == reference.segment_digest
                && delta_bytes.is_some() == reference.delta.is_some(),
            "checkpoint stress chunk reference mismatch",
        )?;
        // The base keeps the revision and row count it was written with; a delta
        // carries the partition from there to the manifest's revision.
        let state = ReplayState::<K, V>::decode_checkpoint(&state_bytes, LIMITS, || true)?;
        require(
            state.binding() == chunk_binding(manifest.binding, index)?
                && state.sequence() == reference.base_revision()
                && state.rows().len() == reference.base_rows() as usize
                && state.rows().iter().all(|r| partition_of(r.key) == index),
            "checkpoint stress chunk state mismatch",
        )?;
        let column = Segment::<V>::open(&segment, || true)?;
        let mut values = Vec::new();
        for group in column.groups() {
            values.extend(group?.values());
        }
        require(
            values
                .iter()
                .copied()
                .eq(state.rows().iter().map(|r| r.value)),
            "checkpoint stress chunk representations disagree",
        )?;
        let (Some(bytes), Some(d)) = (&delta_bytes, reference.delta) else {
            assembled.extend_from_slice(state.rows());
            continue;
        };
        require(
            bytes.len() == encoded_delta_len::<K, V>(d.entries as usize)?
                && digest(bytes, || true)? == d.digest,
            "checkpoint stress delta reference mismatch",
        )?;
        let delta = Delta::<K, V>::decode(bytes, || true)?;
        let rows = delta.merged(state.rows(), || true)?;
        require(
            delta.binding() == state.binding()
                && delta.base_revision() == d.base_revision
                && delta.entries().len() == d.entries as usize
                && !delta.is_empty()
                && rows.len() == reference.rows as usize
                && delta.entries().iter().all(|e| partition_of(e.key) == index),
            "checkpoint stress delta mismatch",
        )?;
        deltas += 1;
        assembled.extend_from_slice(&rows);
    }
    stats.max_deltas = stats.max_deltas.max(deltas);
    assembled.sort_unstable_by_key(|r| r.key);
    let mut state =
        ReplayState::seed(manifest.binding, applied as u64, &assembled, LIMITS, || {
            true
        })?;
    let events=client.query("SELECT ordinal,key,before_present,before_value,after_present,after_value FROM syntra.capture_events WHERE source_relation='checkpoint_stress_source'::regclass AND epoch=$1 AND ordinal>$2 ORDER BY ordinal LIMIT 4097",&[&epoch,&applied])?;
    require(
        events.len() as i64 == emitted - applied,
        "checkpoint stress lost pending capture events",
    )?;
    let mut changes = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let ordinal: i64 = event.try_get(0)?;
        require(
            ordinal == applied + index as i64 + 1,
            "checkpoint stress pending event ordinal gap",
        )?;
        let before: bool = event.try_get(2)?;
        // Events are stored as bigint at either layout; narrowing must be exact.
        let old = narrow::<V>(event.try_get(3)?)?;
        let present: bool = event.try_get(4)?;
        let value = narrow::<V>(event.try_get(5)?)?;
        require(
            (before || old.is_none()) && (present || value.is_none()),
            "checkpoint stress invalid event presence flags",
        )?;
        changes.push(Change {
            key: K::try_from(event.try_get::<_, i64>(1)?)
                .map_err(|_| "checkpoint stress event key outside the feed's width")?,
            before: before.then_some(old),
            after: present.then_some(value),
        });
    }
    if !changes.is_empty() {
        state.apply_events(
            Batch {
                binding: state.binding(),
                previous: applied as u64,
                next: emitted as u64,
                changes: &changes,
            },
            || true,
        )?;
    }
    let native = client.query(layout_sql(column).2, &[])?;
    require(
        native.len() == state.rows().len(),
        "checkpoint stress keyed row count mismatch",
    )?;
    for (row, expected) in native.iter().zip(state.rows()) {
        require(
            *expected
                == Row {
                    key: row.try_get(0)?,
                    value: row.try_get(1)?,
                },
            "checkpoint stress keyed row value mismatch",
        )?;
    }
    client.batch_execute("COMMIT")?;
    stats.keyed_checks += 1;
    Ok(())
}
fn narrow<V: TryFrom<i64>>(value: Option<i64>) -> Result<Option<V>> {
    value
        .map(V::try_from)
        .transpose()
        .map_err(|_| "checkpoint stress event value outside the feed's width".into())
}
#[derive(Clone, Copy)]
enum Actor {
    Writer(i32),
    Reader(bool),
    Applier,
    Pruner,
    Monitor,
    Vacuum,
}
struct Worker {
    replay: bool,
    client: Client,
    actor: Actor,
    column: CheckpointColumn,
    read: Statement,
    transfer: Statement,
    move_key: Statement,
    apply: Statement,
    prune: Statement,
    combined: Statement,
    combined_maintenance: bool,
    monitor_vacuum: bool,
    // Soak workers keep their session open after the deadline until the sampler
    // has measured their backend for the final window and sets `stop`.
    hold_until_stop: bool,
    rate: u64,
    name: &'static str,
}
fn rejection(error: &postgres::Error, actor: Actor) -> Option<&'static str> {
    match error.code()?.code() {
        "55P03" => Some("lock_unavailable"),
        "40P01" if matches!(actor,Actor::Writer(_)|Actor::Applier|Actor::Pruner) => Some("deadlock"),
        "54000" if matches!(actor,Actor::Writer(_)) && error.as_db_error()?.message()=="capture log is full or registration changed; apply and prune or reseed before writing" => Some("capture_full"),
        _=>None,
    }
}
fn writer(worker: &mut Worker, stats: &mut Stats, index: i32, rows: i32, total: i64) -> Result<()> {
    let mut at = Instant::now();
    worker.client.batch_execute("BEGIN")?;
    let left = index * 2 + 1;
    let right = left + 1;
    require(
        worker.client.execute(&worker.transfer, &[&left, &right])? == 2,
        "checkpoint stress transfer row count mismatch",
    )?;
    stats.step("transfer", at)?;
    at = Instant::now();
    let temporary = rows + index + 1;
    let moved = temporary + 2;
    require(
        worker.client.execute(
            "INSERT INTO checkpoint_stress_source(id,v) VALUES($1::int4,NULL)",
            &[&temporary],
        )? == 1,
        "checkpoint stress insert row count mismatch",
    )?;
    stats.step("insert", at)?;
    at = Instant::now();
    require(
        worker
            .client
            .execute(&worker.move_key, &[&temporary, &moved])?
            == 1,
        "checkpoint stress key move row count mismatch",
    )?;
    stats.step("move", at)?;
    if stats.attempts.is_multiple_of(31) {
        at = Instant::now();
        read(
            &mut worker.client,
            &worker.read,
            stats,
            true,
            total + 7,
            worker.replay,
        )?;
        stats.step("own_read", at)?;
    }
    at = Instant::now();
    require(
        worker.client.execute(
            "DELETE FROM checkpoint_stress_source WHERE id=$1::int4",
            &[&moved],
        )? == 1,
        "checkpoint stress delete row count mismatch",
    )?;
    stats.step("delete", at)?;
    at = Instant::now();
    if stats.attempts.is_multiple_of(11) {
        worker.client.batch_execute("ROLLBACK")?;
        stats.rollbacks += 1;
    } else {
        worker.client.batch_execute("COMMIT")?;
        stats.commits += 1;
        stats.step("commit", at)?;
    }
    Ok(())
}
fn cycle(worker: &mut Worker, stats: &mut Stats, rows: i32, total: i64) -> Result<()> {
    match worker.actor {
        Actor::Writer(index) => writer(worker, stats, index, rows, total)?,
        Actor::Reader(repeatable) => {
            if repeatable {
                worker
                    .client
                    .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
            }
            let first = read(
                &mut worker.client,
                &worker.read,
                stats,
                false,
                total,
                worker.replay,
            )?;
            if repeatable {
                for _ in 0..8 {
                    let last = read(
                        &mut worker.client,
                        &worker.read,
                        stats,
                        false,
                        total,
                        worker.replay,
                    )?;
                    require(
                        first == last,
                        "checkpoint stress repeatable-read snapshot changed",
                    )?;
                }
                worker.client.batch_execute("COMMIT")?;
            }
            stats.commits += 1;
        }
        Actor::Applier => {
            worker.client.batch_execute("BEGIN")?;
            let (applied, removed): (i64, i64) =
                if worker.combined_maintenance && !stats.attempts.is_multiple_of(2) {
                    let row = worker.client.query_one(&worker.combined, &[])?;
                    (row.try_get(0)?, row.try_get(1)?)
                } else {
                    let applied = worker.client.query_one(&worker.apply, &[])?.try_get(0)?;
                    let removed = if stats.attempts.is_multiple_of(2) {
                        0
                    } else {
                        worker.client.query_one(&worker.prune, &[])?.try_get(0)?
                    };
                    (applied, removed)
                };
            require(
                applied >= 0 && (0..=4096).contains(&removed),
                "checkpoint stress invalid maintenance progress",
            )?;
            if stats.attempts.is_multiple_of(17) {
                worker.client.batch_execute("ROLLBACK")?;
                stats.rollbacks += 1;
            } else {
                worker.client.batch_execute("COMMIT")?;
                stats.commits += 1;
                stats.pruned += removed as u64;
            }
        }
        Actor::Pruner => {
            let removed: i64 = worker.client.query_one(&worker.prune, &[])?.try_get(0)?;
            require(
                (0..=4096).contains(&removed),
                "checkpoint stress invalid prune count",
            )?;
            stats.pruned += removed as u64;
            stats.commits += 1;
        }
        Actor::Monitor => {
            check_keys(&mut worker.client, stats, rows, worker.column)?;
            if worker.monitor_vacuum && stats.attempts.is_multiple_of(10) {
                worker
                    .client
                    .batch_execute("VACUUM (ANALYZE) syntra.capture_events")?;
                worker
                    .client
                    .batch_execute("VACUUM (ANALYZE) syntra.capture_marks")?;
                worker
                    .client
                    .batch_execute("VACUUM (ANALYZE) syntra.capture_checkpoints")?;
                worker
                    .client
                    .batch_execute("VACUUM (ANALYZE) syntra.capture_chunks")?;
                stats.vacuums += 4;
            }
            stats.commits += 1;
        }
        Actor::Vacuum => {
            worker.client.batch_execute(VACUUM)?;
            stats.vacuums += 6;
            stats.commits += 1;
        }
    }
    Ok(())
}
/// Keyed check with the feed's stored key and value types; numeric values are
/// stored in 8 bytes.
fn check_keys(
    client: &mut Client,
    stats: &mut Stats,
    rows: i32,
    column: CheckpointColumn,
) -> Result<()> {
    match column {
        CheckpointColumn::Int4 => keyed_check::<i32, i32>(client, stats, rows, column),
        CheckpointColumn::Int8 => keyed_check::<i64, i64>(client, stats, rows, column),
        CheckpointColumn::Numeric => keyed_check::<i32, i64>(client, stats, rows, column),
    }
}
/// An error with PostgreSQL's SQLSTATE, message and detail when it has them;
/// `postgres::Error` alone displays only "db error".
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    match error
        .downcast_ref::<postgres::Error>()
        .and_then(postgres::Error::as_db_error)
    {
        Some(db) => format!(
            "{} {}{}",
            db.code().code(),
            db.message(),
            db.detail().map(|d| format!(" ({d})")).unwrap_or_default()
        ),
        None => error.to_string(),
    }
}
fn work(
    mut worker: Worker,
    start: Instant,
    deadline: Instant,
    stop: &AtomicBool,
    rows: i32,
    total: i64,
    progress: &AtomicU64,
) -> Result<Stats> {
    let rate = worker.rate;
    let mut stats = Stats::new(worker.name, start)?;
    let vacuum = matches!(worker.actor, Actor::Vacuum);
    let span = deadline.duration_since(start);
    stats.planned = if vacuum {
        span.as_secs() / VACUUM_INTERVAL.as_secs()
    } else {
        rate * span.as_secs()
    };
    let mut next = 0;

    // unslop-ignore N_PLUS_ONE_IN_LOOP -- Each cycle is an independent concurrent database operation under correctness stress; batching changes the workload.
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        let scheduled = if vacuum {
            let target = start + VACUUM_INTERVAL * u32::try_from(stats.attempts)?;
            while Instant::now() < target && !stop.load(Ordering::Acquire) {
                thread::sleep(
                    target
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                break;
            }
            target
        } else if rate == 0 {
            Instant::now()
        } else {
            let target = start + arrival_offset(next, rate)?;
            while Instant::now() < target && !stop.load(Ordering::Acquire) {
                thread::sleep(
                    target
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                break;
            }
            let slot = checkpoint_due_slot(next, start.elapsed(), rate, stats.planned)?;
            if slot == stats.planned {
                break;
            }
            next = slot + 1;
            start + arrival_offset(slot, rate)?
        };
        stats.attempts += 1;
        let cycle_start = Instant::now();
        match cycle(&mut worker, &mut stats, rows, total) {
            Ok(()) => {
                stats
                    .latency
                    .record_us(u64::try_from(cycle_start.elapsed().as_micros())?)?;
                stats
                    .scheduled_latency
                    .record_us(u64::try_from(scheduled.elapsed().as_micros())?)?;
            }
            Err(error) => {
                let expected = error
                    .downcast_ref::<postgres::Error>()
                    .and_then(|e| rejection(e, worker.actor));
                if let Some(reason) = expected {
                    worker.client.batch_execute("ROLLBACK")?;
                    *stats.rejections.entry(reason).or_default() += 1;
                    stats
                        .rejected_latency
                        .record_us(u64::try_from(cycle_start.elapsed().as_micros())?)?;
                    stats
                        .scheduled_rejected_latency
                        .record_us(u64::try_from(scheduled.elapsed().as_micros())?)?;
                } else {
                    return Err(error);
                }
            }
        }
        progress.store(stats.commits, Ordering::Release);
    }
    while worker.hold_until_stop && !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(10));
    }
    require(
        stats.commits > 0,
        "checkpoint stress worker made no committed progress",
    )?;
    Ok(stats)
}
struct StopOnExit<'a>(&'a AtomicBool);
impl Drop for StopOnExit<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn footprint(client: &mut Client) -> Result<Value> {
    Ok(client.query_one("SELECT json_build_object('source_bytes',pg_total_relation_size('checkpoint_stress_source'),'checkpoint_catalog_bytes',pg_total_relation_size('syntra.capture_checkpoints')+pg_total_relation_size('syntra.capture_chunks')+pg_total_relation_size('syntra.capture_chunk_deltas'),'checkpoint_delta_bytes',pg_total_relation_size('syntra.capture_chunk_deltas'),'checkpoint_manifest_bytes',pg_total_relation_size('syntra.capture_checkpoints'),'checkpoint_chunk_bytes',pg_total_relation_size('syntra.capture_chunks'),'event_catalog_bytes',pg_total_relation_size('syntra.capture_events'),'base_catalog_bytes',pg_total_relation_size('syntra.capture_base_rows'),'head_catalog_bytes',pg_total_relation_size('syntra.capture_heads'),'wal_lsn',pg_current_wal_insert_lsn()::text,'emitted',h.emitted,'pruned',h.pruned,'applied',c.applied,'generation',c.generation,'retained_events',(SELECT count(*) FROM syntra.capture_events WHERE source_relation=h.source_relation)) FROM (SELECT hh.source_relation,hh.epoch,hh.base_rows,hh.pruned,coalesce(m.ordinal,hh.emitted) AS emitted,coalesce(m.live_rows,hh.live_rows) AS live_rows FROM syntra.capture_heads AS hh LEFT JOIN LATERAL (SELECT ordinal,live_rows FROM syntra.capture_marks WHERE epoch=hh.epoch AND ordinal>hh.emitted ORDER BY ordinal DESC LIMIT 1) AS m ON true) AS h JOIN syntra.capture_checkpoints AS c USING(source_relation,epoch) WHERE h.source_relation='checkpoint_stress_source'::regclass LIMIT 1",&[])?.try_get(0)?)
}
pub(super) fn run(cluster: &Cluster, options: &Options) -> Result<Value> {
    let soak = options.checkpoint_soak_seconds > 0;
    let seconds = if soak {
        options.checkpoint_soak_seconds
    } else {
        options.checkpoint_stress_seconds
    };
    eprintln!(
        "Checkpoint {}: {seconds} seconds, {} workers and one retained snapshot",
        if soak { "soak" } else { "concurrency stress" },
        if soak { 9 } else { 8 }
    );
    let rows = options.rows.clamp(4, 100000);
    let mut admin = cluster.connect("syntra_enabled")?;
    let column = options.checkpoint_column;
    // The int8 feed keeps values past the int4 range, so a truncating width fails.
    // The numeric feed's values, in hundredths, are past it too and have fractions,
    // so a scale or width error fails the aggregate and keyed checks.
    let (key, value, seed, offset): (&str, &str, &str, i64) = match column {
        CheckpointColumn::Int4 => ("int4", "int4", "$2::int8+n", 1_000_000),
        CheckpointColumn::Int8 => ("int8", "int8", "$2::int8+n", 5_000_000_000),
        CheckpointColumn::Numeric => (
            "int4",
            "numeric(18,2)",
            "($2::int8+n*37)::numeric/100",
            5_000_000_000,
        ),
    };
    // unslop-ignore NO_SQL_INTERPOLATION -- key and value are literals chosen just above.
    admin.batch_execute(&format!("SET statement_timeout='15s'; SET lock_timeout='5s'; CREATE TABLE checkpoint_stress_source(id {key} PRIMARY KEY,v {value}); CREATE ROLE checkpoint_stress_reader NOLOGIN; GRANT SELECT ON checkpoint_stress_source TO checkpoint_stress_reader"))?;
    // unslop-ignore NO_SQL_INTERPOLATION -- seed is a literal chosen just above; the values are bound.
    admin.execute(&format!("INSERT INTO checkpoint_stress_source(id,v) SELECT n,CASE WHEN n%17=0 THEN NULL ELSE {seed} END FROM generate_series(1,$1::int4) AS n"),&[&rows,&offset])?;
    admin.query_one(
        "SELECT syntra_seed_capture('checkpoint_stress_source','id','v',4096)",
        &[],
    )?;
    admin.query_one(APPLY, &[])?;
    let total: i64 = admin
        .query_one(
            if column == CheckpointColumn::Numeric {
                "SELECT (sum(v)*100)::int8 FROM checkpoint_stress_source"
            } else {
                "SELECT sum(v)::int8 FROM checkpoint_stress_source"
            },
            &[],
        )?
        .try_get(0)?;
    let before =
        footprint(&mut admin).map_err(|e| format!("checkpoint stress initial footprint: {e}"))?;
    let mut anchor = cluster.connect("syntra_enabled")?;
    // A soak holds this snapshot until its midpoint; a stress run until the end.
    let held = if soak { seconds / 2 } else { seconds } + 30;
    anchor.query_one(
        "SELECT set_config('idle_in_transaction_session_timeout',$1,false)",
        &[&format!("{held}s")],
    )?;
    anchor.batch_execute("SET statement_timeout='15s'; SET ROLE checkpoint_stress_reader; BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    let anchor_read = anchor.prepare(&read_sql(column))?;
    let mut anchor_stats = Stats::new("retained_snapshot", Instant::now())?;
    let initial = read(
        &mut anchor,
        &anchor_read,
        &mut anchor_stats,
        false,
        total,
        options.checkpoint_reader_replay,
    )?;
    require(
        anchor_stats.routes.get("checkpoint/eligible") == Some(&1),
        "checkpoint stress initial retained snapshot was not eligible",
    )?;
    let mut workers = Vec::new();
    let mut pids = Vec::new();
    let mut actors = vec![
        (Actor::Writer(0), "writer_0"),
        (Actor::Writer(1), "writer_1"),
        (Actor::Reader(false), "read_committed"),
        (Actor::Reader(true), "repeatable_read"),
        (Actor::Applier, "applier_0"),
        (Actor::Applier, "applier_1"),
        (Actor::Pruner, "pruner"),
        (Actor::Monitor, "keyed_monitor"),
    ];
    if soak {
        actors.push((Actor::Vacuum, "vacuum"));
    }
    actors.retain(|(_, name)| !options.checkpoint_soak_skip.iter().any(|skip| skip == name));
    for (actor, name) in actors {
        let mut client = cluster.connect("syntra_enabled")?;
        client.batch_execute("SET statement_timeout='15s'; SET lock_timeout='250ms'; SET idle_in_transaction_session_timeout='20s'")?;
        // Names are fixed actor labels; they let a profiler find each backend.
        client.batch_execute(&format!("SET application_name='{name}'"))?;
        if matches!(actor, Actor::Vacuum) {
            client.batch_execute("SET statement_timeout='60s'; SET lock_timeout='5s'")?;
        }
        pids.push(
            client
                .query_one("SELECT pg_backend_pid()", &[])?
                .try_get::<_, i32>(0)?,
        );
        if matches!(actor, Actor::Reader(_)) {
            client.batch_execute("SET ROLE checkpoint_stress_reader")?;
        }
        let read = client.prepare(&read_sql(column))?;
        let (transfer_sql, move_sql, _) = layout_sql(column);
        let transfer = client.prepare(transfer_sql)?;
        let move_key = client.prepare(move_sql)?;
        let apply = client.prepare(APPLY)?;
        let prune = client.prepare(PRUNE)?;
        let combined = client.prepare(COMBINED)?;
        workers.push(Worker {
            replay: options.checkpoint_reader_replay,
            client,
            actor,
            column,
            read,
            transfer,
            move_key,
            apply,
            prune,
            combined,
            combined_maintenance: options.checkpoint_combined_maintenance,
            monitor_vacuum: !soak,
            hold_until_stop: soak,
            rate: options.checkpoint_worker_rate,
            name,
        });
    }
    eprintln!("Checkpoint stress: setup complete; starting workers");
    let names: Vec<&'static str> = workers.iter().map(|w| w.name).collect();
    let progress: Vec<AtomicU64> = workers.iter().map(|_| AtomicU64::new(0)).collect();
    let mut windows = Vec::new();
    let stop = AtomicBool::new(false);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let reports = thread::scope(|scope| -> Result<Vec<Stats>> {
        let mut handles = Vec::new();
        for (worker, progress) in workers.into_iter().zip(&progress) {
            let stop = &stop;
            let name = worker.name;
            match thread::Builder::new()
                .name(name.into())
                .spawn_scoped(scope, move || {
                    let _stop = StopOnExit(stop);
                    work(worker, start, deadline, stop, rows, total, progress)
                        .map_err(|e| format!("{name}: {}", describe(e.as_ref())))
                }) {
                Ok(handle) => handles.push(handle),
                Err(e) => {
                    stop.store(true, Ordering::Release);
                    return Err(e.into());
                }
            }
        }
        let mut failure = None;
        if soak {
            let sampler = crate::checkpoint_soak::Sampler {
                names: &names,
                pids: &pids,
                progress: &progress,
                start,
                window: Duration::from_secs(options.checkpoint_soak_window_seconds),
                windows: seconds / options.checkpoint_soak_window_seconds,
                disk_budget_bytes: options.checkpoint_soak_disk_budget_mb * 1024 * 1024,
                stop: &stop,
            };
            let observed = sampler.observe(&mut admin, &mut windows, || {
                let retained = read(
                    &mut anchor,
                    &anchor_read,
                    &mut anchor_stats,
                    false,
                    total,
                    options.checkpoint_reader_replay,
                )?;
                require(
                    retained == initial
                        && anchor_stats.routes.get("checkpoint/eligible") == Some(&2),
                    "checkpoint soak lost its retained checkpoint snapshot",
                )?;
                anchor.batch_execute("COMMIT")?;
                eprintln!("Checkpoint soak: retained snapshot verified and released");
                Ok(())
            });
            stop.store(true, Ordering::Release);
            if let Err(e) = observed {
                failure = Some(format!("soak sampler: {e}"));
            }
        }
        let mut reports = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(Ok(stats)) => reports.push(stats),
                Ok(Err(e)) => {
                    // Report every worker's error: the first one joined is not
                    // necessarily the first to fail.
                    eprintln!("Checkpoint stress worker failed: {e}");
                    failure.get_or_insert(e);
                    stop.store(true, Ordering::Release);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| "checkpoint stress worker panicked".into());
                    stop.store(true, Ordering::Release);
                }
            }
        }
        if let Some(e) = failure {
            return Err(e.into());
        }
        Ok(reports)
    })?;
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!("Checkpoint stress: workers joined after {elapsed:.3}s");
    require(
        reports
            .iter()
            .filter(|s| s.name.starts_with("writer") || s.name.starts_with("applier"))
            .all(|s| s.rollbacks > 0),
        "checkpoint stress did not exercise intentional rollbacks",
    )?;
    require(
        reports.iter().map(|s| s.pruned).sum::<u64>() > 0
            && reports.iter().any(|s| s.keyed_checks > 0)
            && reports.iter().any(|s| s.vacuums > 0),
        "checkpoint stress did not exercise pruning, keyed checks and vacuum",
    )?;
    require(
        reports.iter().any(|s| s.name == "pruner" && s.pruned > 0),
        "checkpoint stress independent pruner deleted no events",
    )?;
    let after =
        footprint(&mut admin).map_err(|e| format!("checkpoint stress terminal footprint: {e}"))?;
    let committed_writes = reports
        .iter()
        .filter(|s| s.name.starts_with("writer"))
        .map(|s| s.commits)
        .sum::<u64>();
    let pruned_events = reports.iter().map(|s| s.pruned).sum::<u64>();
    require(
        after["emitted"].as_u64() == Some(committed_writes * 6)
            && after["pruned"].as_u64() == Some(pruned_events)
            && after["retained_events"].as_i64()
                == after["emitted"]
                    .as_i64()
                    .zip(after["pruned"].as_i64())
                    .map(|(emitted, pruned)| emitted - pruned),
        "checkpoint stress capture or pruning accounting disagreed with committed operations",
    )?;
    require(
        after["emitted"].as_i64().is_some_and(|n| n > 4096)
            && after["generation"].as_i64().is_some_and(|n| n > 1),
        "checkpoint stress did not advance beyond the original event budget",
    )?;
    if !soak {
        require(
            read(
                &mut anchor,
                &anchor_read,
                &mut anchor_stats,
                false,
                total,
                options.checkpoint_reader_replay,
            )? == initial
                && anchor_stats.routes.get("checkpoint/eligible") == Some(&2),
            "checkpoint stress lost its retained checkpoint snapshot",
        )?;
        anchor
            .batch_execute("COMMIT")
            .map_err(|e| format!("checkpoint stress release retained snapshot: {e}"))?;
    }
    eprintln!("Checkpoint stress: retained snapshot verified; draining checkpoint");
    let wal: String = admin
        .query_one(
            "SELECT pg_wal_lsn_diff($1::text::pg_lsn,$2::text::pg_lsn)::text",
            &[
                &after["wal_lsn"].as_str().ok_or("missing final WAL LSN")?,
                &before["wal_lsn"]
                    .as_str()
                    .ok_or("missing initial WAL LSN")?,
            ],
        )?
        .try_get(0)?;
    let soak_report = if soak {
        let wal_bytes: u64 = wal.parse()?;
        let service = reports
            .iter()
            .filter(|s| {
                matches!(
                    s.name,
                    "writer_0" | "writer_1" | "read_committed" | "repeatable_read"
                )
            })
            .map(|s| crate::checkpoint_soak::Service {
                name: s.name,
                scheduled_p99_ms: s.scheduled_latency.report()["p99_ms"]
                    .as_f64()
                    .unwrap_or(f64::MAX),
                planned: s.planned,
                missed: s.planned.saturating_sub(s.attempts),
            })
            .collect::<Vec<_>>();
        crate::checkpoint_soak::evaluate(&windows, &service, wal_bytes, committed_writes)
            .inspect_err(|_| {
                // A failed soak writes no report; keep each worker's latencies,
                // including writer steps, in the run's output.
                for report in reports.iter().map(Stats::report) {
                    eprintln!("Checkpoint soak worker: {report}");
                }
            })?
    } else {
        Value::Null
    };
    admin
        .query_one(APPLY, &[])
        .map_err(|e| format!("checkpoint stress final apply: {e}"))?;
    admin
        .query_one(PRUNE, &[])
        .map_err(|e| format!("checkpoint stress final prune: {e}"))?;
    let mut final_stats = Stats::new("quiescent", Instant::now())?;
    check_keys(&mut admin, &mut final_stats, rows, column)?;
    let final_read = admin.prepare(&read_sql(column))?;
    read(
        &mut admin,
        &final_read,
        &mut final_stats,
        false,
        total,
        options.checkpoint_reader_replay,
    )?;
    require(
        final_stats.routes.get("checkpoint/eligible") == Some(&1),
        "checkpoint reuse failed after stress drain",
    )?;
    let drained =
        footprint(&mut admin).map_err(|e| format!("checkpoint stress drained footprint: {e}"))?;
    require(
        drained["emitted"] == drained["applied"]
            && drained["emitted"] == drained["pruned"]
            && drained["retained_events"] == 0,
        "checkpoint stress drain left pending events",
    )?;
    eprintln!("Checkpoint stress: checkpoint drained; running final vacuum");
    admin
        .batch_execute("VACUUM (ANALYZE) checkpoint_stress_source")
        .map_err(|e| format!("checkpoint stress final source vacuum: {e}"))?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.capture_events")?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.capture_marks")?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.capture_checkpoints")?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.capture_chunks")?;
    admin.batch_execute("VACUUM (ANALYZE) syntra.capture_heads")?;
    let vacuumed =
        footprint(&mut admin).map_err(|e| format!("checkpoint stress vacuumed footprint: {e}"))?;
    admin
        .batch_execute("DROP TABLE checkpoint_stress_source; DROP ROLE checkpoint_stress_reader")?;
    require(
        options.checkpoint_soak_skip.is_empty(),
        "ablated soak is diagnostic only; read the per-window samples, no report is written",
    )?;
    Ok(
        json!({"duration_seconds":seconds,"soak":soak_report,"elapsed_including_drain_seconds":elapsed,"source_rows":rows,"capture_event_budget":4096,"reader_tail_opt_in":options.checkpoint_reader_replay,"combined_maintenance":options.checkpoint_combined_maintenance,"column_type":match column {CheckpointColumn::Int4=>"int4",CheckpointColumn::Int8=>"int8",CheckpointColumn::Numeric=>"numeric(18,2)"},"offered_cycles_per_worker_second":options.checkpoint_worker_rate,"arrival_policy":"zero rate is saturated; otherwise absolute slots, drop missed slots, no catch-up burst","workers":reports.iter().map(Stats::report).collect::<Vec<_>>(),"retained_snapshot_checks":anchor_stats.checks,"quiescent_keyed_check":true,"quiescent_checkpoint_reuse":true,"unexpected_errors":0,"invariant_failures":0,"before":before,"after":after,"after_drain":drained,"after_vacuum":vacuumed,"wal_bytes":wal,"scope":if soak {"paced correctness soak; independent sessions and vacuum worker; one old snapshot retained until the midpoint; per-window progress, storage, WAL, xmin horizon and backend RSS samples; RSS includes touched shared buffers; WAL excludes final drain/vacuum; no cross-database speed claim"} else {"correctness stress; optional paced per-worker arrivals; independent sessions; successful and rejected cycle service latencies; WAL excludes final drain/vacuum; one old snapshot retained throughout; no RSS quota, disk reclamation or cross-database speed claim"}}),
    )
}
