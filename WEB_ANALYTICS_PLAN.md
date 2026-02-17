# DuckDB Web Analytics — Project Plan

A self-hosted, privacy-focused web analytics platform powered by DuckDB and
the `behavioral` extension. Designed as a lightweight alternative to
Plausible Analytics (which uses ClickHouse) — demonstrating that DuckDB with
behavioral analytics functions can serve real-world web analytics workloads.

## Motivation

1. **Showcase `duckdb-behavioral`**: Provide a tangible, production-ready
   application that uses every behavioral analytics function in a real context.
2. **Lower operational barrier**: DuckDB is embedded — no separate database
   server to install, configure, or maintain. A single binary serves the entire
   analytics stack.
3. **Direct comparison**: Users can evaluate DuckDB-based analytics against
   ClickHouse-based alternatives (Plausible, PostHog) on the same workloads.
4. **Privacy-first**: No cookies, no personal data collection, GDPR/CCPA
   compliant by design.

## Architecture Overview

```
┌─────────────────────┐      ┌──────────────────────┐
│  Tracking Script    │─────▶│  Ingestion API       │
│  (JS, <1KB)         │ HTTP │  POST /api/event     │
└─────────────────────┘      └──────────┬───────────┘
                                        │
                                        ▼
                             ┌──────────────────────┐
                             │  Event Buffer        │
                             │  (In-memory batch)    │
                             └──────────┬───────────┘
                                        │ Flush every N events
                                        │ or T seconds
                                        ▼
                             ┌──────────────────────┐
                             │  DuckDB              │
                             │  + behavioral ext    │
                             │                      │
                             │  events.parquet      │
                             │  sessions (derived)  │
                             └──────────┬───────────┘
                                        │
                                        ▼
                             ┌──────────────────────┐
                             │  Dashboard API       │
                             │  GET /api/stats/*    │
                             └──────────┬───────────┘
                                        │
                                        ▼
                             ┌──────────────────────┐
                             │  Dashboard UI        │
                             │  (Static SPA)        │
                             └──────────────────────┘
```

### Key Design Decisions

1. **Single process**: The entire application (ingestion, storage, query,
   dashboard) runs as a single process. DuckDB is embedded — no client/server
   protocol overhead.

2. **Parquet storage**: Events are stored in partitioned Parquet files
   (`data/events/date=YYYY-MM-DD/*.parquet`). This enables:
   - Efficient date-range pruning via partition elimination
   - Easy backup (copy files)
   - Direct analysis with any tool that reads Parquet (DuckDB CLI, pandas, etc.)

3. **Batch ingestion**: Events are buffered in memory and flushed to Parquet
   periodically. This amortizes write overhead and avoids per-event I/O.

4. **Session derivation via `sessionize`**: Sessions are not a separate table.
   They are computed on-the-fly using the `sessionize` window function with a
   30-minute timeout, matching industry-standard session definitions.

5. **Behavioral analytics via extension**: Funnel analysis, retention cohorts,
   and sequence pattern matching are powered by `duckdb-behavioral` functions,
   not application-level code.

## Technology Stack

| Component | Technology | Rationale |
|---|---|---|
| Language | **Rust** | Memory safety, performance, single binary deployment |
| Web framework | **Axum** | Async, tower-based, production-ready |
| Database | **DuckDB** (embedded) | No separate server, OLAP-optimized, Parquet-native |
| Analytics | **`behavioral` extension** | Funnel, retention, session, sequence functions |
| Storage | **Parquet** (partitioned by date) | Columnar, compressed, portable |
| Frontend | **Preact + HTM** | Minimal JS (~10KB), no build step required |
| Deployment | **Single binary** + `docker-compose` | No external dependencies beyond the binary |

### Why Rust (Not Elixir Like Plausible)

- The `behavioral` extension is written in Rust — same ecosystem, shared
  tooling, consistent FFI story
- Single statically-linked binary simplifies deployment
- Direct DuckDB C API access via `libduckdb-sys` (same crate the extension uses)
- Axum provides async HTTP handling comparable to Phoenix

## Data Model

### Events Table

