# Exploration Report: production-readiness

## Summary

Tesseract es un proyecto funcional con arquitectura sólida (HNSW + Merkle Tree + WAL + topológico) pero con múltiples gaps propios de un prototipo hacia producción. Se identificaron **12 áreas** con problemas concretos, totalizando ~25+ issues individuales.

**Gravedad**: 4 críticos (panics en APIs públicas, sin graceful shutdown, sin auth, embedding sin timeouts), 6 altos (lock poisoning, serialización inconsistente, coarse-grained locking, sin observabilidad, sin rate limiting, CI incompleta), 2 medios (tests e2e, código muerto).

**Orden sugerido**: Primero resolver los panics (A1) y lock poisoning (A2), que son correctness. Luego serialización (A4) y graceful shutdown (A10) para integridad. Después embedding (A12), auth (A8), rate limiting (A7), observabilidad (A9). Tests e2e (A3) y CI (A11) pueden ir en paralelo. HNSW locking (A5) y dead code (A6) son los de menor prioridad.

---

## Detailed Findings

### A1. Panics en APIs públicas

#### `NormalizedVector::new()` — panic en lugar de Result

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\distance.rs`
**Líneas**: 28-32

```rust
pub fn new(v: Vec<f64>) -> Self {
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    assert!(norm.is_finite() && norm > 0.0, "NormalizedVector requires a finite, non-zero vector");
    Self(v.into_iter().map(|x| x / norm).collect())
}
```

**Problema**: Cualquier vector vacío, zero, NaN o Inf recibido desde la red (HTTP/gRPC) crashea todo el proceso. No hay `catch_unwind` en los handlers de axum/tonic.

**Solución propuesta**: Convertir a `Result<Self>`, retornando `Error::ServiceError` en lugar de panic. Actualizar `TryFrom<Vec<f64>>` consecuentemente. El `TryFrom` en línea 36-42 actualmente convierte el panic en un error String, pero sigue siendo frágil.

**Dependencias**: Ninguna. Se puede hacer primero.

#### `NumericalBucketTracker::register_field()` — panic en lugar de Result

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\topological.rs`
**Línea**: 385

```rust
pub fn register_field(&mut self, field: &str, boundaries: Vec<f64>) {
    assert!(!boundaries.is_empty(), "bucket boundaries must not be empty");
    ...
}
```

**Problema**: Si alguien pasa boundaries vacío desde configuración (archivo YAML, variables de entorno), crashea.

**Solución propuesta**: Retornar `Result<()>`.

#### `PageCache::new()` — panic en capacity=0

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\page_cache.rs`
**Línea**: 38

```rust
let cap = NonZeroUsize::new(capacity).expect("PageCache capacity must be greater than 0");
```

**Problema**: Panic si la capacidad se configura en 0. Aunque improbable, cualquier entrada de configuración inválida no debería crashear.

**Solución propuesta**: Validar en el constructor de config, o retornar `Result`.

#### `PageCache` — `.expect()` en todos los lock accesses (lock poisoning)

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\page_cache.rs`
**Líneas**: 47, 55, 61, 67, 73

```rust
let mut cache = self.inner.lock().expect("page cache lock poisoned");
```

**Problema**: 5 puntos donde un lock poisoned crashea. Deberían propagar error.

**Solución propuesta**: Retornar `Result<T>` desde todos los métodos públicos. Aunque el poisoning es improbable con `Mutex` de `std`, el `expect` es innecesario.

**Dependencias**: Ninguna.

#### Otros asserts en producción (mencionados pero en tests, OK)

Los demás `assert!` en el codebase están dentro de `#[cfg(test)]` — no son problema.

---

### A2. Lock poisoning ignorado

#### `EpisodicMemory::get_footprint()` — .ok()? silencia poisoning

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\episodic.rs`
**Línea**: 32

```rust
let fp = self.footprints.read().ok()?;
```

**Problema**: Si el lock está poisoned, `ok()` convierte el error en `None`. El poisoning es poco común, PERO: los métodos `write()` de esta misma struct (línea 44) usan `map_err` para propagar como `Error::ServiceError`, lo cual es inconsistente. `get_footprint` devuelve `None` silenciosamente, cuando debería propagar o loggear.

**Solución propuesta**: Usar `map_err` igual que en `update_footprint`, o al menos loggear un warning. Idealmente cambiar la firma a `Result<Option<Vec<f64>>>`.

#### `EpisodicMemory::len()` — .unwrap_or(0) silencia poisoning

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\episodic.rs`
**Línea**: 84

```rust
self.footprints.read().map(|fp| fp.len()).unwrap_or(0)
```

