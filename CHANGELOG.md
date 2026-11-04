# Changelog

Release notes for each version are on the
[GitHub releases page](https://github.com/fikrifirmanf/syntradb/releases).
Upgrade and downgrade steps are in [docs/upgrades.md](docs/upgrades.md).

## 0.12.0

- Add `syntra_heap_group_integer_numeric_summary`,
  `syntra_heap_group_text_numeric_summary` and
  `syntra_heap_group_uuid_numeric_summary` for exact grouped sums, minimums and
  maximums over `numeric(p,s)` measures with precision up to 18. See
  [grouped decimal measures](docs/grouped-decimal-measures.md).
- Add `syntradb--0.11.0--0.12.0.sql` and a reverse script that refuses dependent
  user objects.
- Verification record: [docs/verification-0.12.0.md](docs/verification-0.12.0.md).
- Known limitation: the held-snapshot throughput gate still fails. Concurrent
  writers to one captured table queue on its head row, and a split-table control
  showed this is not the only cause; see
  [docs/capture-ordinal-design.md](docs/capture-ordinal-design.md).