```sql
CREATE TABLE events (
    -- Identifiers
    site_id       VARCHAR NOT NULL,    -- domain or site identifier
    visitor_id    VARCHAR NOT NULL,    -- hashed, non-PII identifier

    -- Timing
    timestamp     TIMESTAMP NOT NULL,

    -- Event classification
    event_name    VARCHAR NOT NULL,    -- 'pageview', 'custom_goal', etc.

    -- Page context
    pathname      VARCHAR NOT NULL,    -- URL path (no query string)
    hostname      VARCHAR,             -- for multi-domain support

    -- Referral
    referrer      VARCHAR,             -- full referrer URL
    referrer_source VARCHAR,           -- extracted source (google, twitter, etc.)
    utm_source    VARCHAR,
    utm_medium    VARCHAR,
    utm_campaign  VARCHAR,
    utm_content   VARCHAR,
    utm_term      VARCHAR,

    -- Device/browser (derived from User-Agent)
    browser       VARCHAR,
    browser_version VARCHAR,
    os            VARCHAR,
    os_version    VARCHAR,
    device_type   VARCHAR,             -- 'desktop', 'mobile', 'tablet'
    screen_size   VARCHAR,             -- viewport category

    -- Geography (derived from IP, then IP discarded)
    country_code  VARCHAR(2),
    region        VARCHAR,
    city          VARCHAR,

    -- Custom properties (JSON)
    props         VARCHAR,             -- JSON string for custom event properties

    -- Revenue (for e-commerce goals)
    revenue_amount   DECIMAL(12,2),
    revenue_currency VARCHAR(3)
);
```

### Storage Layout

```
data/
├── events/
│   ├── site_id=example.com/
│   │   ├── date=2024-01-15/
│   │   │   ├── 0001.parquet
│   │   │   └── 0002.parquet
│   │   └── date=2024-01-16/
│   │       └── 0001.parquet
│   └── site_id=other-site.org/
│       └── ...
├── config.db          -- SQLite or DuckDB for app config (sites, users, goals)
└── duckdb-behavioral.so   -- the extension
```

## Feature Roadmap

### Phase 1: Core Analytics (MVP)

Minimum viable product — equivalent to basic Plausible functionality.

- [ ] **Tracking script** (`<1KB`): Sends pageview events via `POST /api/event`
- [ ] **Ingestion API**: Accepts events, extracts User-Agent/IP metadata,
  buffers, and flushes to Parquet
- [ ] **Visitor ID**: Hash of (IP + User-Agent + daily salt) — no cookies, no PII stored
- [ ] **Core dashboard metrics**:
  - Unique visitors (count distinct `visitor_id`)
  - Total pageviews
  - Bounce rate (single-page sessions / total sessions, using `sessionize`)
  - Visit duration (using `sessionize` for session boundaries)
  - Pages per visit
- [ ] **Dimension breakdowns**: Top pages, referrer sources, countries, browsers,
  OS, device type
- [ ] **Date range filtering**: Today, last 7/30/90 days, custom range
- [ ] **Real-time counter**: Current visitors (events in last 5 minutes)
- [ ] **Dashboard UI**: Single-page app with time-series chart + breakdown tables
- [ ] **Multi-site support**: One instance serves multiple domains
- [ ] **Docker image**: `docker run -p 8000:8000 -v data:/data duckdb-analytics`

### Phase 2: Behavioral Analytics

Leverage `duckdb-behavioral` functions for advanced analytics.

- [ ] **Funnel analysis** (`window_funnel`):
  - Define multi-step conversion funnels in the UI
  - Visualize drop-off between steps
  - Support all modes (strict, strict_order, strict_increase, etc.)
  - SQL: `window_funnel(INTERVAL '1 hour', timestamp, pathname='/pricing',
    event_name='signup', event_name='payment')`

- [ ] **Retention cohorts** (`retention`):
  - Weekly/monthly cohort retention tables
  - Heatmap visualization
  - SQL: `retention(event_date = cohort_start, event_date = cohort_start + INTERVAL '7 days', ...)`

- [ ] **Session analytics** (`sessionize`):
  - Session-level metrics already in Phase 1
  - Add session flow visualization (entry → exit paths)
  - Session duration distribution histogram

- [ ] **User journey patterns** (`sequence_match`, `sequence_count`):
  - "How many users viewed pricing then signed up within 1 hour?"
  - Pattern builder UI for constructing sequence queries
  - SQL: `sequence_match('(?1).*(?t<=3600)(?2)', timestamp, pathname='/pricing', event_name='signup')`

- [ ] **Flow analysis** (`sequence_next_node`):
  - "After visiting /pricing, what page do users go to next?"
  - Sankey diagram visualization
  - SQL: `sequence_next_node('forward', 'first_match', timestamp, pathname, pathname='/pricing', ...)`