**Problema**: Ídem — el poisoning se traga silenciosamente.

#### `StorageEngine` — 14 `.lock().unwrap()` calls en producción

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\engine.rs`
**Líneas**: 242, 246, 257, 289, 299, 375, 384, 489, 490, 491, 811, 829, 849, 853

Todas siguen el patrón:
```rust
let mut c = centroids_lock.lock().unwrap();
```

**Problema**: Cualquier panic en un hilo que haya tomado estos locks previamente dejará el lock poisoned y la siguiente vez crasheará todo el engine.

**Solución propuesta**: Usar `lock().map_err(...)` o un helper que convierta poisoning en `Error::ServiceError`. Idealmente usar `std::sync::PoisonError` para recovery o al menos propagación limpia.

**Dependencias**: A1 debe hacerse antes, porque los panics de A1 son la causa raíz más probable de poisoning.

---

### A3. Tests e2e / executor

#### NoopEmbeddingService — siempre error

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\embedding.rs`
**Líneas**: 16-25

```rust
pub struct NoopEmbeddingService;
impl EmbeddingService for NoopEmbeddingService {
    async fn embed(&self, _text: &str, _model: &str) -> Result<Vec<f64>> {
        Err(tesseract_common::error::Error::ServiceError(
            "No embedding service configured. ...".into(),
        ))
    }
}
```

**Problema**: Cualquier query e2e con texto siempre falla porque no hay un embedding service de prueba que retorne vectores determinísticos. Los tests existentes en executor.rs (líneas 529-545, 567-582) solo prueban el caso de error.

**Solución propuesta**: Crear `TestEmbeddingService` que implemente `EmbeddingService` y retorne un hash determinístico del texto como vector (e.g., byte sum → f64 normalizado). Así se puede hacer un happy path completo: insert → find semantic → scored results.

**Dependencias**: Ninguna.

#### Sin test de happy path completo (VQL → parse → plan → embed → search → results)

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-vql\src\executor.rs`
**Líneas**: 529-545

```rust
let result = executor.execute("FIND SIMILARITY(emb, 'test') LIMIT 10", None).await;
assert!(result.is_err(), "NoopEmbedding should produce an embed error");
```

El único test e2e del executor espera error. No hay ningún test e2e de insert + search con resultados reales.

---

### A4. Serialización inconsistente en WAL

#### Payload en JSON, error reportado como BincodeError

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\engine.rs`
**Líneas**: 222-225

```rust
payload: serde_json::to_vec(&(id.clone(), &vector, &metadata))
    .map_err(|e| Error::BincodeError(e.to_string()))?,
```

**Problema**: `serde_json::to_vec()` serializa como JSON, pero el error se mapea a `Error::BincodeError`. Es engañoso — debería ser `Error::SerializationError` o similar. Además, en `apply_wal_entry` (línea 509) se deserializa con `serde_json::from_slice`, confirmando que es JSON.

**Solución propuesta**: Agregar variante `Error::JsonError(String)` o cambiar el nombre a `Error::SerializationError`. Corregir el error en `from<bincode::Error>` (línea 61-64 de error.rs) si sigue siendo válido.

**Dependencias**: Ninguna.

#### Checkpoint usa bincode, entry payload usa JSON

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\wal.rs`
**Líneas**: 358, 454

```rust
let data = bincode::serialize(checkpoint)?;  // Checkpoint
let (id, vector, metadata) = serde_json::from_slice::<...>(&entry.payload)?;  // WAL entry
```

**Problema**: No es inconsistencia grave (cada uno tiene su propósito), pero vale la pena documentar que los payloads de WAL son JSON mientras que los metadatos del sistema (checkpoint, segmentos) son bincode. Existe el riesgo de que alguien en el futuro asuma bincode en todas partes.

**Solución propuesta**: Documentar explícitamente la decisión. Opcionalmente estandarizar a bincode también para payloads (más compacto).

---

### A5. Coarse-grained locking en HNSW

#### `RwLock<()>` serializa todas las searches

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-index\src\hnsw.rs`
**Líneas**: 93, 232-233

```rust
lock: RwLock<()>,
...
let _lock = self.lock.read().unwrap();
```

**Problema**: El `RwLock<()>` es un lock de meta-nivel — no protege datos específicos sino que serializa lecturas contra escrituras. Mientras haya una lectura activa (search), ninguna escritura (insert) puede proceder. Para producción con alta concurrencia, esto es un bottleneck significativo: las lecturas de HNSW deberían poder ejecutarse en paralelo con escrituras usando un ARC (Atomic Reference Counting) o versionado de nodos.

