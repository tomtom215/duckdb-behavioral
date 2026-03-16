# Examples

Standalone SQL scripts demonstrating `duckdb-behavioral` functions. Each script
is self-contained with sample data — copy and paste into any DuckDB session with
the extension loaded.

## Running

```sql
INSTALL behavioral FROM community;
LOAD behavioral;
```

Then run any script:

```bash
duckdb < examples/01_sessions.sql
```

## Scripts

| File | Description |
|---|---|
| `01_sessions.sql` | Session assignment and metrics |
| `02_funnel.sql` | Conversion funnel analysis with drop-off report |
| `03_retention.sql` | Weekly cohort retention |
| `04_pattern_matching.sql` | Event sequence detection with time constraints |
| `05_user_flow.sql` | Forward and backward user journey analysis |
| `06_combined_analysis.sql` | Multi-function analysis combining sessions, funnels, and patterns |