- [ ] **Event sequence debugging** (`sequence_match_events`):
  - Show exact timestamps of matched funnel steps
  - Diagnostic tool for funnel configuration

### Phase 3: Production Hardening

- [ ] **Authentication**: Simple username/password, optional SSO
- [ ] **API key management**: For headless/programmatic access
- [ ] **Data retention policies**: Auto-delete data older than N days
- [ ] **Parquet compaction**: Merge small Parquet files into larger ones
- [ ] **Export**: CSV/JSON export of any dashboard view
- [ ] **Email reports**: Weekly/monthly email summaries
- [ ] **Goal tracking**: Named goals with conversion rates
- [ ] **Custom properties**: Attach arbitrary key-value metadata to events
- [ ] **Revenue tracking**: Track monetary values alongside conversion goals
- [ ] **GeoIP**: MaxMind GeoLite2 integration for country/city resolution
- [ ] **User-Agent parsing**: Detect browser, OS, device type from UA string
- [ ] **Proxy/CDN support**: `X-Forwarded-For`, `CF-Connecting-IP` headers
- [ ] **Rate limiting**: Per-IP rate limits on the ingestion endpoint
- [ ] **CORS configuration**: Restrict which origins can send events

### Phase 4: Ecosystem & Integrations

- [ ] **WordPress plugin**: Auto-install tracking script
- [ ] **Google Search Console import**: Import search analytics data
- [ ] **Google Analytics import**: Migrate historical data from GA
- [ ] **Plausible import**: Import from Plausible CE exports
- [ ] **API compatibility layer**: Plausible-compatible Stats API for
  existing dashboard integrations
- [ ] **Grafana data source**: Query DuckDB directly from Grafana
- [ ] **CLI**: Command-line tool for ad-hoc queries and management
- [ ] **Shared/public dashboards**: Generate public URLs for dashboards
- [ ] **Annotations**: Mark events on the timeline (deployments, campaigns)

## Repository Structure

```
duckdb-web-analytics/
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── CLAUDE.md
├── README.md
├── LICENSE                     # MIT or AGPL-3.0 (TBD)
│
├── src/
│   ├── main.rs                 # CLI entry point, config loading
│   ├── config.rs               # Configuration (env vars, TOML)
│   ├── server.rs               # Axum HTTP server setup
│   │
│   ├── ingest/
│   │   ├── mod.rs
│   │   ├── handler.rs          # POST /api/event handler
│   │   ├── buffer.rs           # In-memory event buffer + flush
│   │   ├── visitor_id.rs       # Privacy-safe visitor hashing
│   │   ├── useragent.rs        # UA parsing (browser, OS, device)
│   │   └── geoip.rs            # IP → country/city lookup
│   │
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── parquet.rs          # Parquet write/read/compaction
│   │   ├── schema.rs           # DuckDB table definitions
│   │   └── migrations.rs       # Schema versioning
│   │
│   ├── query/
│   │   ├── mod.rs
│   │   ├── metrics.rs          # Core metric calculations
│   │   ├── breakdowns.rs       # Dimension breakdown queries
│   │   ├── timeseries.rs       # Time-bucketed aggregations
│   │   ├── funnel.rs           # window_funnel query builder
│   │   ├── retention.rs        # retention query builder
│   │   ├── sessions.rs         # sessionize-based session queries
│   │   ├── sequences.rs        # sequence_match/count query builder
│   │   └── flow.rs             # sequence_next_node query builder
│   │
│   ├── api/
│   │   ├── mod.rs
│   │   ├── stats.rs            # GET /api/stats/* handlers
│   │   ├── funnels.rs          # GET /api/funnels handlers
│   │   ├── auth.rs             # Authentication middleware
│   │   └── errors.rs           # API error types
│   │
│   └── dashboard/
│       ├── mod.rs
│       └── assets/             # Static SPA files (embedded in binary)
│           ├── index.html
│           ├── app.js          # Preact + HTM dashboard
│           └── style.css       # Minimal CSS
│
├── tracking/
│   └── script.js               # Tracking script (<1KB minified)
│
├── tests/
│   ├── integration/
│   │   ├── ingest_test.rs      # End-to-end ingestion tests
│   │   ├── query_test.rs       # Query correctness tests
│   │   └── api_test.rs         # HTTP API tests
│   └── fixtures/               # Test data (Parquet files)
│
├── benches/
│   ├── ingest_bench.rs         # Ingestion throughput benchmarks
│   └── query_bench.rs          # Query latency benchmarks
│
└── docs/
    ├── book.toml               # mdBook configuration
    └── src/
        ├── SUMMARY.md
        ├── index.md
        ├── getting-started.md
        ├── self-hosting.md
        ├── tracking-script.md
        ├── dashboard.md
        ├── behavioral-analytics.md  # Funnel, retention, sequences
        ├── api-reference.md
        ├── comparison.md           # vs Plausible, vs PostHog, vs Matomo
        └── architecture.md
```

