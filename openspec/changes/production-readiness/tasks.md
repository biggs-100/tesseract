# Tasks: production-readiness

> 4 PRs stacked-to-main, 12 issues (A1–A12), review budget 800 lines/PR.
> Each task is ~30 min, each PR independently revertable.

---

## Delivery Strategy

| Dimension | Choice |
|-----------|--------|
| **Strategy** | `stacked-to-main` — each PR merges to `main` in order |
| **Review budget** | 800 lines per PR (additions + deletions) |
| **Chaining** | `force-chained` — no single-PR exception |

### Chain Dependencies

```
main ← PR1 (Core Correctness) ← PR2 (Security/Ops) ← PR3 (Quality)
                                                       ← PR4 (Performance)
```

- PR1 → PR2: graceful shutdown (A4) es prereq para operaciones seguras.
- PR1 → PR3: TestEmbeddingService (A9) necesita embedding estable post-A5.
- PR3, PR4: paralelizables después de PR2.

---

## Review Workload Forecast

| PR | Estimated additions | Estimated deletions | Net lines | Budget risk |
|----|--------------------|--------------------|-----------|-------------|
| PR1 — Core Correctness | ~580 | ~120 | ~700 | ✅ Fits |
| PR2 — Security/Ops | ~650 | ~40 | ~690 | ✅ Fits |
| PR3 — Quality | ~280 | ~30 | ~310 | ✅ Fits (low risk) |
| PR4 — Performance | ~300 | ~80 | ~380 | ✅ Fits |

**Total**: ~1,920 net lines across 4 PRs. All within budget.

---

## PR1 — Core Correctness

Issues: A1 (panics) → A2 (lock poisoning) → A4 (graceful shutdown) → A3 (WAL serialization)

### Review Workload

| Component | Lines | Detail |
|-----------|-------|--------|
| A1 — Panics to Result | ~180 | NormalizedVector + register_field + PageCache |
| A2 — Lock poisoning | ~220 | Error variant + EpisodicMemory + engine.rs 14 sites |
| A3 — Serialization rename | ~120 | Error enum rename + WAL + cold_store sites |
| A4 — Graceful shutdown | ~180 | main.rs signal + engine shutdown + integration test |
| **Total PR1** | **~700** | ✅ Within 800-line budget |

---

### Issue A1 — Panics to Result

#### PR1-T1: NormalizedVector::new → Result<Self>  `[x]`

- **Issue**: A1 (panics)
- **Archivos**: `tesseract-core/src/distance.rs`, `tesseract-common/src/error.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. En `error.rs`: agregar variant `InvalidVector(String)` con `#[error("Invalid vector: {0}")]`
  2. En `distance.rs` línea 28–32: cambiar `NormalizedVector::new()` de `pub fn new(v: Vec<f64>) -> Self` a `pub fn new(v: Vec<f64>) -> Result<Self>`;
     - Reemplazar `assert!(norm.is_finite() && norm > 0.0, ...)` con:
       ```rust
       if !norm.is_finite() || norm == 0.0 {
           return Err(Error::InvalidVector(
               "vector must be finite and non-zero".into()
           ));
       }
       ```
  3. En `distance.rs` líneas 39–42: `TryFrom<Vec<f64>>` — cambiar `Ok(Self::new(v))` → `Self::new(v).map_err(|e| e.to_string())` (el `Error` type de TryFrom es `String`)
  4. En `distance.rs` tests (líneas 88–110): actualizar los 6 call sites de `NormalizedVector::new(...)` a `NormalizedVector::new(...).unwrap()`
  5. En `distance.rs` línea 101–104: cambiar el test `#[should_panic]` (`zero_vector_panics`) a:
     ```rust
     fn zero_vector_returns_err() {
         assert!(NormalizedVector::new(vec![0.0, 0.0, 0.0]).is_err());
     }
     ```
  6. En `distance.rs` líneas 118–119, 125–126, 133–134: los call sites existentes en `CosineDistance` wrapper usan `NormalizedVector::new(...)` directamente — agregar `.unwrap()` (o convertir a `?` si están en fn que devuelve Result)
- **Test strategy**:
  - Unit test: vector `[0.0, 0.0, 0.0]` → `is_err()`
  - Unit test: vector `[f64::NAN, 1.0]` → `is_err()`
  - Unit test: vector `[1.0, 2.0, 3.0]` → `is_ok()` y L2 norm ≈ 1.0
  - Los tests existentes de `CosineDistance` siguen funcionando (agregan `.unwrap()`)
- **Estimado**: +45 líneas (error variant + fn change + test updates + call site unwraps)

#### PR1-T2: register_field → Result<()>

- **Issue**: A1 (panics)
- **Archivos**: `tesseract-core/src/topological.rs`
- **Depende de**: PR1-T1 (por consistencia del Error enum — pero puede ser independiente porque usa Error existente)
- **Descripción**:
  1. En `topological.rs` línea 384: cambiar firma de `register_field`:
     ```rust
     pub fn register_field(&mut self, field: &str, boundaries: Vec<f64>) -> Result<()>
     ```
     (importar `use tesseract_common::error::Result;` si no existe)
  2. Reemplazar `assert!(!boundaries.is_empty(), ...)` línea 385 con:
     ```rust
     if boundaries.is_empty() {
         return Err(Error::InvalidConfig("bucket boundaries must not be empty".into()));
     }
     ```
     (agregar `InvalidConfig(String)` a error.rs si no existe, o reusar `InvalidVector`)
  3. Actualizar los 3 call sites en producción (`engine.rs` línea 147 y 2 usos en el mismo archivo `topological.rs`)
  4. Actualizar call sites en benchmarks (`benches/topological.rs` línea 218)
  5. En `topological.rs` tests (líneas 1007, 1022, 1035, 1195): agregar `.unwrap()` a cada call
- **Test strategy**:
  - Unit test: `boundaries = vec![]` → `is_err()`
  - Unit test: `boundaries = vec![0.0, 1.0, 2.0]` → `is_ok()`
  - Tests existentes de bucket registrations: actualizar con `.unwrap()`
- **Estimado**: +35 líneas

#### PR1-T3: PageCache::new(0) → Result<Self>

