-- PostgreSQL oracle: int8 keys and values, with sums beyond the int8 range.
WITH input AS (
    SELECT CASE WHEN n % 17 = 0 THEN NULL ELSE (n % 13 - 6)::bigint + 5000000000 END AS k,
           CASE WHEN n % 7 = 0 THEN NULL
                ELSE ((n * 7919) % 100003 - 50001)::bigint * 1000000000000
                     + 3000000000000000000 END AS v
    FROM generate_series(0, 4999) AS generated(n)
)
SELECT k, count(*), count(v), sum(v), min(v), max(v)
FROM input
GROUP BY k
ORDER BY k NULLS FIRST;
