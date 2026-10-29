use super::*;
use crate::native_catalog::int2_key;
use crate::source::WideSummary;
use crate::Column;
use std::sync::atomic::{AtomicU64, Ordering};
use syntra_storage::chunks::{
    chunk_binding, digest, partition_of, ChunkRef, DeltaRef, Manifest, PARTITIONS,
};
use syntra_storage::delta::{encoded_delta_len, Delta};
/// Where a partition's delta is stored.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stored {
    /// No delta: the chunk row holds the base at the manifest revision.
    Base,
    /// The chunk row holds the base and its delta at the manifest revision, as
    /// 0.6.0 and earlier wrote it.
    Inline,
    /// The chunk row holds the base at its base revision, and the delta is a row
    /// of `capture_chunk_deltas` at the manifest revision.
    Appended,
}
/// A partition's stored bytes. `state` and `segment` are its base; `delta`, when
/// the manifest records one, is fetched whenever either of them is.
pub(super) struct Chunk {
    index: usize,
    stored: Stored,
    /// The appended delta row's position, when `stored` is `Appended`.
    delta_row: Option<String>,
    state: Option<Vec<u8>>,
    segment: Option<Vec<u8>>,
    delta: Option<Vec<u8>>,
}
// Backend-local observation of the latest chunk pass. Backends are single-threaded;
// atomics avoid unsafe statics. Values are reset when a pass starts.
static PASS_CHUNKS: AtomicU64 = AtomicU64::new(0);
static PASS_BYTES: AtomicU64 = AtomicU64::new(0);
static PASS_PEAK: AtomicU64 = AtomicU64::new(0);
/// (chunks passed to the caller, stored bytes fetched including a chunk that then
/// failed its digest, largest bytes held at once).
pub(super) fn last_pass() -> (u64, u64, u64) {
    (
        PASS_CHUNKS.load(Ordering::Relaxed),
        PASS_BYTES.load(Ordering::Relaxed),
        PASS_PEAK.load(Ordering::Relaxed),
    )
}
/// Streams referenced chunks one at a time; `each` owns that chunk's bytes, which
/// are dropped before the next partition is read. Stored lengths must equal the
/// manifest's exact encodings (base rows for state and segment, entry count for
/// the delta) before any bytes are fetched or detoasted. A partition the manifest
/// records without a delta must store NULL there.
fn visit<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    manifest: &Manifest,
    states: u64,
    segments: u64,
    byte_limit: usize,
    mut each: impl FnMut(Chunk) -> Checked<()>,
) -> Checked<()> {
    for counter in [&PASS_CHUNKS, &PASS_BYTES, &PASS_PEAK] {
        counter.store(0, Ordering::Relaxed);
    }
    let keys = |index: usize, revision: u64| {
        [
            oid_key(1, "source_relation", source.oid()),
            int8_key(
                2,
                "epoch",
                manifest.binding.epoch as i64,
                Int8Comparison::Equal,
            ),
            int2_key(3, "partition", index as i16),
            int8_key(4, "revision", revision as i64, Int8Comparison::Equal),
        ]
    };
    let mut scan = CatalogScan::open(
        c"syntra",
        c"capture_chunks",
        c"capture_chunks_version_index",
        snapshot,
        &keys(0, manifest.chunks[0].revision),
    )
    .ok_or("checkpoint_metadata_missing")?;
    // Before ALTER EXTENSION UPDATE creates the delta table, every delta is
    // inline, as 0.6.0 stored it.
    let mut appended = match CatalogScan::open(
        c"syntra",
        c"capture_chunk_deltas",
        c"capture_chunk_deltas_pkey",
        snapshot,
        &keys(0, manifest.chunks[0].revision),
    ) {
        Some(scan) => Some(scan),
        None if !crate::native_catalog::relation_exists(c"syntra", c"capture_chunk_deltas") => None,
        None => return Err("checkpoint_metadata_missing"),
    };
    let mut input_bytes = 0_usize;
    // Fixed native index probes share two scans; no per-partition SQL requests.
    for index in 0..PARTITIONS {
        let bit = 1_u64 << index;
        let reference = &manifest.chunks[index];
        let fetch = (states | segments) & bit != 0;
        // A delta row at the manifest revision means the chunk row holds only
        // the base, at its base revision. Without one, the chunk row is at the
        // manifest revision and carries any delta inline, as 0.6.0 wrote it.
        let mut stored = Stored::Base;
        let mut delta = None;
        let mut delta_row = None;
        if let (Some(d), Some(appended)) = (reference.delta, appended.as_mut()) {
            appended
                .rescan(&keys(index, reference.revision))
                .ok_or("checkpoint_chunk_index_unavailable")?;
            if let Some(row) = appended.next() {
                if row.get::<pg_sys::Oid>(1, "source_relation") != Some(source.oid())
                    || row.get::<i64>(2, "epoch") != Some(manifest.binding.epoch as i64)
                    || row.get::<i16>(3, "partition") != Some(index as i16)
                    || row.get::<i64>(4, "revision") != Some(reference.revision as i64)
                    || row.get::<i64>(5, "row_count") != Some(i64::from(reference.rows))
                {
                    return Err("invalid_checkpoint");
                }
                if fetch {
                    let len = encoded_delta_len::<K, V>(d.entries as usize)
                        .map_err(|_| "invalid_checkpoint")?;
                    input_bytes = super::input_bytes(input_bytes, len, byte_limit)?;
                    let bytes = row
                        .bytes_exact(6, "delta", len)
                        .ok_or("invalid_checkpoint")?;
                    PASS_BYTES.fetch_add(len as u64, Ordering::Relaxed);
                    if digest(&bytes, keep).map_err(|_| "invalid_checkpoint")? != d.digest {
                        return Err("invalid_checkpoint");
                    }
                    delta = Some(bytes);
                }
                stored = Stored::Appended;
                delta_row = Some(row.tid());
                if appended.next().is_some() {
                    return Err("invalid_checkpoint");
                }
            } else {
                stored = Stored::Inline;
            }
        } else if reference.delta.is_some() {
            stored = Stored::Inline;
        }
        let (revision, rows) = if stored == Stored::Appended {
            (reference.base_revision(), reference.base_rows())
        } else {
            (reference.revision, reference.rows)
        };
        scan.rescan(&keys(index, revision))
            .ok_or("checkpoint_chunk_index_unavailable")?;
        let row = scan.next().ok_or("invalid_checkpoint")?;
        if row.get::<pg_sys::Oid>(1, "source_relation") != Some(source.oid())
            || row.get::<i64>(2, "epoch") != Some(manifest.binding.epoch as i64)
            || row.get::<i16>(3, "partition") != Some(index as i16)
            || row.get::<i64>(4, "revision") != Some(revision as i64)
            || row.get::<i64>(5, "row_count") != Some(i64::from(rows))
        {
            return Err("invalid_checkpoint");
        }
        let base_rows = reference.base_rows() as usize;
        let state = if states & bit != 0 {
            let len = ReplayState::<K, V>::encoded_checkpoint_len(base_rows)
                .map_err(|_| "invalid_checkpoint")?;
            input_bytes = super::input_bytes(input_bytes, len, byte_limit)?;
            let bytes = row
                .bytes_exact(6, "state", len)
                .ok_or("invalid_checkpoint")?;
            PASS_BYTES.fetch_add(len as u64, Ordering::Relaxed);
            if digest(&bytes, keep).map_err(|_| "invalid_checkpoint")? != reference.state_digest {
                return Err("invalid_checkpoint");
            }
            Some(bytes)
        } else {
            None
        };
        let segment = if segments & bit != 0 {
            let len =
                syntra_storage::encoded_len::<V>(base_rows).map_err(|_| "invalid_checkpoint")?;
            input_bytes = super::input_bytes(input_bytes, len, byte_limit)?;
            let bytes = row
                .bytes_exact(7, "segment", len)
                .ok_or("invalid_checkpoint")?;
            PASS_BYTES.fetch_add(len as u64, Ordering::Relaxed);
            if digest(&bytes, keep).map_err(|_| "invalid_checkpoint")? != reference.segment_digest {
                return Err("invalid_checkpoint");
            }
            Some(bytes)
        } else {
            None
        };
        let null = row.is_null(8, "delta", pg_sys::BYTEAOID);
        match (stored, reference.delta) {
            (Stored::Base, _) | (Stored::Appended, _) if !null => return Err("invalid_checkpoint"),
            (Stored::Inline, Some(d)) if fetch => {
                let len = encoded_delta_len::<K, V>(d.entries as usize)
                    .map_err(|_| "invalid_checkpoint")?;
                input_bytes = super::input_bytes(input_bytes, len, byte_limit)?;
                let bytes = row
                    .bytes_exact(8, "delta", len)
                    .ok_or("invalid_checkpoint")?;
                PASS_BYTES.fetch_add(len as u64, Ordering::Relaxed);
                if digest(&bytes, keep).map_err(|_| "invalid_checkpoint")? != d.digest {
                    return Err("invalid_checkpoint");
                }
                delta = Some(bytes);
            }
            _ => {}
        }
        if scan.next().is_some() {
            return Err("invalid_checkpoint");
        }
        let held = [&state, &segment, &delta]
            .iter()
            .map(|b| b.as_ref().map_or(0, Vec::len))
            .sum::<usize>();
        PASS_CHUNKS.fetch_add(1, Ordering::Relaxed);
        PASS_PEAK.fetch_max(held as u64, Ordering::Relaxed);
        each(Chunk {
            index,
            stored,
            delta_row,
            state,
            segment,
            delta,
        })?;
    }
    Ok(())
}
/// A stored delta, checked against its manifest reference and partition.
fn open_delta<K: Column, V: Column>(
    bytes: &[u8],
    index: usize,
    manifest: &Manifest,
) -> Checked<Delta<K, V>> {
    let reference = manifest.chunks[index].delta.ok_or("invalid_checkpoint")?;
    let delta = Delta::<K, V>::decode(bytes, keep).map_err(|_| "invalid_checkpoint")?;
    if delta.binding()
        != chunk_binding(manifest.binding, index).map_err(|_| "invalid_checkpoint")?
        || delta.base_revision() != reference.base_revision
        || delta.entries().len() != reference.entries as usize
        || i64::from(reference.base_rows) + delta.row_change()
            != i64::from(manifest.chunks[index].rows)
    {
        return Err("invalid_checkpoint");
    }
    for (n, entry) in delta.entries().iter().enumerate() {
        if n % 1024 == 0 {
            keep();
        }
        if partition_of(entry.key) != index {
            return Err("invalid_checkpoint");
        }
    }
    Ok(delta)
}
/// A partition's base state and its delta (empty when none is stored). The delta
/// must agree with the base at every key it records.
fn decode<K: Column, V: Column>(
    chunk: &Chunk,
    manifest: &Manifest,
) -> Checked<(ReplayState<K, V>, Delta<K, V>)> {
    let state = ReplayState::<K, V>::decode_checkpoint(
        chunk.state.as_deref().ok_or("invalid_checkpoint")?,
        LIMITS,
        keep,
    )
    .map_err(|_| "invalid_checkpoint")?;
    let reference = &manifest.chunks[chunk.index];
    if state.binding()
        != chunk_binding(manifest.binding, chunk.index).map_err(|_| "invalid_checkpoint")?
        || state.sequence() != reference.base_revision()
        || state.rows().len() != reference.base_rows() as usize
    {
        return Err("invalid_checkpoint");
    }
    for (index, row) in state.rows().iter().enumerate() {
        if index % 1024 == 0 {
            keep();
        }
        if partition_of(row.key) != chunk.index {
            return Err("invalid_checkpoint");
        }
    }
    representations(
        &state,
        chunk.segment.as_deref().ok_or("invalid_checkpoint")?,
    )?;
    let delta = match chunk.delta.as_deref() {
        Some(bytes) => {
            let delta = open_delta::<K, V>(bytes, chunk.index, manifest)?;
            delta
                .merged(state.rows(), keep)
                .map_err(|_| "invalid_checkpoint")?;
            delta
        }
        None if reference.delta.is_none() => Delta::empty(state.binding(), state.sequence()),
        None => return Err("invalid_checkpoint"),
    };
    Ok((state, delta))
}
/// Aggregates of a partition from its base segment and delta, without its keyed
/// state: base values minus the delta's removed values, plus its current values.
fn chunk_summary<K: Column, V: Column>(chunk: &Chunk, manifest: &Manifest) -> Checked<WideSummary> {
    let segment = chunk.segment.as_deref().ok_or("invalid_checkpoint")?;
    let s = match chunk.delta.as_deref() {
        None if manifest.chunks[chunk.index].delta.is_none() => summary::<V>(segment)?,
        None => return Err("invalid_checkpoint"),
        Some(bytes) => {
            let delta = open_delta::<K, V>(bytes, chunk.index, manifest)?;
            let column = syntra_storage::Segment::<V>::open(segment, keep)
                .map_err(|_| "invalid_checkpoint")?;
            let mut groups = Vec::new();
            for group in column.groups() {
                groups.push(group.map_err(|_| "invalid_checkpoint")?);
            }
            let mut removals = delta.removals();
            let s = aggregate(
                groups
                    .iter()
                    .flat_map(|g| g.values())
                    .filter(|v| !removals.take(*v))
                    .chain(delta.additions()),
            )
            .map_err(|_| "invalid_checkpoint")?;
            if !removals.finished() {
                return Err("invalid_checkpoint");
            }
            s
        }
    };
    if s.0 != i64::from(manifest.chunks[chunk.index].rows) {
        return Err("invalid_checkpoint");
    }
    Ok(s)
}
pub(super) fn validate<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    manifest: &Manifest,
) -> Checked<()> {
    visit::<K, V>(
        source,
        snapshot,
        manifest,
        u64::MAX,
        u64::MAX,
        usize::MAX,
        |chunk| decode::<K, V>(&chunk, manifest).map(drop),
    )
}
pub(super) fn summarize<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    manifest: &Manifest,
) -> Checked<WideSummary> {
    let mut result: WideSummary = (0, 0, None, None, None);
    visit::<K, V>(
        source,
        snapshot,
        manifest,
        0,
        u64::MAX,
        usize::MAX,
        |chunk| merge_summary(&mut result, chunk_summary::<K, V>(&chunk, manifest)?),
    )?;
    if result.0 != i64::from(manifest.rows().map_err(|_| "invalid_checkpoint")?) {
        return Err("invalid_checkpoint");
    }
    Ok(result)
}
fn merge_summary(result: &mut WideSummary, s: WideSummary) -> Checked<()> {
    result.0 = result.0.checked_add(s.0).ok_or("invalid_checkpoint")?;
    result.1 = result.1.checked_add(s.1).ok_or("invalid_checkpoint")?;
    if let Some(sum) = s.2 {
        result.2 = Some(
            result
                .2
                .unwrap_or(0)
                .checked_add(sum)
                .ok_or("invalid_checkpoint")?,
        );
    }
    if let Some(min) = s.3 {
        result.3 = Some(result.3.map_or(min, |old| old.min(min)));
    }
    if let Some(max) = s.4 {
        result.4 = Some(result.4.map_or(max, |old| old.max(max)));
    }
    Ok(())
}
/// Read-only reconstruction; no SPI, encoding or persistent progress update.
pub(super) fn replay<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    manifest: &Manifest,
    delta: &[Change<K, V>],
    expected_rows: i64,
) -> Checked<WideSummary> {
    const MAX_ROWS: usize = 65_536;
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    if delta.is_empty() || delta.len() > 256 {
        return Err("checkpoint_tail_limit");
    }
    let mut edits: [Vec<Change<K, V>>; PARTITIONS] = std::array::from_fn(|_| Vec::new());
    let mut dirty = 0_u64;
    for change in delta {
        let index = partition_of(change.key);
        dirty |= 1_u64 << index;
        edits[index].push(*change);
    }
    let initial_rows: usize = manifest
        .chunks
        .iter()
        .enumerate()
        .filter(|(index, _)| dirty & (1_u64 << index) != 0)
        .map(|(_, c)| c.rows as usize)
        .sum();
    if initial_rows
        .checked_add(delta.len())
        .is_none_or(|n| n > MAX_ROWS)
    {
        return Err("checkpoint_tail_row_limit");
    }
    let mut result: WideSummary = (0, 0, None, None, None);
    visit::<K, V>(
        source,
        snapshot,
        manifest,
        dirty,
        u64::MAX,
        MAX_BYTES,
        |chunk| {
            let s = if dirty & (1_u64 << chunk.index) != 0 {
                let (state, delta) = decode::<K, V>(&chunk, manifest)?;
                let rows = delta
                    .advance(state.rows(), &edits[chunk.index], keep)
                    .and_then(|next| next.merged(state.rows(), keep))
                    .map_err(|_| "checkpoint_tail_rejected")?;
                aggregate(rows.iter().map(|r| r.value)).map_err(|_| "checkpoint_tail_rejected")?
            } else {
                chunk_summary::<K, V>(&chunk, manifest)?
            };
            merge_summary(&mut result, s)
        },
    )?;
    if result.0 != expected_rows {
        return Err("checkpoint_tail_row_count");
    }
    Ok(result)
}
/// How the revision a write replaces is stored: an appended delta row is
/// deleted at its position, and an inline 0.6.0 delta is cleared from its chunk
/// row at its revision.
struct Prior {
    stored: Stored,
    revision: i64,
    delta_row: Option<String>,
}
impl Prior {
    const NONE: Prior = Prior {
        stored: Stored::Base,
        revision: 0,
        delta_row: None,
    };
    fn of(chunk: &Chunk, revision: u64) -> Prior {
        Prior {
            stored: chunk.stored,
            revision: revision as i64,
            delta_row: chunk.delta_row.clone(),
        }
    }
}
/// A new base for a partition, or only a new delta over its existing base.
enum Write {
    Base {
        index: i16,
        revision: i64,
        rows: i64,
        state: Vec<u8>,
        segment: Vec<u8>,
        prior: Prior,
    },
    Delta {
        index: i16,
        revision: i64,
        rows: i64,
        delta: Vec<u8>,
        base_revision: i64,
        base_rows: i64,
        prior: Prior,
    },
}
fn encode<K: Column, V: Column>(
    index: usize,
    state: &ReplayState<K, V>,
    prior: Prior,
) -> Checked<(ChunkRef, Write)> {
    let keyed = state
        .encode_checkpoint(keep)
        .map_err(|_| "checkpoint encoding failed")?;
    let segment = syntra_storage::encode(state.rows().iter().map(|r| r.value), keep)
        .map_err(|_| "checkpoint segment encoding failed")?;
    let reference = ChunkRef {
        revision: state.sequence(),
        rows: state.rows().len() as u32,
        state_digest: digest(&keyed, keep).map_err(|_| "checkpoint encoding failed")?,
        segment_digest: digest(&segment, keep).map_err(|_| "checkpoint encoding failed")?,
        delta: None,
    };
    let write = Write::Base {
        index: index as i16,
        revision: state.sequence() as i64,
        rows: state.rows().len() as i64,
        state: keyed,
        segment,
        prior,
    };
    Ok((reference, write))
}
fn persist(source: &PgRelation, manifest: &Manifest, writes: Vec<Write>) -> Checked<()> {
    let (mut partitions, mut revisions, mut rows, mut states, mut segments) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut delta_partitions, mut delta_revisions, mut delta_rows, mut deltas) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut cleared_partitions, mut cleared_revisions, mut base_revisions, mut base_rows) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut superseded = Vec::new();
    for w in writes {
        let prior = match w {
            Write::Base {
                index,
                revision,
                rows: n,
                state,
                segment,
                prior,
            } => {
                partitions.push(index);
                revisions.push(revision);
                rows.push(n);
                states.push(state);
                segments.push(segment);
                prior
            }
            Write::Delta {
                index,
                revision,
                rows: n,
                delta,
                base_revision,
                base_rows: b,
                prior,
            } => {
                delta_partitions.push(index);
                delta_revisions.push(revision);
                delta_rows.push(n);
                deltas.push(delta);
                if prior.stored == Stored::Inline {
                    cleared_partitions.push(index);
                    cleared_revisions.push(prior.revision);
                    base_revisions.push(base_revision);
                    base_rows.push(b);
                }
                prior
            }
        };
        if prior.stored == Stored::Appended {
            superseded.push(
                prior
                    .delta_row
                    .ok_or("checkpoint chunk publication failed")?,
            );
        }
    }
    // No statement walks the versions an old snapshot keeps of other
    // revisions. The manifest is published last.
    if !partitions.is_empty() {
        Spi::run_with_args(r#"INSERT INTO syntra.capture_chunks(source_relation,epoch,partition,revision,row_count,state,segment,delta)
            SELECT $1,$2,p,r,n,s,v,NULL FROM unnest($3::int2[],$4::int8[],$5::int8[],$6::bytea[],$7::bytea[]) AS t(p,r,n,s,v)
            ON CONFLICT(source_relation,partition) DO UPDATE SET epoch=EXCLUDED.epoch,revision=EXCLUDED.revision,
            row_count=EXCLUDED.row_count,state=EXCLUDED.state,segment=EXCLUDED.segment,delta=NULL"#,
            &[source.oid().into(),(manifest.binding.epoch as i64).into(),partitions.into(),revisions.into(),rows.into(),states.into(),segments.into()])
            .map_err(|_|"checkpoint chunk publication failed")?;
    }
    // A delta is a new row. The base row is not written, so its state and
    // segment stay where they are and it gains no version per apply.
    if !deltas.is_empty() {
        Spi::run_with_args(
            r#"INSERT INTO syntra.capture_chunk_deltas(source_relation,epoch,partition,revision,row_count,delta)
            SELECT $1,$2,p,r,n,d FROM unnest($3::int2[],$4::int8[],$5::int8[],$6::bytea[]) AS t(p,r,n,d)"#,
            &[
                source.oid().into(),
                (manifest.binding.epoch as i64).into(),
                delta_partitions.into(),
                delta_revisions.into(),
                delta_rows.into(),
                deltas.into(),
            ],
        )
        .map_err(|_| "checkpoint chunk publication failed")?;
    }
    // A chunk row written by 0.6.0 carries its delta inline at the old
    // revision; once its delta moves to its own row, it holds only the base.
    if !cleared_partitions.is_empty() {
        let expected = cleared_partitions.len() as i64;
        let cleared = Spi::get_one_with_args::<i64>(
            r#"WITH u AS (UPDATE syntra.capture_chunks AS c
            SET revision=t.b,row_count=t.n,delta=NULL
            FROM unnest($3::int2[],$4::int8[],$5::int8[],$6::int8[]) AS t(p,r,b,n)
            WHERE c.source_relation=$1 AND c.epoch=$2 AND c.partition=t.p AND c.revision=t.r RETURNING 1)
            SELECT count(*) FROM u"#,
            &[
                source.oid().into(),
                (manifest.binding.epoch as i64).into(),
                cleared_partitions.into(),
                cleared_revisions.into(),
                base_revisions.into(),
                base_rows.into(),
            ],
        )
        .map_err(|_| "checkpoint chunk publication failed")?;
        if cleared != Some(expected) {
            return Err("checkpoint chunk publication failed");
        }
    }
    // The rows are deleted at the positions the read found them, as a TID scan:
    // a join on the key could plan a scan over every version the snapshot keeps.
    // The maintenance lock keeps those positions current.
    if !superseded.is_empty() {
        let expected = superseded.len() as i64;
        let deleted = Spi::get_one_with_args::<i64>(
            r#"WITH d AS (DELETE FROM syntra.capture_chunk_deltas AS c
            WHERE c.ctid=ANY($3::text[]::tid[]) AND c.source_relation=$1 AND c.epoch=$2 RETURNING 1)
            SELECT count(*) FROM d"#,
            &[
                source.oid().into(),
                (manifest.binding.epoch as i64).into(),
                superseded.into(),
            ],
        )
        .map_err(|_| "checkpoint chunk publication failed")?;
        if deleted != Some(expected) {
            return Err("checkpoint chunk publication failed");
        }
    }
    let bytes = manifest.encode().map_err(|_| "invalid_checkpoint")?;
    Spi::run_with_args(r#"INSERT INTO syntra.capture_checkpoints(source_relation,epoch,applied,generation,row_count,manifest)
        VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(source_relation) DO UPDATE SET epoch=EXCLUDED.epoch,
        applied=EXCLUDED.applied,generation=EXCLUDED.generation,row_count=EXCLUDED.row_count,manifest=EXCLUDED.manifest"#,
        &[source.oid().into(),(manifest.binding.epoch as i64).into(),(manifest.applied as i64).into(),(manifest.generation as i64).into(),i64::from(manifest.rows().map_err(|_|"invalid_checkpoint")?).into(),bytes.into()])
        .map_err(|_|"checkpoint manifest publication failed")?;
    Ok(())
}
pub(super) fn seed<K: Column, V: Column>(
    source: &PgRelation,
    config: &Configuration,
    h: &Head,
    mut state: ReplayState<K, V>,
    delta: &[Change<K, V>],
) -> Checked<()> {
    if !delta.is_empty() {
        state
            .apply_events(
                Batch {
                    binding: binding(config),
                    previous: 0,
                    next: h.emitted as u64,
                    changes: delta,
                },
                keep,
            )
            .map_err(|_| "checkpoint replay rejected the captured event prefix")?;
    }
    if state.rows().len() as i64 != h.rows {
        return Err("checkpoint row count disagrees with capture head");
    }
    let mut partitions: [Vec<Row<K, V>>; PARTITIONS] = std::array::from_fn(|_| Vec::new());
    for (index, row) in state.rows().iter().enumerate() {
        if index % 1024 == 0 {
            keep();
        }
        partitions[partition_of(row.key)].push(*row);
    }
    let mut manifest = Manifest {
        binding: binding(config),
        applied: h.emitted as u64,
        generation: 1,
        chunks: [ChunkRef::default(); PARTITIONS],
    };
    let mut writes = Vec::new();
    for (index, rows) in partitions.iter().enumerate() {
        let state = ReplayState::seed(
            chunk_binding(manifest.binding, index).map_err(|_| "invalid_checkpoint")?,
            0,
            rows,
            LIMITS,
            keep,
        )
        .map_err(|_| "invalid_checkpoint")?;
        let (reference, write) = encode(index, &state, Prior::NONE)?;
        manifest.chunks[index] = reference;
        writes.push(write);
    }
    persist(source, &manifest, writes)
}
/// Applies `delta` and writes a new base for every partition in the `fold` mask,
/// whether or not the delta touched it.
pub(super) fn advance<K: Column, V: Column>(
    source: &PgRelation,
    snapshot: pg_sys::Snapshot,
    h: &Head,
    mut manifest: Manifest,
    delta: &[Change<K, V>],
    fold: u64,
) -> Checked<()> {
    let mut edits: [Vec<Change<K, V>>; PARTITIONS] = std::array::from_fn(|_| Vec::new());
    let mut dirty = fold;
    for change in delta {
        let index = partition_of(change.key);
        dirty |= 1_u64 << index;
        edits[index].push(*change);
    }
    let mut writes = Vec::new();
    let mut references = Vec::new();
    // Only dirty partitions are read; each decoded state is dropped after encoding.
    visit::<K, V>(
        source,
        snapshot,
        &manifest,
        dirty,
        dirty,
        usize::MAX,
        |chunk| {
            if dirty & (1_u64 << chunk.index) == 0 {
                return Ok(());
            }
            let (state, delta) = decode::<K, V>(&chunk, &manifest)?;
            let old = manifest.chunks[chunk.index];
            let revision = old
                .revision
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or("checkpoint chunk revision exhausted")?;
            let next = delta
                .advance(state.rows(), &edits[chunk.index], keep)
                .map_err(|_| "checkpoint replay rejected the captured event prefix")?;
            // Changes that cancel out leave an unchanged partition unwritten.
            if next.is_empty() && delta.is_empty() {
                return Ok(());
            }
            if next.is_empty()
                || fold & (1_u64 << chunk.index) != 0
                || next.fold_due(state.rows().len())
            {
                let rows = next
                    .merged(state.rows(), keep)
                    .map_err(|_| "checkpoint replay rejected the captured event prefix")?;
                let base = ReplayState::seed(state.binding(), revision, &rows, LIMITS, keep)
                    .map_err(|_| "checkpoint replay rejected the captured event prefix")?;
                let (reference, write) =
                    encode(chunk.index, &base, Prior::of(&chunk, old.revision))?;
                references.push((chunk.index, reference));
                writes.push(write);
                return Ok(());
            }
            let bytes = next
                .encode(keep)
                .map_err(|_| "checkpoint encoding failed")?;
            let rows = i64::try_from(state.rows().len())
                .ok()
                .and_then(|n| n.checked_add(next.row_change()))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("checkpoint row count disagrees with capture head")?;
            references.push((
                chunk.index,
                ChunkRef {
                    revision,
                    rows,
                    delta: Some(DeltaRef {
                        base_revision: state.sequence(),
                        base_rows: state.rows().len() as u32,
                        entries: next.entries().len() as u32,
                        digest: digest(&bytes, keep).map_err(|_| "checkpoint encoding failed")?,
                    }),
                    ..old
                },
            ));
            writes.push(Write::Delta {
                index: chunk.index as i16,
                revision: revision as i64,
                rows: i64::from(rows),
                delta: bytes,
                base_revision: state.sequence() as i64,
                base_rows: state.rows().len() as i64,
                prior: Prior::of(&chunk, old.revision),
            });
            Ok(())
        },
    )?;
    for (index, reference) in references {
        manifest.chunks[index] = reference;
    }
    manifest.applied = h.emitted as u64;
    manifest.generation = manifest
        .generation
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or("checkpoint generation exhausted")?;
    if i64::from(manifest.rows().map_err(|_| "invalid_checkpoint")?) != h.rows {
        return Err("checkpoint row count disagrees with capture head");
    }
    persist(source, &manifest, writes)
}