**Impacto**: En benchmarks con carga mixta (inserts + searches concurrentes), el throughput de búsqueda cae drásticamente porque los inserts bloquean todos los searches.

**Solución propuesta**: Migrar a un esquema de multi-versioning (MVCC) o copy-on-write para nodos HNSW. Alternativa más simple: usar `parking_lot::RwLock` (más rápido) y reducir ventanas de escritura.

#### `tokio::sync::Mutex` en StorageEngine para el índice

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\engine.rs`
**Línea**: 44

```rust
index: Option<Mutex<AnyIndex>>,
```

**Problema**: `AnyIndex` (que contiene `HnswIndex` con su propio `RwLock`) está envuelto en un `tokio::sync::Mutex`. Esto significa que mientras un insert está en progreso (esperando la inserción en HNSW), ningún search puede ni siquiera *iniciar* — porque `await` en el Mutex de tokio no es reentrante. Esto es doble bloqueo: el Mutex de tokio serializa al nivel del engine, y el RwLock de HNSW serializa al nivel del índice.

**Solución propuesta**: Usar `tokio::sync::RwLock` en lugar de `Mutex` para permitir lecturas concurrentes. O mejor: mover el lock de HNSW fuera y usar `Arc<RwLock<AnyIndex>>`.

**Dependencias**: A5.1 (HNSW locking) debe hacerse en conjunto con este.

---

### A6. Código muerto

#### `#[allow(dead_code)]` en StorageEngine

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\engine.rs`
**Línea**: 36

```rust
#[allow(dead_code)]
pub struct StorageEngine {
    wal: Arc<WriteAheadLog>,
    hot: Arc<HotStore>,
    cold: Arc<ColdStore>,
    skeleton: Arc<VectorSkeleton>,
    cache: Arc<Mutex<PageCache>>,
    config: StorageConfig,
    index: Option<Mutex<AnyIndex>>,
    _lifecycle_handle: Option<tokio::task::JoinHandle<()>>,
    ...
```

**Problema**: `allow(dead_code)` es genérico para toda la struct. No sabemos qué campos están realmente sin usar. `config` se usa en varios métodos. `_lifecycle_handle` el prefijo `_` indica que se ignora intencionalmente. Sin embargo, varios campos como `skeleton` o `cache` podrían tener métodos no llamados.

#### `#[allow(dead_code)]` en HotStore

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\hot_store.rs`
**Línea**: 37

```rust
#[allow(dead_code)]
config: HotStoreConfig,
```

**Problema**: La config se almacena pero nunca se usa para controlar la evicción por `max_records`. La evicción no está implementada.

#### `#[expect(dead_code)]` en SegmentWriter

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\wal.rs`
**Línea**: 137

```rust
#[expect(dead_code)]
path: PathBuf,
```

**Problema**: El path del segmento se almacena pero no se usa para nada. Podría ser útil para debugging o logs.

#### `#[expect(dead_code)]` en ReplicationEngine

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-cluster\src\replication.rs`
**Línea**: 86

```rust
#[expect(dead_code)]
node_id: String,
```

**Problema**: El node_id se pasa al constructor pero nunca se usa.

---

### A7. Sin límites de recursos

#### No hay timeouts en ningún lado

**Búsqueda**: 0 resultados para `timeout`, `rate.limit`, `backpressure`, `semaphore` en todo el storage layer.

**Archivos afectados**:
- `tesseract-storage/src/engine.rs` — `search()` no tiene timeout
- `tesseract-storage/src/hot_store.rs` — `insert()` no tiene límite de tamaño
- `tesseract-api/src/http.rs` — handlers sin timeouts
- `tesseract-core/src/embedding.rs` — OpenAI client sin timeout

**Problema**: Una query ANN sobre un índice con millones de vectores sin timeout puede colgar el servidor. Una inserción masiva puede llenar la RAM sin control.

#### No hay rate limiting

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-api\src\http.rs`
**Líneas**: 92-96

```rust
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/query", post(query_handler))
        .route("/insert", post(insert_handler))
        .with_state(state)
}
```

**Problema**: No hay middleware de rate limiting. Un cliente puede inundar el servidor de queries.

**Solución propuesta**: Agregar tower middleware para rate limiting en axum (e.g., `tower_governor` o `tower::limit`).

#### No hay límite de memoria para HotBuffer

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\types.rs`
**Líneas**: 170-171

```rust
pub hot_buffer_capacity: usize,
pub max_cluster_size: usize,
```

Estos *existen* en la configuración pero no hay enforcement en tiempo de ejecución.

**Dependencias**: A10 (graceful shutdown) debería considerar drenar el buffer.

---

### A8. Sin auth

#### HTTP API — sin middleware de autenticación

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-api\src\http.rs`
**Línea**: 92

```rust
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/query", post(query_handler))
        .route("/insert", post(insert_handler))
        .with_state(state)
}
```

**Problema**: Cero middleware de autenticación. Cualquiera que alcance el puerto puede hacer insert y query.

**Solución propuesta**: Agregar capa de auth vía `axum::middleware`. Soporte para API key (header `X-API-Key`) y/o JWT. Hacerlo configurable (auth optional para dev, required para prod).

#### gRPC — sin interceptors

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-api\src\grpc.rs`
**Líneas**: 114-118

```rust
tonic::transport::Server::builder()
    .add_service(TesseractQueryServer::new(service))
    .serve(socket_addr)
    .await?;
```

**Problema**: Sin interceptor de autenticación.

**Solución propuesta**: Agregar `tonic::service::Interceptor` para validación de tokens.

---

### A9. Sin observabilidad

#### Solo tracing básico — sin métricas, sin OpenTelemetry

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-api\src\main.rs`
**Líneas**: 27-29

```rust
tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
    .init();
```

**Problema**: Solo logs no estructurados a stdout. No hay:
- Métricas de negocio (queries/segundo, latencia percentiles, inserts/segundo)
- Métricas de sistema (memoria, goroutines/threads, tamaño del índice)
- Tracing distribuido para seguir requests a través del pipeline
- Health checks con profundidad (liveness vs readiness)

**Archivos afectados**: todo el proyecto.

**Solución propuesta**: 
1. Agregar `metrics-exporter-prometheus` con counters/histograms en puntos clave (query handler, insert handler, search latency)
2. Health check diferenciado: `/health` (liveness) y `/ready` (readiness — verifica que el WAL esté operativo, índice cargado)
3. Agregar `tracing` spans con `#[instrument]` en el pipeline del executor

#### Sin health check de profundidad

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-api\src\http.rs`
**Líneas**: 103-106

```rust
async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok".to_string(), version: "0.1.0".to_string() })
}
```

No verifica que el storage engine esté funcional, que el WAL responda, que el índice esté cargado. Es un liveness check trivial.

---

### A10. Sin graceful shutdown

#### main.rs no maneja señales

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-api\src\main.rs`
**Líneas**: 84-85

