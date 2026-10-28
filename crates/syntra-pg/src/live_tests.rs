#[pg_test]
fn trusted_publication_binds_bytes_vector_and_epoch_atomically() {
    // Serialize this global-catalog fixture against other pgrx test backends.
    Spi::run("LOCK TABLE syntra.trusted_int4_projections IN SHARE ROW EXCLUSIVE MODE").unwrap();
    Spi::run("CREATE TABLE trusted_publish(v int4); INSERT INTO trusted_publish(v) VALUES(4),(NULL),(9); SELECT syntra_track_source('trusted_publish','v')").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_publish_trusted_int4('trusted_publish','v')").unwrap(),
        Some(3)
    );
    assert_eq!(Spi::get_one::<bool>("SELECT p.epoch=t.epoch AND p.generation_vector=t.generation_vector AND s.rows=3 AND s.non_nulls=2 AND s.sum=13 AND s.min=4 AND s.max=9 FROM syntra.trusted_int4_projections AS p, syntra_source_tracking_status('trusted_publish') AS t, LATERAL syntra_segment_summary(p.segment) AS s WHERE p.source_relation='trusted_publish'::regclass LIMIT 1").unwrap(),Some(true));
    assert_eq!(Spi::get_one::<bool>("SELECT sum=13 AND route='heap' AND reason='transaction_has_xid' FROM syntra_live_int4_summary('trusted_publish','v') LIMIT 1").unwrap(),Some(true));
}

#[pg_test]
fn live_heap_fallback_covers_nulls_empty_and_strict_arguments() {
    Spi::run("CREATE TEMP TABLE trusted_empty(v int4)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows=0 AND non_nulls=0 AND sum IS NULL AND min IS NULL AND max IS NULL AND route='heap' FROM syntra_live_int4_summary('trusted_empty','v') LIMIT 1").unwrap(),Some(true));
    Spi::run("INSERT INTO trusted_empty(v) VALUES(NULL),(NULL)").unwrap();
    assert_eq!(Spi::get_one::<bool>("SELECT rows=2 AND non_nulls=0 AND sum IS NULL FROM syntra_live_int4_summary('trusted_empty','v') LIMIT 1").unwrap(),Some(true));
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra_live_int4_summary(NULL::regclass,'v')")
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM syntra_live_int4_range_summary('trusted_empty','v',NULL,1)"
        )
        .unwrap(),
        Some(0)
    );
}

#[pg_test(error = "trusted publication requires a complete active source registration")]
fn trusted_publication_requires_registration() {
    Spi::run("CREATE TABLE trusted_unregistered(v int4); SELECT syntra_publish_trusted_int4('trusted_unregistered','v')").unwrap();
}

#[pg_test(error = "trusted publication currently requires a superuser")]
fn trusted_publication_checks_superuser_even_after_an_execute_grant() {
    Spi::run("CREATE TABLE trusted_denied(v int4); CREATE ROLE trusted_publisher NOLOGIN; GRANT SELECT ON trusted_denied TO trusted_publisher; GRANT EXECUTE ON FUNCTION syntra_publish_trusted_int4(regclass,text) TO trusted_publisher; SET LOCAL ROLE trusted_publisher; SELECT syntra_publish_trusted_int4('trusted_denied','v')").unwrap();
}

#[pg_test(error = "source capture does not support row-level security")]
fn live_reader_rejects_rls_for_the_table_owner() {
    Spi::run("CREATE TABLE trusted_rls(v int4); ALTER TABLE trusted_rls ENABLE ROW LEVEL SECURITY; SELECT rows FROM syntra_live_int4_summary('trusted_rls','v') LIMIT 1").unwrap();
}

#[pg_test(error = "trusted catalog capacity of 64 columns reached")]
fn trusted_publication_enforces_capacity_for_a_new_column() {
    // Admission is intentionally nonblocking; the fixture owns its lock first.
    Spi::run("LOCK TABLE syntra.trusted_int4_projections IN SHARE ROW EXCLUSIVE MODE").unwrap();
    Spi::run("CREATE TABLE trusted_capacity(v int4,w int4); INSERT INTO trusted_capacity(v,w) VALUES(1,2); SELECT syntra_track_source('trusted_capacity','v'); SELECT syntra_publish_trusted_int4('trusted_capacity','v')").unwrap();
    // Admin-only fixture fills unused attribute keys; ordinary roles cannot insert.
    Spi::run("INSERT INTO syntra.trusted_int4_projections(source_relation,source_attribute,source_database,source_identity,epoch,generation_vector,segment) SELECT p.source_relation,n::int2,p.source_database,p.source_identity,p.epoch,p.generation_vector,p.segment FROM syntra.trusted_int4_projections AS p CROSS JOIN generate_series(3,65) AS n WHERE p.source_relation='trusted_capacity'::regclass AND p.source_attribute=1").unwrap();
    assert_eq!(
        Spi::get_one::<i64>("SELECT count(*) FROM syntra.trusted_int4_projections").unwrap(),
        Some(64)
    );
    // Replacing an existing key remains possible at capacity.
    assert_eq!(
        Spi::get_one::<i64>("SELECT syntra_publish_trusted_int4('trusted_capacity','v')").unwrap(),
        Some(1)
    );
    Spi::run("SELECT syntra_publish_trusted_int4('trusted_capacity','w')").unwrap();
}