- **Issue**: A1 (panics)
- **Archivos**: `tesseract-storage/src/page_cache.rs`, `tesseract-storage/src/engine.rs`
- **Depende de**: PR1-T1 (Error::InvalidConfig o InvalidArgument)
- **Descripción**:
  1. En `page_cache.rs` línea 37: cambiar firma:
     ```rust
     pub fn new(capacity: usize) -> Result<Self>
     ```
  2. Reemplazar línea 38 `.expect("PageCache capacity must be greater than 0")` con:
     ```rust
     let cap = NonZeroUsize::new(capacity).ok_or_else(|| {
         Error::InvalidConfig(format!("PageCache capacity must be > 0, got {capacity}"))
     })?;
     ```
  3. En `engine.rs` línea 75: `PageCache::new(config.cache.capacity)` — propagar error con `?` (el método `open` ya retorna `Result<Self>`)
  4. En `page_cache.rs` tests (líneas 106, 117, 124, 149, 175, 190, 202, 227, 232): actualizar todos los call sites agregando `.unwrap()`
  5. Línea 225–228: cambiar `#[should_panic]` a test que verifica `is_err()`
- **Test strategy**:
  - Unit test: `PageCache::new(0)` → `is_err()` con mensaje descriptivo
  - Unit test: `PageCache::new(100)` → `is_ok()`
  - Tests existentes: agregar `.unwrap()` a cada constructor
- **Estimado**: +30 líneas

---

### Issue A2 — Lock Poisoning

#### PR1-T4: Agregar Error::LockPoisoned

- **Issue**: A2 (lock poisoning)
- **Archivos**: `tesseract-common/src/error.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Agregar variant al enum `Error`:
     ```rust
     #[error("Lock poisoned: {0}")]
     LockPoisoned(String),
     ```
  2. Agregar test de display en el módulo `#[cfg(test)]`:
     ```rust
     fn lock_poisoned_display() {
         let err = Error::LockPoisoned("engine mutex".into());
         assert_eq!(err.to_string(), "Lock poisoned: engine mutex");
     }
     ```
- **Test strategy**:
  - Unit test: verificar display del nuevo variant
- **Estimado**: +10 líneas

#### PR1-T5: Lock poisoning en EpisodicMemory

- **Issue**: A2 (lock poisoning)
- **Archivos**: `tesseract-core/src/episodic.rs`
- **Depende de**: PR1-T4 (LockPoisoned variant)
- **Descripción**:
  1. Línea 32: `self.footprints.read().ok()?` → reemplazar con:
     ```rust
     let fp = self.footprints.read()
         .map_err(|e| Error::LockPoisoned(e.to_string()))?;
     ```
  2. Línea 41–44: `update_footprint` ya usa `map_err` con `ServiceError` — cambiar `ServiceError("Lock poisoned".into())` a `Error::LockPoisoned("footprints write lock".into())`
  3. El método `get_footprint` ahora retorna `Result<Option<Vec<f64>>>` en vez de `Option<Vec<f64>>` — actualizar firma y call sites
- **Test strategy**:
  - Verificar que los tests existentes pasan con el nuevo signature
  - No hay tests específicos de lock poisoning (difícil simular poisoned lock)
- **Estimado**: +15 líneas

#### PR1-T6: Lock poisoning en StorageEngine (14 sitios .lock().unwrap())

- **Issue**: A2 (lock poisoning)
- **Archivos**: `tesseract-storage/src/engine.rs`
- **Depende de**: PR1-T4 (LockPoisoned variant)
- **Descripción**:
  En total 14 sitios con `.lock().unwrap()` en `engine.rs` — todos siguen el mismo patrón:
  1. **Líneas 242–299** (6 sitios en `replay_wal` interno):
     - `centroids_lock.lock().unwrap()` → `centroids_lock.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?`
     - `correlations_lock.lock().unwrap()` → igual
     - `buckets_lock.lock().unwrap()` → igual
     - `buffer_lock.lock().unwrap()` → igual
     - `tree_lock.lock().unwrap()` → igual
  2. **Líneas 375, 384** (2 sitios en query methods):
     - `buffer_lock.lock().unwrap()` → map_err
     - `tree_lock.lock().unwrap()` → map_err
  3. **Líneas 489–491** (3 sitios en `apply_topological_bias`):
     - `centroids_lock.lock().unwrap()` → map_err
     - `correlations_lock.lock().unwrap()` → map_err
     - `buckets_lock.lock().unwrap()` → map_err
  4. **Líneas 811, 829, 849, 853** (4 sitios en methods varios):
     - `hot_buffer.as_ref().unwrap().lock().unwrap()` → pattern: primero unwrap del Option, luego map_err del lock
     - `merkle_tree.as_ref().unwrap().lock().unwrap()` → igual
  5. **IMPORTANTE**: Donde el método retorna `void`, cambiar a `Result<()>` y propagar. Donde retorna otro tipo, cambiar a `Result<T>`.
     - `apply_topological_bias` (línea 481): ya retorna `Vec<f64>` — cambiar a `Result<Vec<f64>>`
     - Actualizar todos los call sites de `apply_topological_bias`
- **Test strategy**:
  - Tests existentes de engine: deben seguir pasando (agregar `?` o `.unwrap()` en tests)
  - No hay tests específicos de poisoned Mutex (requiere hacer panic en otro thread)
- **Estimado**: +120 líneas (cambios de firma + map_err + call sites externos)

#### PR1-T6b: Lock poisoning en PageCache y ColdStore (sites .lock().expect())

- **Issue**: A2 (lock poisoning)
- **Archivos**: `tesseract-storage/src/page_cache.rs`, `tesseract-storage/src/cold_store.rs`
- **Depende de**: PR1-T4 (LockPoisoned variant), PR1-T3 (PageCache::new → Result)
- **Descripción**:
  1. **page_cache.rs** (5 sitios con `.lock().expect()`):
     - Líneas 47, 55, 61, 67, 73: reemplazar cada `.lock().expect(...)` con `lock().map_err(|e| Error::LockPoisoned(...))?`
     - Los métodos `get`, `insert`, `evict`, `remove`, `len` cambian de retornar tipos directos a `Result<T>`:
       - `fn get(&self, key: &PageKey) -> Result<Option<Page>>`
       - `fn insert(&self, key: PageKey, page: Page) -> Result<()>`
       - `fn len(&self) -> Result<usize>`
     - Actualizar call sites en `engine.rs` (accede a `cache` via `self.cache.lock().await` que envuelve PageCache en tokio::sync::Mutex)
  2. **cold_store.rs** (4 sitios con `.lock().expect()`):
     - Líneas 123, 133, 187, 193: reemplazar cada `.lock().expect(...)` con `.lock().map_err(|e| Error::LockPoisoned(...))?`
     - Los métodos involucrados ya retornan `Result<()>` — solo cambiar el error propagation
