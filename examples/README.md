# Examples

Standalone SQL scripts demonstrating `duckdb-behavioral` functions. Each script
is self-contained with sample data — copy and paste into any DuckDB session with
the extension loaded.

## Running

Each script is a complete DuckDB session body, so run it from inside a session
that has the extension loaded:

```bash
duckdb -c ".read examples/01_sessions.sql"          # after INSTALL/LOAD below
```

or load the extension in the same invocation:

```bash
(echo "INSTALL behavioral FROM community; LOAD behavioral;"; cat examples/01_sessions.sql) | duckdb
```

```sql
-- In an interactive session
INSTALL behavioral FROM community;
LOAD behavioral;
.read examples/01_sessions.sql
```

Piping a script to a fresh `duckdb` process without the `LOAD` fails with
`Catalog Error: ... window_funnel does not exist`: extensions are loaded per process.
(A locally built extension loads with `duckdb -unsigned` and
`LOAD 'build/release/behavioral.duckdb_extension'`.)

## Scripts

| File | Description |
|---|---|
| `01_sessions.sql` | Session assignment and metrics |
| `02_funnel.sql` | Conversion funnel analysis with drop-off report |
| `03_retention.sql` | Weekly cohort retention |
| `04_pattern_matching.sql` | Event sequence detection with time constraints |
| `05_user_flow.sql` | Forward and backward user journey analysis |
| `06_combined_analysis.sql` | Multi-function analysis combining sessions, funnels, and patterns |
