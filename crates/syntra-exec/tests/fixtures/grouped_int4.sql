-- PostgreSQL oracle for grouping.rs: 5,000 rows, 13 integer keys and a NULL key.
WITH input AS (
    SELECT CASE WHEN n % 17 = 0 THEN NULL ELSE n % 13 - 6 END AS k,
           CASE WHEN n % 7 = 0 THEN NULL ELSE (n * 7919) % 100003 - 50001 END AS v
    FROM generate_series(0, 4999) AS generated(n)
)
SELECT k, count(*), count(v), sum(v), min(v), max(v)
FROM input
GROUP BY k
ORDER BY k NULLS FIRST;