- **Test strategy**:
  - Tests de PageCache: agregar `.unwrap()` a call sites en tests
  - Tests de ColdStore: deben seguir compilando sin cambios (ya usan `?`)
- **Estimado**: +60 líneas

---

### Issue A3 — WAL Serialization

#### PR1-T7: Renombrar BincodeError → SerializationError y agregar JsonError

- **Issue**: A3 (WAL serialization)
- **Archivos**: `tesseract-common/src/error.rs`, y todos los usos de `Error::BincodeError`
- **Depende de**: (ninguna)
- **Descripción**:
  1. En `error.rs` línea 30–31:
     - Cambiar `BincodeError(String)` → `SerializationError(String)`
     - Cambiar `#[error("Bincode error: {0}")]` → `#[error("Serialization error: {0}")]`
     - Agregar nuevo variant: `JsonError(String)` con `#[error("JSON error: {0}")]`
  2. En `error.rs` línea 61–65: actualizar `From<bincode::Error>`:
     - `Error::BincodeError(e.to_string())` → `Error::SerializationError(e.to_string())`
  3. Reemplazar usos de `BincodeError` según contexto:
     - **Usos de bincode** (mantener `SerializationError`):
       - `tesseract-index/src/merkle/tree.rs` líneas 365, 374: `BincodeError` → `SerializationError`
       - `tesseract-index/src/serialization.rs` líneas 65, 97: docs comments (actualizar referencias en comentarios)
     - **Usos de JSON** (cambiar a `JsonError`):
       - `tesseract-storage/src/engine.rs` línea 225: `serde_json` → `Error::JsonError`
       - `tesseract-storage/src/engine.rs` línea 510: `serde_json` → `Error::JsonError`
       - `tesseract-storage/src/engine.rs` línea 538: `serde_json::from_slice` → `Error::JsonError`
       - `tesseract-storage/src/cold_store.rs` líneas 115, 140, 143, 177, 208: `serde_json` → `Error::JsonError`
     - **Usos de opcode mapping** (mantener `SerializationError`):
       - `tesseract-storage/src/types.rs` línea 78: unknown opcode → `Error::SerializationError`
  4. Actualizar tests existentes que matchean sobre `BincodeError`
- **Test strategy**:
  - Verificar que todos los usos de `BincodeError` fueron reemplazados (`grep -r BincodeError` retorna 0)
  - Test de display para `SerializationError` y `JsonError`
  - Tests de integración existentes deben pasar sin cambios
- **Estimado**: +60 líneas (cambios distribuidos en ~10 archivos)

#### PR1-T8: Corregir reporte de error en WAL (engine.rs)

- **Issue**: A3 (WAL serialization)
- **Archivos**: `tesseract-storage/src/engine.rs`
- **Depende de**: PR1-T7 (JsonError variant)
- **Descripción**:
  1. Línea 509–510: la deserialización de payload WAL usa `serde_json::from_slice` — cambiar:
     `.map_err(|e| Error::BincodeError(e.to_string()))?` →
     `.map_err(|e| Error::JsonError(e.to_string()))?`
  2. Verificar que no quede ningún `Error::BincodeError` en `engine.rs` después de PR1-T7
- **Test strategy**:
  - Test unitario: si es posible inyectar un payload JSON corrupto y verificar que el error es `JsonError`
- **Estimado**: +2 líneas

---

### Issue A4 — Graceful Shutdown

#### PR1-T9: Agregar shutdown signal a main.rs

