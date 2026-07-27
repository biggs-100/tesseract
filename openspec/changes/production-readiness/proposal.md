# Proposal: Production Readiness

## Intent

Tesseract está funcional pero tiene 12 áreas que bloquean producción: panics en APIs públicas, sin graceful shutdown, sin auth ni límites de recursos, sin observabilidad, y calidad insuficiente. Resolver en 4 PRs encadenados (stacked-to-main, review budget 800 líneas).

## Scope

### In Scope (4 PRs, 12 issues)

| PR | Issues | Deliverable |
|----|--------|-------------|
| **PR1 — Core Correctness** | A1 panics, A2 lock poisoning, A4 graceful shutdown, A3 WAL serialization | API robusta sin panics, shutdown limpio, errores consistentes |
| **PR2 — Security/Ops** | A5 embedding timeout, A6 auth, A7 rate limit, A8 observabilidad | Cliente HTTP con timeout/retry, auth middleware, métricas, health checks |
| **PR3 — Quality** | A9 test embedding, A10 CI audit | Tests e2e determinísticos, CI con auditoría de vulnerabilidades |
| **PR4 — Performance** | A11 HNSW locking, A12 dead code | Concurrencia real, código limpio sin advertencias suprimidas |

### Out of Scope
Clustering/HA, VQL grammar extensions, hot tier eviction real, WAL payload migration a bincode.

## Capabilities

### New Capabilities
- `api-auth`: API key + JWT middleware para HTTP y gRPC
- `observability`: OpenTelemetry (métricas Prometheus, tracing, health liveness/readiness)
- `embedding-service-test`: TestEmbeddingService determinístico (hash → vector)

### Modified Capabilities
- `http-api`: auth layer, rate limiting, timeouts
- `grpc-api`: auth interceptor
- `wal-engine`: error variant naming, serialization consistency
- `ci`: vulnerability auditing, code coverage step

## Approach

**PR1**: `assert!` → `Result` en `NormalizedVector::new`, `register_field`, `PageCache::new`. Lock poisoning con `map_err` en todos los `.lock().unwrap()`. SIGTERM/SIGINT handler en `main.rs` → llama a `StorageEngine::shutdown()`. `BincodeError` → `SerializationError` y agregar `JsonError`.

**PR2**: reqwest `Client::builder().timeout(30s)` + retry con backoff para 429/5xx. Axum middleware validando `X-API-Key` + JWT. `tower::limit::RateLimitLayer` en rutas públicas. OpenTelemetry SDK con exporter Prometheus, `#[instrument]` en pipeline, health check diferenciado.

**PR3**: Implementar `TestEmbeddingService: EmbeddingService` con hash SHA-256 → vector normalizado. Agregar `cargo deny check advisories`, `cargo audit` a CI. Configurar `cargo llvm-cov` con threshold inicial.

**PR4**: `RwLock<()>` → `parking_lot::RwLock` en HNSW. `tokio::sync::Mutex<AnyIndex>` → `tokio::sync::RwLock<AnyIndex>`. Remover `#[allow(dead_code)]` y campos no usados.

## Affected Areas

| Crate | Impact | Issues |
|-------|--------|--------|
| `tesseract-core/src/distance.rs` | Modify | A1 |
| `tesseract-core/src/topological.rs` | Modify | A1 |
| `tesseract-core/src/embedding.rs` | Modify | A5, A9 |
| `tesseract-storage/src/page_cache.rs` | Modify | A1, A2 |
| `tesseract-storage/src/engine.rs` | Modify | A1, A2, A3, A4, A11, A12 |
| `tesseract-storage/src/wal.rs` | Modify | A12 |
| `tesseract-storage/src/hot_store.rs` | Modify | A12 |
| `tesseract-api/src/http.rs` | Modify | A4, A6, A7, A8 |
| `tesseract-api/src/grpc.rs` | Modify | A6 |
| `tesseract-api/src/main.rs` | Modify | A4, A8 |
| `tesseract-cluster/src/replication.rs` | Modify | A12 |
| `tesseract-index/src/hnsw.rs` | Modify | A11 |
| `tesseract-common/src/error.rs` | Modify | A3 |
| `.github/workflows/ci.yml` | Modify | A10 |

## Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| A1: Cambio a Result rompe API existente | Med | Actualizar todos los call sites en el mismo PR. CI debe compilar limpio. |
| A4: Shutdown timeout → SIGKILL antes de completar | Bajo | Timeout configurable + warning log. Default 30s. |
| A11: Nuevo locking introduce race conditions | Alto | Tests de concurrencia. Feature flag para revertir a lock anterior. |
| A6: Auth cambia contrato de API externa | Med | Auth configurable (dev sin auth, prod con auth). Release notes. |
| PR2: OpenTelemetry overhead en latencia | Bajo | Sampling rate configurable. Medir impacto en benchmarks. |

## Rollback Plan

Cada PR es reversible independientemente:

- **PR1**: Revertir commit. Estado anterior (con panics) es conocido. WAL backward-compatible.
- **PR2**: Revertir commit. Sin auth el sistema vuelve a público. Timeouts/retry removidos.
- **PR3**: Revertir commit. Tests determinísticos no afectan producción. CI vuelve a estado anterior.
- **PR4**: Revertir commit. Performance vuelve a locking simple pero correcto.

**Nota**: No hay migración de datos ni cambios de schema en ningún PR. Rollback es seguro.

## Dependencies

- **PR1 → PR2**: graceful shutdown (A4) es prereq para operaciones seguras.
- **PR1 → PR3**: TestEmbeddingService (A9) necesita embedding module estable post-A5.
- **PR3, PR4**: Independientes de PR1/PR2. Pueden hacerse en paralelo.
- **Dentro de PR1**: A1 (panics) → A2 (poisoning) → A4 (shutdown) → A3 (serialization).

## Success Criteria

- [ ] `cargo test --workspace` pasa en cada PR
- [ ] `cargo clippy --all-targets` sin warnings nuevos
- [ ] `cargo build --release` compila sin errores
- [ ] Health endpoint responde `pass` con diagnóstico real (WAL, índice)
- [ ] WAL errors consistentes: sin `BincodeError` para payloads JSON
- [ ] Auth middleware rechaza requests sin API key o con JWT inválido
- [ ] CI bloquea en vulnerabilidades conocidas (advisories)
- [ ] Tests e2e insert + search con `TestEmbeddingService` pasan happy path
- [ ] Sin `#[allow(dead_code)]` ni `#[expect(dead_code)]` en producción