```rust
let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
axum::serve(listener, router).await?;
```

**Problema**: `axum::serve` corre hasta que el listener se cierra, pero no hay manejo de SIGTERM/SIGINT. Cuando el proceso recibe `kill` o Ctrl+C:
1. El TierLifecycle sigue corriendo en background
2. El HotBuffer no se drena al MerkleTree
3. El índice HNSW no se persiste a disco
4. La WAL no se flushea
5. Los requests en tránsito se abortan

**StorageEngine.shutdown() existe pero no se llama**

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\engine.rs`
**Líneas**: 457-474

```rust
pub async fn shutdown(&self) -> Result<()> {
    // Persist index
    if let Some(ref idx_lock) = self.index { ... }
    // Flush WAL
    self.wal.flush().await?;
    Ok(())
}
```

El método existe y funciona (se usa en tests: `integration.rs` línea 46), pero nunca se invoca desde `main.rs`.

**Dependencias**: Debería hacerse después de A5 (locking) porque shutdown necesita tomar locks del índice y del buffer.

---

### A11. CI

#### cargo deny — solo licencias, falta cargo audit

**Archivo**: `C:\Users\USER\Desktop\VQL\.github\workflows\ci.yml`
**Líneas**: 78-87

```yaml
audit:
    name: cargo deny
    ...
    - run: cargo deny check licenses
```

**Problema**: `cargo deny check licenses` solo verifica licencias. No corre `cargo deny check advisories` (vulnerabilidades conocidas) ni `cargo deny check bans` (duplicación de dependencias). Tampoco corre `cargo audit`.

#### Sin cobertura de código

**Archivo**: `C:\Users\USER\Desktop\VQL\openspec\config.yaml`
**Línea**: 31

```yaml
coverage_threshold: 0
```

**Problema**: La cobertura está en 0 — no se mide. CI no tiene paso de cobertura.

#### Sin benchmark regression

No hay `cargo bench` en CI. Los benchmarks en `tesseract-index/benches/` existen pero no se ejecutan automáticamente.

---

### A12. Embedding service (OpenAI)

#### reqwest::Client sin timeout ni retry

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\embedding.rs`
**Líneas**: 43, 60

```rust
client: reqwest::Client::new(),
...
.send()
```