- **Issue**: A4 (graceful shutdown)
- **Archivos**: `tesseract-api/src/main.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Agregar imports:
     ```rust
     use tokio::signal;
     use tracing::info;
     ```
  2. Crear función `shutdown_signal`:
     ```rust
     async fn shutdown_signal() {
         #[cfg(unix)]
         {
             let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
                 .expect("failed to install SIGTERM handler");
             let mut int = signal::unix::signal(signal::unix::SignalKind::interrupt())
                 .expect("failed to install SIGINT handler");
             tokio::select! {
                 _ = term.recv() => info!("Received SIGTERM, starting shutdown..."),
                 _ = int.recv() => info!("Received SIGINT, starting shutdown..."),
                 _ = signal::ctrl_c() => info!("Received Ctrl+C, starting shutdown..."),
             }
         }
         #[cfg(not(unix))]
         {
             signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
             info!("Received Ctrl+C, starting shutdown...");
         }
     }
     ```
  3. En `main.rs`, reemplazar `axum::serve(listener, router).await?` con:
     ```rust
     axum::serve(listener, router)
         .with_graceful_shutdown(shutdown_signal())
         .await?;
     ```
  4. Después del serve, llamar `engine.shutdown().await?` si el engine está en scope
  5. Leer `TESSERACT_SHUTDOWN_TIMEOUT_SECS` del environment (default 30) y pasarlo al engine
- **Test strategy**:
  - Test de integración: enviar SIGTERM al proceso y verificar logs de shutdown (ver PR1-T11)
- **Estimado**: +50 líneas

#### PR1-T10: Implementar StorageEngine::shutdown() real

- **Issue**: A4 (graceful shutdown)
- **Archivos**: `tesseract-storage/src/engine.rs`
- **Depende de**: PR1-T9 (shutdown signal) — funcionalmente acoplado
- **Descripción**:
  1. Modificar `shutdown()` (línea 457) para aceptar timeout y ejecutar en orden:
     ```rust
     pub async fn shutdown(&self, timeout: Duration) -> Result<()> {
         tokio::time::timeout(timeout, async {
             // 1. Persist index (ya existe, líneas 460-469)
             // 2. Drain HotBuffer
             if let Some(ref buffer_lock) = self.hot_buffer {
                 let mut buffer = buffer_lock.lock().map_err(|e| Error::LockPoisoned(...))?;
                 buffer.drain()?;
                 info!("HotBuffer drained");
             }
             // 3. Flush WAL (ya existe, línea 471)
             self.wal.flush().await?;
             // 4. Cerrar cold store si aplica
             info!("StorageEngine shut down");
             Ok(())
         }).await
           .map_err(|_| Error::ServiceError("shutdown timed out".into()))?
     }
     ```
  2. Agregar `shutdown_config: ShutdownConfig` al `StorageConfig` con `timeout_secs: u64`
  3. Leer `TESSERACT_SHUTDOWN_TIMEOUT_SECS` durante inicialización del engine
  4. Verificar que `HnswIndex` tiene método `save()` o similar para persistence (ya existe, línea 467)
  5. En `main.rs`: pasar `shutdown_config` al construir `StorageConfig`
- **Test strategy**:
  - Integration test: shutdown con datos insertados, verificar WAL flush y HotBuffer drain (PR1-T11)
- **Estimado**: +60 líneas

#### PR1-T11: Test de integración para shutdown

- **Issue**: A4 (graceful shutdown)
- **Archivos**: `tesseract-storage/tests/shutdown_integration.rs` (nuevo)
- **Depende de**: PR1-T10 (shutdown implementado)
- **Descripción**:
  1. Crear archivo de test con:
     ```rust
     #[tokio::test]
     async fn shutdown_flushes_wal_and_hotbuffer() {
         // Setup: crear StorageEngine con temp dir
         // Insertar algunos vectores
         // Shutdown con timeout 10s
         // Verificar que shutdown retorna Ok
         // Reabrir engine y verificar que los datos están
     }

     #[tokio::test]
     async fn shutdown_timeout_logs_warning() {
         // Setup: engine con shutdown_timeout = 1s
         // Insertar datos para que el drain tome tiempo
         // Verificar que timeout se maneja gracefulmente
     }
     ```
- **Test strategy**:
  - Integration test con directorio temporal
  - Verificar post-shutdown que WAL y HotBuffer están consistentes
  - Usar `tracing-test` o log assertions para verificar mensajes de shutdown
- **Estimado**: +90 líneas

---

## PR2 — Security/Ops

Issues: A5 (embedding timeout) → A6 (auth) → A7 (rate limits) → A8 (observability)

### Review Workload

| Component | Lines | Detail |
|-----------|-------|--------|
| A5 — Embedding timeout + retry | ~110 | reqwest builder + retry loop + config |
| A6 — Auth module + middleware | ~260 | auth.rs + http middleware + gRPC interceptor + deps |
| A7 — Rate limiting + query timeout | ~130 | Sliding window + axum layer + executor timeout |
| A8 — Observability | ~190 | Health endpoints + Prometheus + tracing + log format |
| **Total PR2** | **~690** | ✅ Within 800-line budget |

---

### Issue A5 — Embedding Timeout + Retry

#### PR2-T1: Agregar timeout al cliente HTTP de embeddings

- **Issue**: A5 (embedding timeout)
- **Archivos**: `tesseract-core/src/embedding.rs`, `tesseract-core/src/embeddings.rs` (si existe)
- **Depende de**: (ninguna)
- **Descripción**:
  1. Crear struct `OpenAIEmbeddingConfig`:
     ```rust
     pub struct OpenAIEmbeddingConfig {
         pub timeout_secs: u64,
         pub max_retries: u32,
         pub base_delay_ms: u64,
     }
     ```
  2. Agregar `TESSERACT_EMBEDDING_TIMEOUT_SECS` (default 30) al constructor
  3. Cambiar `reqwest::Client::new()` a `reqwest::Client::builder().timeout(Duration::from_secs(config.timeout_secs)).build()?`
  4. Pasar `config` al `OpenAIEmbeddingService::new()`
- **Test strategy**:
  - Unit test (mock HTTP): timeout con servidor lento → `Err(Error::ServiceError)`
  - Test con `TESSERACT_EMBEDDING_TIMEOUT_SECS=5` verifica que se usa el valor configurado
- **Estimado**: +40 líneas

#### PR2-T2: Agregar retry con exponential backoff

- **Issue**: A5 (embedding timeout)
- **Archivos**: `tesseract-core/src/embedding.rs`
- **Depende de**: PR2-T1 (config struct)
- **Descripción**:
  1. En `embed()` o el método que llama a la API de OpenAI:
     ```rust
     pub async fn embed(&self, text: &str) -> Result<Vec<f64>> {
         let mut last_error = None;
         for attempt in 0..=self.config.max_retries {
             if attempt > 0 {
                 let delay = Duration::from_millis(
                     self.config.base_delay_ms * 2u64.pow(attempt as u32 - 1)
                 );
                 tokio::time::sleep(delay).await;
             }
             match self.call_openai(text).await {
                 Ok(vec) => return Ok(vec),
                 Err(e) if e.is_retryable() => { last_error = Some(e); }
                 Err(e) => return Err(e),
             }
         }
         Err(last_error.unwrap_or_else(|| Error::ServiceError("embedding retries exhausted".into())))
     }
     ```
  2. Implementar helper `is_retryable()` en Error o directamente en el embedding service:
     - Retry en HTTP 429 (Rate Limit) y 5xx
     - No retry en 4xx (excepto 429)
  3. Agregar env var `TESSERACT_EMBEDDING_RETRY_MAX` (default 3)
- **Test strategy**:
  - Test con mock que retorna 429 tres veces → `Err` después de 3 retries
  - Test con mock que retorna 429 luego 200 → `Ok` (retry exitoso)
  - Test con mock que retorna 400 → `Err` inmediato (no retry)
- **Estimado**: +70 líneas

---

### Issue A6 — Authentication

#### PR2-T3: Crear módulo auth.rs con trait AuthProvider

- **Issue**: A6 (auth)
- **Archivos**: `tesseract-api/src/auth.rs` (nuevo), `tesseract-api/Cargo.toml`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Crear `tesseract-api/src/auth.rs`:
     ```rust
     use axum::http::Request;
     use std::collections::HashMap;

     #[derive(Debug, Clone)]
     pub struct Claims {
         pub sub: String,
         pub role: String,
         pub exp: u64,
     }

     pub trait AuthProvider: Send + Sync {
         fn authenticate(&self, req: &Request<axum::body::Body>) -> Result<Claims, AuthError>;
     }

     pub enum AuthError {
         MissingCredentials,
         InvalidCredentials(String),
         ExpiredToken,
     }

     pub struct ApiKeyAuth {
         keys: HashMap<String, Claims>,
     }

     impl ApiKeyAuth {
         pub fn new(keys_csv: &str) -> Self {
             // Parsear "key1:role1,key2:role2" en HashMap
         }
     }

     impl AuthProvider for ApiKeyAuth { ... }

     pub struct JwtAuth {
         secret: String,
     }

     impl JwtAuth {
         pub fn new(secret: &str) -> Self { ... }
     }

     impl AuthProvider for JwtAuth { ... }
     ```
  2. Agregar `jsonwebtoken` a `tesseract-api/Cargo.toml` dependencies
  3. Config: leer `TESSERACT_AUTH_MODE` (none/api-key/jwt/both), `TESSERACT_JWT_SECRET`, `TESSERACT_API_KEYS`
- **Test strategy**:
  - Unit test: `ApiKeyAuth` con key válida → Ok, key inválida → Err
  - Unit test: `JwtAuth` con token válido → Ok, token expirado → Err
  - Unit test: `JwtAuth` con firma inválida → Err
- **Estimado**: +120 líneas

#### PR2-T4: Axum middleware para auth

- **Issue**: A6 (auth)
- **Archivos**: `tesseract-api/src/http.rs`, `tesseract-api/src/auth.rs`
- **Depende de**: PR2-T3 (AuthProvider trait)
- **Descripción**:
  1. Crear middleware layer:
     ```rust
     pub fn auth_middleware(provider: Arc<dyn AuthProvider>) -> AuthLayer { ... }
     ```
  2. El layer extrae `X-API-Key` o `Authorization: Bearer <jwt>` del header
  3. Rutas públicas excluidas de auth: `/health/*`, `/metrics`
  4. Si auth mode es `"none"`, el layer no se registra
  5. En `http.rs`: wrap routes con auth layer condicional
- **Test strategy**:
  - Integration test HTTP: request con X-API-Key válida → 200
  - Integration test: request sin header → 401
  - Integration test: request con JWT inválido → 401
  - Integration test: request a /health/liveness sin auth → 200 (pública)
- **Estimado**: +70 líneas

#### PR2-T5: Interceptor gRPC para auth

- **Issue**: A6 (auth)
- **Archivos**: `tesseract-api/src/grpc.rs`, `tesseract-api/src/auth.rs`
- **Depende de**: PR2-T3 (AuthProvider trait)
- **Descripción**:
  1. Crear interceptor tonic:
     ```rust
     pub fn auth_interceptor(provider: Arc<dyn AuthProvider>) -> tonic::service::Interceptor { ... }
     ```
  2. Extraer `x-api-key` o `authorization` de metadata gRPC
  3. Feature-gated con `#[cfg(feature = "grpc")]`
  4. Si auth mode es `"none"`, interceptor pasa todas las requests
  5. Error: retornar `tonic::Status::unauthenticated("missing credentials")`
- **Test strategy**:
  - Integration test: request gRPC sin metadata → UNAUTHENTICATED
  - Integration test: request gRPC con API key válida → Ok
- **Estimado**: +60 líneas

---

### Issue A7 — Rate Limiting

#### PR2-T6: Rate limiting por IP

- **Issue**: A7 (rate limits)
- **Archivos**: `tesseract-api/src/rate_limiter.rs` (nuevo), `tesseract-api/src/http.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Crear módulo `rate_limiter.rs`:
     ```rust
     use std::collections::HashMap;
     use std::net::IpAddr;
     use tokio::sync::RwLock;

     struct SlidingWindow {
         window_start: Instant,
         count: u64,
     }

     pub struct RateLimiter {
         windows: RwLock<HashMap<IpAddr, SlidingWindow>>,
         max_requests: u64,
         window_duration: Duration,
     }

     impl RateLimiter {
         pub fn new(max_rpm: u64) -> Self { ... }
         pub async fn check(&self, ip: IpAddr) -> Result<(), RateLimitExceeded> { ... }
     }
     ```
  2. Implementar como Tower Layer (Service):
     - Extraer IP de `X-Forwarded-For` header o `req.extensions().get::<SocketAddr>()`
     - Sliding window: si `now - window_start > window_duration` → resetear ventana
     - Si `count > max_requests` → retornar HTTP 429 con `Retry-After`
  3. En `http.rs`: agregar layer de rate limiter sobre rutas no-públicas
  4. Config: `TESSERACT_RATE_LIMIT_RPM` (default 100)
- **Test strategy**:
  - Unit test: rate limiter permite hasta RPM requests
  - Unit test: request excedente → 429
  - Integration test HTTP: 101 requests → 1 es 429
  - Integration test: 429 response incluye header `Retry-After`
- **Estimado**: +100 líneas

#### PR2-T7: Timeout implícito para queries sin WITHIN

- **Issue**: A7 (rate limits)
- **Archivos**: `tesseract-vql/src/executor.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. En el método `execute()` del query executor:
     ```rust
     pub async fn execute(&self, query: &Query) -> Result<QueryResult> {
         let timeout = self.config.query_timeout;
         match tokio::time::timeout(timeout, self.run_query(query)).await {
             Ok(result) => result,
             Err(_) => Err(Error::ServiceError("query timed out".into())),
         }
     }
     ```
  2. Solo aplicar timeout si la query no especifica `WITHIN` explícitamente
  3. Leer `TESSERACT_QUERY_TIMEOUT_SECS` (default 30) del config
- **Test strategy**:
  - Unit test con mock slow query: timeout se dispara
  - Test sleep + timeout corto (1s)
- **Estimado**: +30 líneas

---

### Issue A8 — Observability

#### PR2-T8: Endpoints /health

- **Issue**: A8 (observability)
- **Archivos**: `tesseract-api/src/http.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Agregar handler `GET /health/liveness`:
     ```rust
     async fn health_liveness() -> Json<serde_json::Value> {
         Json(serde_json::json!({"status": "pass"}))
     }
     ```
  2. Agregar handler `GET /health/readiness`:
     ```rust
     async fn health_readiness(
         State(engine): State<Arc<StorageEngine>>,
     ) -> Result<Json<serde_json::Value>, StatusCode> {
         match engine.is_ready() {
             Ok(diag) => Ok(Json(serde_json::json!({"status": "pass", "components": diag}))),
             Err(reason) => Err((StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
                 "status": "fail",
                 "reason": reason
             }))).into()),
         }
     }
     ```
  3. Agregar `StorageEngine::is_ready()` → `Result<HashMap<String, bool>>` que verifica:
     - WAL: `self.wal.is_open()`
     - Index: `self.index.is_some()`
     - HotBuffer: alive
  4. En `engine.rs`: implementar `is_ready()` con verificaciones reales
- **Test strategy**:
  - Integration test HTTP: `GET /health/liveness` → 200 `{"status": "pass"}`
  - Integration test: `GET /health/readiness` con engine normal → 200 con components
- **Estimado**: +80 líneas

#### PR2-T9: Métricas Prometheus con OpenTelemetry

- **Issue**: A8 (observability)
- **Archivos**: `tesseract-api/src/http.rs`, `tesseract-api/src/main.rs`, `tesseract-api/Cargo.toml`
- **Depende de**: PR2-T8 (estructura de main.rs)
- **Descripción**:
  1. Agregar a `tesseract-api/Cargo.toml`:
     ```toml
     opentelemetry = { version = "0.27", features = ["metrics"] }
     opentelemetry-prometheus = "0.27"
     opentelemetry_sdk = { version = "0.27", features = ["metrics"] }
     ```
  2. En `main.rs`: inicializar OTel metrics provider:
     ```rust
     let meter = global::meter("tesseract");
     let queries_counter = meter.u64_counter("queries_total").init();
     let query_duration = meter.f64_histogram("query_duration_seconds").init();
     // etc.
     ```
  3. En `http.rs`: agregar endpoint `GET /metrics` que exporta Prometheus:
     ```rust
     async fn metrics() -> (StatusCode, String) {
         let encoder = opentelemetry_prometheus::PrometheusEncoder::new();
         let mut buf = vec![];
         encoder.encode(&global::meter_provider().unwrap().collect(), &mut buf).unwrap();
         (StatusCode::OK, String::from_utf8(buf).unwrap())
     }
     ```
  4. Métricas a exponer:
     - `queries_total` (counter)
     - `query_duration_seconds` (histogram, P50/P95/P99)
     - `inserts_total` (counter)
     - `index_size` (gauge)
     - `hotbuffer_size` (gauge)
  5. Instrumentar el pipeline de query para reportar duración y resultados
- **Test strategy**:
  - Integration test HTTP: `GET /metrics` → 200 en formato Prometheus
  - Verificar que las métricas existen en el output
- **Estimado**: +80 líneas

#### PR2-T10: Tracing con #[instrument] y structured logging

- **Issue**: A8 (observability)
- **Archivos**: `tesseract-vql/src/executor.rs`, `tesseract-vql/src/planner.rs`, `tesseract-api/src/main.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. En `tesseract-vql/src/executor.rs`: agregar `#[instrument(skip(self))]` en:
     - `execute(&self, query: &Query) -> Result<QueryResult>`
     - `run_query(&self, query: &Query) -> Result<QueryResult>`
  2. En `tesseract-vql/src/planner.rs`: agregar `#[instrument]` en:
     - `plan(&self, query: &Query) -> Result<Plan>`
  3. En `tesseract-api/src/main.rs`: configurar logging:
     ```rust
     let log_format = std::env::var("TESSERACT_LOG_FORMAT").unwrap_or_else(|_| "text".into());
     if log_format == "json" {
         tracing_subscriber::fmt().json().init();
     } else {
         tracing_subscriber::fmt().init();
     }
     ```
  4. Si se usa OpenTelemetry, conectar tracing con OTel:
     ```rust
     let tracer = opentelemetry_jaeger::new_pipeline().install_simple()?;
     let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
     tracing_subscriber::registry().with(telemetry).init();
     ```
  5. Reemplazar `println!` y `eprintln!` existentes con `tracing::info!`, `tracing::warn!`, etc.
- **Test strategy**:
  - Test de logging: verificar que `TESSERACT_LOG_FORMAT=json` produce JSON válido
  - No hay tests automáticos para spans individuales (verificar en desarrollo)
- **Estimado**: +50 líneas

---

## PR3 — Quality

Issues: A9 (test embedding) → A10 (CI audit)

### Review Workload

| Component | Lines | Detail |
|-----------|-------|--------|
| A9 — TestEmbeddingService | ~150 | New service + feature gate + E2E test |
| A10 — CI audit + coverage | ~160 | cargo-deny config + CI jobs + cargo-llvm-cov |
| **Total PR3** | **~310** | ✅ Well within 800-line budget |

---

### Issue A9 — Test Embedding Service

#### PR3-T1: Implementar TestEmbeddingService `[x]`

- **Issue**: A9 (test embedding)
- **Archivos**: `tesseract-core/src/test_embedding.rs` (nuevo), `tesseract-core/Cargo.toml`, `tesseract-core/src/lib.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Crear archivo `tesseract-core/src/test_embedding.rs`:
     ```rust
     use sha2::{Sha256, Digest};
     use tesseract_common::error::Result;

     #[cfg(feature = "test-embedding")]
     pub struct TestEmbeddingService {
         dim: usize,
     }

     #[cfg(feature = "test-embedding")]
     impl TestEmbeddingService {
         pub fn new(dim: usize) -> Self {
             Self { dim }
         }
     }

     #[cfg(feature = "test-embedding")]
     impl EmbeddingService for TestEmbeddingService {
         fn embed(&self, text: &str) -> Result<Vec<f64>> {
             let hash = Sha256::digest(text.as_bytes());
             let mut vec: Vec<f64> = hash.iter()
                 .take(self.dim)
                 .map(|&b| b as f64)
                 .collect();
             // L2 normalize
             let norm = vec.iter().map(|x| x * x).sum::<f64>().sqrt();
             for x in &mut vec { *x /= norm; }
             Ok(vec)
         }
     }
     ```
  2. En `tesseract-core/Cargo.toml`: agregar feature:
     ```toml
     [features]
     test-embedding = ["dep:sha2"]
     ```
  3. Agregar `sha2` como dependencia opcional
  4. En `tesseract-core/src/lib.rs`: agregar `#[cfg(feature = "test-embedding")] pub mod test_embedding;`
- **Test strategy**:
  - Unit test: `embed("hello")` dos veces → mismo vector (determinismo)
  - Unit test: vector resultante tiene L2 norm ≈ 1.0
  - Unit test: `embed("cat")` y `embed("dog")` producen vectores diferentes
- **Estimado**: +70 líneas

#### PR3-T2: Test e2e completo con TestEmbeddingService `[x]`

- **Issue**: A9 (test embedding)
- **Archivos**: `tesseract-core/tests/e2e_test_embedding.rs` (nuevo, o en `tesseract-api/tests/`)
- **Depende de**: PR3-T1 (TestEmbeddingService)
- **Descripción**:
  1. Crear test e2e:
     ```rust
     #[cfg(feature = "test-embedding")]
     #[tokio::test]
     async fn insert_and_find_similarity() {
         // Crear engine con TestEmbeddingService
         // INSERT vector with id "vec1"
         // FIND SIMILARITY(query, 5)
         // Verificar que vec1 aparece en resultados con score > 0
         // Probar metadata filter
     }
     ```
  2. Usar `StorageEngine::open()` con config mínima
  3. Probar: insert → search → verify deterministic results
  4. Probar variante con metadata filters
  5. Probar: search sin matching data → resultados vacíos
- **Test strategy**:
  - E2E test: INSERT + FIND SIMILARITY → resultado no vacío
  - E2E test: INSERT con metadata + FILTER por metadata → match exacto
  - E2E test: search sin datos → resultados vacíos
- **Estimado**: +90 líneas

---

### Issue A10 — CI Hardening

#### PR3-T3: Agregar cargo-deny advisories a CI `[x]`

- **Issue**: A10 (CI audit)
- **Archivos**: `.github/workflows/ci.yml`, `.cargo/deny.toml` (nuevo, si no existe)
- **Depende de**: (ninguna)
- **Descripción**:
  1. Verificar si `.cargo/deny.toml` existe; si no, crearlo con config básica:
     ```toml
     [advisories]
     db-path = "~/.cargo/advisory-db"
     db-urls = ["https://github.com/rustsec/advisory-db"]
     ```
  2. En `.github/workflows/ci.yml`: agregar job:
     ```yaml
     audit:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: dtolnay/rust-toolchain@stable
         - uses: actions/cache@v4
           with:
             path: ~/.cargo/advisory-db
             key: advisory-db-${{ hashFiles('**/Cargo.lock') }}
         - uses: taiki-e/install-action@cargo-deny
         - run: cargo deny check advisories
     ```
- **Test strategy**:
  - Verificar que `cargo deny check advisories` pasa localmente
  - No hay test automático para CI (verificar en GitHub después del merge)
- **Estimado**: +30 líneas

#### PR3-T4: Agregar cobertura con cargo-llvm-cov a CI `[x]`

- **Issue**: A10 (CI audit)
- **Archivos**: `.github/workflows/ci.yml`
- **Depende de**: (ninguna)
- **Descripción**:
  1. En `.github/workflows/ci.yml`: agregar job:
     ```yaml
     coverage:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: dtolnay/rust-toolchain@stable
           with:
             components: llvm-tools-preview
         - uses: taiki-e/install-action@cargo-llvm-cov
         - run: cargo llvm-cov --workspace --exclude tesseract-pg --html
         - uses: actions/upload-artifact@v4
           with:
             name: coverage-report
             path: target/llvm-cov/html/
     ```
  2. Agregar step que verifica threshold 70% como warning:
     ```yaml
     - name: Check coverage threshold
       run: |
         cargo llvm-cov --workspace --exclude tesseract-pg --json > cov.json
         python3 -c "
         import json
         with open('cov.json') as f:
             data = json.load(f)
         rate = data['data'][0]['totals']['percent_covered']
         print(f'Coverage: {rate:.1f}%')
         if rate < 70.0:
             print('::warning::Coverage below 70% threshold')
         "
     ```
- **Test strategy**:
  - Verificar que `cargo llvm-cov` corre localmente (puede tardar)
  - Coverage report se genera como artifact de CI
- **Estimado**: +40 líneas

---

## PR4 — Performance

Issues: A11 (HNSW locking) → A12 (dead code)

### Review Workload

| Component | Lines | Detail |
|-----------|-------|--------|
| A11 — HNSW locking | ~260 | parking_lot RwLock + tokio RwLock + feature flag + concurrency tests |
| A12 — Dead code | ~120 | Remove #[allow(dead_code)] + unused fields + unused structs |
| **Total PR4** | **~380** | ✅ Within 800-line budget |

---

### Issue A11 — HNSW Locking

#### PR4-T1: HNSW RwLock → parking_lot::RwLock

- **Issue**: A11 (HNSW locking)
- **Archivos**: `tesseract-index/src/hnsw.rs`, `tesseract-index/Cargo.toml`
- **Depende de**: (ninguna)
- **Descripción**:
  1. En `tesseract-index/Cargo.toml`: agregar:
     ```toml
     [dependencies]
     parking_lot = "0.12"

     [features]
     legacy-locking = []
     ```
  2. En `hnsw.rs`: encontrar todos los `std::sync::RwLock<()>` y reemplazar condicionalmente:
     ```rust
     #[cfg(not(feature = "legacy-locking"))]
     use parking_lot::RwLock;
     #[cfg(feature = "legacy-locking")]
     use std::sync::RwLock;
     ```
  3. Cambiar el tipo del campo `lock` en `HnswIndex`:
     ```rust
     lock: RwLock<()>,  // Ahora es parking_lot::RwLock por defecto
     ```
  4. `parking_lot::RwLock` no tiene método `lock().unwrap()` — usar `read()`/`write()` directamente:
     - `self.lock.read()` (no retorna Result, parking_lot no poison)
     - `self.lock.write()` (no retorna Result)
  5. Ajustar todos los accesos al lock (aproximadamente 8–15 sitios dependiendo del archivo)
- **Test strategy**:
  - Build test: `cargo build --features legacy-locking` compila
  - Build test: `cargo build` (sin feature) compila con parking_lot
  - Tests existentes de HNSW pasan con ambos modes
- **Estimado**: +60 líneas

#### PR4-T2: StorageEngine Mutex → tokio::sync::RwLock

- **Issue**: A11 (HNSW locking)
- **Archivos**: `tesseract-storage/src/engine.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. Cambiar tipo de `index` de `Option<Mutex<AnyIndex>>` a `Option<RwLock<AnyIndex>>`:
     ```rust
     index: Option<RwLock<AnyIndex>>,  // tokio::sync::RwLock
     ```
     ```rust
     #[cfg(not(feature = "legacy-locking"))]
     use tokio::sync::RwLock;
     #[cfg(feature = "legacy-locking")]
     use tokio::sync::Mutex;
     ```
  2. Donde se adquiere el lock para **lectura** (search, queries): `idx.lock().await` → `idx.read().await`
  3. Donde se adquiere el lock para **escritura** (insert, build): `idx.lock().await` → `idx.write().await`
  4. Identificar sitios correctos:
     - `read` operations: search, query, compute_distance, apply_topological_bias
     - `write` operations: insert, delete, rebuild_index
  5. Feature flag: si `legacy-locking` está activado, usar Mutex con lock.await (serializa todo)