## Key SQL Queries (Using `behavioral` Extension)

### Bounce Rate

```sql
-- Uses sessionize to derive sessions, then calculates bounce rate
WITH sessions AS (
    SELECT
        visitor_id,
        sessionize(timestamp, INTERVAL '30 minutes') OVER (
            PARTITION BY visitor_id ORDER BY timestamp
        ) AS session_id,
        pathname
    FROM events
    WHERE site_id = ? AND timestamp >= ? AND timestamp < ?
)
SELECT
    COUNT(DISTINCT CASE WHEN page_count = 1 THEN session_key END)::FLOAT
    / NULLIF(COUNT(DISTINCT session_key), 0) AS bounce_rate
FROM (
    SELECT
        visitor_id || '-' || session_id AS session_key,
        COUNT(*) AS page_count
    FROM sessions
    WHERE event_name = 'pageview'
    GROUP BY visitor_id, session_id
);
```

### Conversion Funnel

```sql
-- Multi-step funnel with window_funnel
SELECT
    steps,
    COUNT(*) AS visitors
FROM (
    SELECT
        visitor_id,
        window_funnel(
            INTERVAL '1 day',
            timestamp,
            pathname = '/landing',
            pathname = '/pricing',
            event_name = 'signup',
            event_name = 'payment'
        ) AS steps
    FROM events
    WHERE site_id = ? AND timestamp >= ? AND timestamp < ?
    GROUP BY visitor_id
)
GROUP BY steps
ORDER BY steps;
```

### Retention Cohorts

```sql
-- Weekly retention over 8 weeks
SELECT
    DATE_TRUNC('week', first_seen) AS cohort_week,
    retention(
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen),
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '1 week',
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '2 weeks',
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '3 weeks',
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '4 weeks',
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '5 weeks',
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '6 weeks',
        DATE_TRUNC('week', timestamp) = DATE_TRUNC('week', first_seen) + INTERVAL '7 weeks'
    ) AS retained
FROM events e
JOIN (
    SELECT visitor_id, MIN(timestamp) AS first_seen
    FROM events
    WHERE site_id = ?
    GROUP BY visitor_id
) f ON e.visitor_id = f.visitor_id
WHERE e.site_id = ?
GROUP BY cohort_week
ORDER BY cohort_week;
```

### User Journey Pattern Detection

```sql
-- Find users who viewed pricing then signed up within 1 hour
SELECT
    COUNT(*) FILTER (WHERE matched) AS converting_visitors,
    COUNT(*) AS total_visitors,
    COUNT(*) FILTER (WHERE matched)::FLOAT / COUNT(*) AS conversion_rate
FROM (
    SELECT
        visitor_id,
        sequence_match(
            '(?1).*(?t<=3600)(?2)',
            timestamp,
            pathname = '/pricing',
            event_name = 'signup'
        ) AS matched
    FROM events
    WHERE site_id = ? AND timestamp >= ? AND timestamp < ?
    GROUP BY visitor_id
);
```

### Flow Analysis (Next Page After Pricing)

```sql
-- What page do users visit after /pricing?
SELECT
    next_page,
    COUNT(*) AS visitors
FROM (
    SELECT
        visitor_id,
        sequence_next_node(
            'forward', 'first_match',
            timestamp,
            pathname,
            TRUE,        -- base condition (always true, match any event)
            pathname = '/pricing'
        ) AS next_page
    FROM events
    WHERE site_id = ? AND timestamp >= ? AND timestamp < ?
    GROUP BY visitor_id
)
WHERE next_page IS NOT NULL
GROUP BY next_page
ORDER BY visitors DESC
LIMIT 10;
```

## Comparison: DuckDB vs ClickHouse for Web Analytics

