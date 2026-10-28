# Trusted reuse on a fixed source

The live API passed nineteen committed-session checks and the crash/restore gates.
This run measured 100,000 rows on the local ARM64 machine. PostgreSQL 18.6 had
assertions disabled. The extension and runner used release builds. Data was warm.
Each case used 51 normal prepared requests per route after one warmup.
Five routes rotated; each result was checked after its timing ended.
Every trusted sample also had to report the projection route.

| Layout | Case | Native SQL ms | Fresh Rust ms | Trusted live ms | Native / trusted |
|---|---|---:|---:|---:|---:|
| ordered | all | 3.0571 | 2.7611 | 0.3515 | 8.70 |
| ordered | range_10_percent | 2.3632 | 1.0212 | 0.1812 | 13.04 |
| ordered | range_0_1_percent | 2.0652 | 0.8121 | 0.1633 | 12.64 |
| shuffled_md5 | all | 3.0885 | 2.7637 | 0.3741 | 8.26 |
| shuffled_md5 | range_10_percent | 2.7387 | 1.5187 | 0.4622 | 5.93 |
| shuffled_md5 | range_0_1_percent | 2.4325 | 1.2510 | 0.4413 | 5.51 |

Values are median client roundtrip times. Publication cost is outside these reads.
For ordered, registration took 0.937 ms; publication took 6.757 ms.
For shuffled_md5, registration took 0.438 ms; publication took 9.096 ms.

Repeated eligible reads were faster than native SQL on these six cases.
A write invalidates the segment until explicit republication. This run used no
concurrent mixed phase. Refresh throughput and OLTP cost remain unmeasured here.
The comparison uses PostgreSQL, with no ClickHouse or other database baseline.
Production readiness and broad HTAP superiority require more evidence.

The restored implementation passed after a deliberate version-check mutation failed.
Bypassing the vector comparison made a committed source update reuse stale bytes.
The committed-write gate rejected that build before timing began.

Raw report: [trusted live run](results/trusted-live-2026-10-27.json).
Source inputs: [archive](results/source-variants/trusted-live-2026-10-27.tar.gz)
and [manifest](results/source-variants/trusted-live-2026-10-27.manifest.json).
The matching source fingerprint is `8e846ce3111142d2d55d6a05d051bdf6c6d813ee1f290d1c79de616d1b8aaa16`.

Next, test long concurrent read, write and maintenance runs. Then add bounded
change replay and measure both refresh lag and write latency. The
[live contract](../docs/trusted-live-projections.md) defines current fallback rules.