- **Test strategy**:
  - Tests existentes pasan con RwLock
  - `cargo build --features legacy-locking` compila
- **Estimado**: +60 líneas

#### PR4-T3: Tests de concurrencia

- **Issue**: A11 (HNSW locking)
- **Archivos**: `tesseract-index/tests/concurrent.rs` (nuevo)
- **Depende de**: PR4-T1, PR4-T2 (nuevos locks)
- **Descripción**:
  1. Crear archivo de test:
     ```rust
     #[tokio::test]
     async fn concurrent_reads_with_write() {
         let index = Arc::new(HnswIndex::new(config));
         let mut handles = vec![];

         // 10 readers
         for _ in 0..10 {
             let idx = index.clone();
             handles.push(tokio::spawn(async move {
                 for _ in 0..100 {
                     let _ = idx.search(&query, 10).await;
                 }
             }));
         }

         // 1 writer
         let idx = index.clone();
         handles.push(tokio::spawn(async move {
             for i in 0..100 {
                 idx.insert(vec![i as f64; 128]).await.unwrap();
             }
         }));

         for h in handles {
             h.await.unwrap();
         }
     }

     #[tokio::test]
     async fn no_deadlock_on_concurrent_access() {
         // Stress test: interleave reads/writes from multiple threads
         // Verify no timeout or deadlock after 5 seconds
     }
     ```
  2. Test con `#[cfg(not(feature = "legacy-locking"))]` (solo tiene sentido con el nuevo locking)