**Problema**: `Client::new()` crea un cliente default sin timeout. Si OpenAI está caído o lento, la request puede colgarse indefinidamente. No hay retry con backoff ni manejo de errores HTTP (429 rate limit, 5xx server error).

#### Sin manejo de rate limiting (HTTP 429)

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-core\src\embedding.rs`
**Líneas**: 51-77

```rust
let resp = self.client.post(&self.endpoint)...
    .send().await.map_err(...)?;
let body: serde_json::Value = resp.json().await...?;
```

**Problema**: Si OpenAI responde con 429 (rate limit), el error se mapea a `IoError` genérico. No hay retry con exponential backoff.

**Solución propuesta**: 
1. Agregar timeout de 30s al cliente: `Client::builder().timeout(Duration::from_secs(30)).build()`
2. Implementar retry con backoff para 429 y 5xx (usar `tokio-retry` o similar)
3. Mejorar errores: distinguir entre error de red, rate limit, y respuesta inválida

**Dependencias**: Ninguna.

---

### Hallazgos adicionales

#### A13. `ColdStore` no tiene límite de particiones

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\cold_store.rs`

Sin límite de archivos abiertos simultáneamente. Cada `read_partition` lee todo el archivo a memoria — peligro con datasets grandes.

#### A14. `VectorSkeleton.find_nearby()` sin límite

**Archivo**: `C:\Users\USER\Desktop\VQL\tesseract-storage\src\skeleton.rs`

No hay límite en la cantidad de particiones que puede despertar.

---

## Recommended Order

| Prioridad | Área | Depende de | Justificación |
|-----------|------|-----------|---------------|
| 1 | **A1. Panics en APIs públicas** | — | Correctness: si crashea, nada más importa |
| 2 | **A2. Lock poisoning** | A1 | Sin A1, la causa raíz de poisoning persiste |
| 3 | **A4. Serialización inconsistente** | — | Impacta integridad del WAL |
| 4 | **A10. Graceful shutdown** | A5 | Sin shutdown limpio, hay pérdida de datos |
| 5 | **A12. Embedding service** | — | Timeouts/retry son críticos si se usa OpenAI |
| 6 | **A8. Auth** | — | Seguridad básica |
| 7 | **A7. Rate limiting / recursos** | — | Protección contra abuso |
| 8 | **A9. Observabilidad** | — | Monitoreo |
| 9 | **A3. Tests e2e** | — | Calidad, puede ir en paralelo |
| 10 | **A11. CI** | — | Calidad, puede ir en paralelo |
| 11 | **A5. HNSW locking** | — | Performance, post-MVP |
| 12 | **A6. Dead code** | — | Higiene, baja prioridad |

## Risk Assessment

### Riesgos Altos

- **Cambiar panics a Results en A1**: Bajo riesgo de regresión si se mantiene la semántica. Los callers actualmente esperan que `NormalizedVector::new` no falle — habrá que actualizar todos los call sites.
- **Graceful shutdown en A10**: Riesgo medio. Si el shutdown tarda mucho (flush WAL, persist index), la señal SIGTERM puede matar el proceso antes de completar. Implementar con timeout en shutdown.
- **HNSW locking en A5**: Riesgo ALTO. Cambiar el modelo de concurrencia de HNSW es complejo y puede introducir race conditions o corrupción de datos. Debe hacerse con tests de concurrencia extensivos.

### Riesgos Medios

- **Auth (A8)**: Agregar auth es relativamente seguro (middleware en axum), pero hay que decidir el modelo: API key estática vs JWT vs OAuth. Afecta la API pública.
- **Embedding (A12)**: Agregar retry puede cambiar el comportamiento observable. Un timeout de 30s puede ser muy agresivo para modelos grandes.
- **Serialización (A4)**: Cambiar el error variant de `BincodeError` a `JsonError` es breaking para código que matchea errores.

### Riesgos Bajos

- **Dead code (A6)**: Remover campos no usados es seguro.
- **Tests e2e (A3)**: Agregar `TestEmbeddingService` no tiene impacto en producción.
- **CI (A11)**: Agregar pasos al CI solo puede mejorar la calidad.

### Ready for Proposal
**Yes** — todas las áreas están suficientemente exploradas para pasar a la fase de propuesta. Se recomienda agrupar los cambios en al menos 3 PRs:
1. **PR Core** (A1, A2, A4, A10) — correctness e integridad
2. **PR Seguridad/Operaciones** (A7, A8, A9, A12) — production hardening
3. **PR Calidad** (A3, A6, A11) — testing y tooling
4. **PR Performance** (A5) — locking, opcional, puede ir después