| Aspect | DuckDB (this project) | ClickHouse (Plausible) |
|---|---|---|
| Deployment | Single binary, no server | Separate server process |
| Memory footprint | ~50-200 MB | 2+ GB recommended |
| Operational complexity | Zero (embedded) | Medium (server management) |
| Storage format | Parquet files (portable) | Proprietary MergeTree |
| Concurrent writes | Single-writer | Multi-writer |
| Query performance | Excellent for analytics | Excellent for analytics |
| Behavioral functions | Via `behavioral` extension | Built-in parametric functions |
| Scale ceiling | ~100M events/site practical | Billions of events |
| Backup strategy | Copy Parquet files | ClickHouse backup tooling |
| Python/notebook access | Direct (same DuckDB library) | Client library required |

### Target Scale

This project targets **small to medium sites** (up to ~10M pageviews/month).
For sites exceeding this, ClickHouse-based solutions are more appropriate.

DuckDB's strengths (embedded, zero-ops, Parquet portability) align with the
self-hosting use case where operators want minimal infrastructure.

## Development Plan

### Sprint 1: Foundation (Weeks 1-2)

1. Initialize Rust project with Axum
2. Implement event ingestion endpoint (`POST /api/event`)
3. Implement in-memory event buffer with periodic Parquet flush
4. Create tracking script (JavaScript, <1KB)
5. Set up DuckDB with `behavioral` extension loading
6. Implement visitor ID hashing (IP + UA + daily salt)
7. Basic date-partitioned Parquet storage

### Sprint 2: Core Dashboard (Weeks 3-4)

1. Implement core metric queries (visitors, pageviews, bounce rate, duration)
2. Implement dimension breakdown queries (pages, sources, countries, browsers)
3. Implement time-series aggregation (hourly/daily buckets)
4. Build minimal dashboard UI (Preact + HTM)
5. Time-series chart component
6. Breakdown table component
7. Date range picker

### Sprint 3: Behavioral Analytics (Weeks 5-6)

1. Funnel builder UI + `window_funnel` query integration
2. Retention cohort table UI + `retention` query integration
3. Session analytics using `sessionize`
4. Journey pattern detection using `sequence_match`
5. Flow visualization using `sequence_next_node`

### Sprint 4: Production Readiness (Weeks 7-8)

1. Authentication (username/password)
2. Multi-site management
3. Docker image + docker-compose
4. User-Agent parsing
5. GeoIP integration (MaxMind GeoLite2)
6. Parquet compaction (merge small files)
7. Data retention policies
8. Documentation (mdBook)

## CLAUDE.md for New Repository

The new repository should include a CLAUDE.md with:

1. **Project overview**: DuckDB-based web analytics using `behavioral` extension
2. **Architecture**: Single-process, embedded DuckDB, Parquet storage
3. **Build & test**: `cargo build`, `cargo test`, Docker build
4. **Key SQL queries**: Document the behavioral extension usage patterns
5. **Code quality standards**: Same rigor as `duckdb-behavioral` — zero clippy
   warnings, comprehensive tests, honest documentation
6. **Session protocol**: Same mandatory verification requirements

## Success Criteria

1. **Feature parity with basic Plausible**: Pageviews, visitors, bounce rate,
   sources, pages, countries, browsers, device types, real-time counter
2. **Behavioral analytics beyond Plausible**: Funnel analysis, retention
   cohorts, sequence patterns, flow analysis — features that require the
   `behavioral` extension and have no equivalent in basic Plausible
3. **Single binary deployment**: `docker run` or download-and-run with zero
   external dependencies
4. **Benchmarked**: Published query latency and ingestion throughput numbers
5. **Documented**: Complete user and developer documentation
6. **Directly comparable**: Published comparison against Plausible on the
   same dataset, showing equivalent core metrics and superior behavioral
   analytics capabilities

## License Considerations

- **AGPL-3.0**: Matches Plausible CE, ensures self-hosted deployments
  contribute improvements back. Network use clause covers SaaS.
- **MIT**: Matches `duckdb-behavioral`, maximizes adoption. No copyleft.
- **Recommendation**: Start with MIT for maximum adoption. Consider AGPL
  if a hosted service version is planned later.

## References

- [Plausible Analytics](https://github.com/plausible/analytics) — Elixir/Phoenix + ClickHouse
- [duckdb-behavioral](https://github.com/tomtom215/duckdb-behavioral) — The DuckDB extension this project showcases
- [DuckDB](https://duckdb.org/) — Embedded OLAP database
- [Axum](https://github.com/tokio-rs/axum) — Rust web framework
- [Plausible Events API](https://plausible.io/docs/events-api) — Reference for event tracking data model