- **Test strategy**:
  - Concurrent stress test: 10 readers + 1 writer, verify no deadlock
  - Timeout de seguridad: fail si el test tarda > 10s
- **Estimado**: +90 líneas

---

### Issue A12 — Dead Code

#### PR4-T4: Remover #[allow(dead_code)] y campos no usados

- **Issue**: A12 (dead code)
- **Archivos**: `tesseract-storage/src/engine.rs`, `tesseract-storage/src/hot_store.rs`, `tesseract-storage/src/wal.rs`, `tesseract-cluster/src/replication.rs`
- **Depende de**: (ninguna)
- **Descripción**:
  1. **engine.rs línea 36**: `#[allow(dead_code)]` en `StorageEngine` — verificar si realmente se usa la struct (sí, es pública) → remover `#[allow(dead_code)]`
     - Ejecutar `cargo clippy --all-targets` para detectar si algún campo está no usado
     - Si `_lifecycle_handle` está prefijado con `_`, mantener (indica intencionalmente no usado)
     - Remover campos que clippy marque como no usados (excepto `_lifecycle_handle`)
  2. **hot_store.rs línea 37**: `#[allow(dead_code)]` en campo `config` de `HotStore`
     - Verificar si `config` se usa en el código — si no, remover el `#[allow]` y decidir:
       - Si el campo es necesario para futura funcionalidad: agregar `#[expect(dead_code, reason = "reserved for future use")]`
       - Si no: remover el campo
  3. **wal.rs línea 137**: `#[expect(dead_code)]` en `SegmentWriter.path` — el campo `path` está marcado como no usado
     - Buscar usos de `SegmentWriter.path` en todo el código
     - Si no se usa: remover el campo y el `#[expect]`
     - Actualizar constructor que inicializa `path` (si existe)
  4. **replication.rs línea 86**: `#[expect(dead_code)]` en struct o campo — investigar y remover o implementar
     - Buscar la struct/campo específico
     - Decisión: remover si no se usa, mantener con `#[expect]` si es parte de API pública
  5. Verificar que no quede ningún `#[allow(dead_code)]` o `#[expect(dead_code)]` en la codebase
- **Test strategy**:
  - `cargo clippy --all-targets` no produce `dead_code` warnings sin supresión
  - `cargo build` compila sin errores
  - No hay cambios de comportamiento — solo remoción de código no usado
- **Estimado**: +120 líneas (mayoría son remociones, neto ~40)

---

## Test Strategy Summary

| PR | Test Location | Type | What It Covers |
|----|--------------|------|----------------|
| PR1 | `tesseract-core/src/distance.rs` | Unit | NormalizedVector zero/NaN/valid → Err/Ok |
| PR1 | `tesseract-core/src/topological.rs` | Unit | register_field empty boundaries → Err |
| PR1 | `tesseract-storage/src/page_cache.rs` | Unit | PageCache::new(0) → Err |
| PR1 | `tesseract-storage/tests/shutdown_integration.rs` | Integration | SIGTERM → WAL flush + HotBuffer drain |
| PR1 | `tesseract-common/src/error.rs` | Unit | LockPoisoned, SerializationError, JsonError display |
| PR2 | `tesseract-core/src/embedding.rs` | Unit | Timeout + retry logic (mock HTTP) |
| PR2 | `tesseract-api/src/auth.rs` | Unit | ApiKeyAuth, JwtAuth unit tests |
| PR2 | `tesseract-api/tests/http_integration.rs` | Integration | Auth middleware, rate limiting, health endpoints |
| PR2 | `tesseract-index/tests/` (si aplica gRPC) | Integration | gRPC auth interceptor |
| PR3 | `tesseract-core/src/test_embedding.rs` | Unit | Determinism + normalization |
| PR3 | `tesseract-core/tests/e2e_test_embedding.rs` | E2E | INSERT + FIND SIMILARITY + metadata filters |
| PR4 | `tesseract-index/tests/concurrent.rs` | Stress | Concurrent reads + writes, no deadlock |
| PR4 | Workspace-wide | Lint | `cargo clippy` sin `dead_code` warnings |
| CI | `.github/workflows/ci.yml` | CI | `cargo deny check advisories`, `cargo llvm-cov` |

---

## Dependency Graph

### Within PR1
```
PR1-T1 (NormalizedVector)  ─┐
                             ├──→  PR1-T3 (PageCache)
PR1-T4 (LockPoisoned) ──────┤
   ├── PR1-T5 (EpisodicMemory)   ─→  PR1-T6 (engine.rs)
   └── PR1-T6b (PageCache/ColdStore)

PR1-T7 (Serialization rename) ─→ PR1-T8 (WAL error fix)

PR1-T9 (signal main.rs) ─→ PR1-T10 (shutdown impl) ─→ PR1-T11 (shutdown test)
```

### Within PR2
```
PR2-T1 (timeout config) ─→ PR2-T2 (retry backoff)

PR2-T3 (auth trait) ─→ PR2-T4 (axum middleware)
                  └──→ PR2-T5 (gRPC interceptor)

PR2-T6 (rate limiter) (independiente)
PR2-T7 (query timeout) (independiente)

PR2-T8 (health) ─→ PR2-T9 (metrics)
PR2-T10 (tracing) (independiente)
```

### Within PR3
```
PR3-T1 (TestEmbeddingService) ─→ PR3-T2 (E2E test)
PR3-T3 (CI audit) (independiente)
PR3-T4 (CI coverage) (independiente)
```

### Within PR4
```
PR4-T1 (HNSW parking_lot) (independiente)
PR4-T2 (tokio RwLock) (independiente)
PR4-T1 + PR4-T2 → PR4-T3 (concurrency tests)
PR4-T4 (dead code) (independiente)
```

### Cross-PR
```
PR1 → PR2: A4 (graceful shutdown) necesario para shutdown seguro durante ops
PR1 → PR3: TestEmbeddingService (A9) necesita embedding estable post-A5
PR3, PR4: Independientes entre sí, paralelizables
```
