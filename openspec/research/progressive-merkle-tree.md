# Progressive Merkle Tree — Design Document

## 1. Problem Statement

Tesseract's current write path is:

```
insert() → WAL → HotStore + HNSW index → (lifecycle) → ColdStore
```

Every insert goes directly into both the HotStore (in-memory buffer) **and** the HNSW index. This provides immediate queryability, but at a cost:

- **HNSW insertion is O(log n) distance computations** with bidirectional edge updates and `shrink_connections` pruning. A batch of 10k new vectors triggers 10k graph traversals from the entry point down to layer 0.
- **No batching in the write path**. Each vector is individually routed through greedy descent, candidate search, neighbor selection, and edge rewiring.
- **Full index rebuild needed for structural reorganization**. If the distribution shifts (concept drift), there is no mechanism short of rebuilding.
- **ColdStore is append-only batch files**. There is no Merkle-style proof system, no incremental compaction, and no freshness guarantee beyond what the HotStore provides.

The fundamental tension:

| Property | Current State | Desired State |
|----------|--------------|---------------|
| Freshness | Immediate (inline HNSW insert) | Immediate via hot buffer + async merge |
| Write throughput | O(log n) per vector | O(1) buffer + O(log c) batch merge |
| Proof of data | None (no authenticated structure) | Merkle root hash per merge |
| Concept drift adaptation | Full rebuild only | Incremental centroid recomputation |
| Disk utilization | Append-only batches | Tree-structured, incremental |

## 2. Related Work

### 2.1 LSM-Trees (LevelDB, RocksDB)

LSM-Trees buffer writes in a memtable, then flush to immutable SSTables. Compaction merges SSTables in the background. Tesseract's HotStore → ColdStore lifecycle already mirrors Level-0 → Level-1, but without the leveled compaction that keeps read amplification bounded.

**Relevance**: The Progressive Merkle Tree (PMT) shares the write-buffer-then-merge pattern. The key difference: LSM merges **key-value pairs**, PMT merges **vector clusters** with centroid recomputation.

**What to borrow**: Size-tiered compaction trigger (merge when buffer ≥ N), leveled structure (hot buffer = L0, Merkle tree = L1+).

**What to avoid**: Write-amplification of leveled compaction. Vector centroids are aggregates, not individual records — merging 10k vectors into a centroid tree requires O(clusters) work, not O(vectors).

### 2.2 HNSW Incremental Insertion

HNSW supports per-vector inserts natively (Malkov & Yashunin 2016, Algorithm 1). Each insert performs greedy descent from the entry point, ef-search at each level, bidirectional edge linking, and connection shrinking.

**Problem with current Tesseract usage**: Every `StorageEngine::insert()` calls `HnswIndex::insert()`, which does full graph traversal. At 1000 inserts/second, the index becomes a bottleneck.

**PMT improvement**: Batch inserts into the hot buffer, then merge into the HNSW index's relevant sub-graph (the cluster nearest the new centroid). This amortizes the graph traversal cost.

### 2.3 FAISS IVF with Incremental Training

FAISS's IVF (Inverted File) index trains k-means centroids, then assigns vectors to the nearest centroid. Incremental training is possible (k-means Mini-Batch), but FAISS does not provide authenticated data structures or proof of freshness.

**Relevance**: Centroid-based routing is the same core idea. PMT differs by building a Merkle tree over the centroids (adding cryptographic proof) and by supporting online centroid updates via weighted averages (no full k-means restart).

### 2.4 SPANN (Microsoft, NeurIPS 2021)

SPANN partitions vectors into clusters with a balanced budget, stores centroids in memory and posting lists on disk. It uses a balancing-aware partitioning to keep clusters roughly equal size.

**Relevance**: PMT's leaves are analogous to SPANN's clusters. SPANN shows that centroid-based routing with disk-based posting lists works at billion-scale.

**Key difference**: SPANN does not provide incremental merging, Merkle proofs, or freshness guarantees — the index is built offline.

## 3. Tesseract's Approach: Centroids over Vectors

### Core Insight

The HNSW index **already** clusters vectors implicitly through its graph structure — nodes in the same region of space tend to share edges. But the graph does not expose clusters explicitly as first-class objects.

PMT makes clusters explicit:

```
Instead of:  HNSW index with N individual vectors
We build:    Merkle tree over C cluster centroids (C ≈ √N for uniform distribution)
             + HNSW sub-graphs per cluster
             + HotBuffer for recent inserts
```

**Why centroids**: A Merkle tree over 1M individual vectors would have height log₂(1M) ≈ 20. A tree over 1000 centroids has height log₂(1000) ≈ 10 — half the depth, and each internal node stores an aggregated centroid (not all vectors in the subtree). The total number of tree nodes is ~2× the number of centroids.

**Scale comparison**:

| Metric | Vectors as leaves | Centroids as leaves |
|--------|-------------------|---------------------|
| Leaf count for 1M vectors | 1,000,000 | ~1,000 |
| Tree nodes (approx) | 2,000,000 | ~2,000 |
| Height | 20 | ~10 |
| Merge cost (10k batch) | O(10k × log n) routing | O(10k × log c) nearest-centroid search |
| Proof size | O(log n) hashes | O(log c) hashes |

### How It Maps to Existing Architecture

Tesseract already has a `CentroidTracker` in `tesseract-core/src/topological.rs` that maintains running sums and counts per metadata category. This is **per-field categorical centroids** for topological bias.

PMT extends the concept to **spatial centroids** — centroids in the embedding space, not metadata space. Each PMT leaf is a centroid of spatially nearby vectors, discovered by the HNSW graph's implicit clustering rather than by metadata keys.

### Data Freshness Model

```
Time

insert(v1) → HotBuffer (queryable immediately)
insert(v2) → HotBuffer
...
insert(v10k) → HotBuffer triggers MERGE
                │
                ├── Find nearest centroid in Merkle tree for each v
                ├── Update centroid via weighted average
                ├── Propagate hash updates to root
                └── Clear HotBuffer

New root hash signed → clients can verify inclusion
```

### Freshness tiers

| Tier | Latency | Query method | Durability |
|------|---------|-------------|------------|
| HotBuffer (in-memory) | < 1ms | Linear scan | WAL-recoverable |
| Merkle tree (recent) | < 5ms | Tree-guided HNSW | Disk (serialized) |
| Merkle tree (merged) | < 10ms | Tree-guided HNSW | Disk (serialized) + ColdStore backup |

## 4. Data Structures

### 4.1 MerkleNode

```rust
/// A node in the Progressive Merkle Tree.
///
/// Leaf nodes represent cluster centroids. Internal nodes aggregate
/// their children's centroids via weighted averaging.
pub struct MerkleNode {
    /// Aggregated centroid of the entire subtree.
    /// For leaves: the cluster centroid.
    /// For internal nodes: weighted average of child centroids.
    pub centroid: Vec<f64>,

    /// SHA-256 hash of the concatenation of children's hashes.
    /// For leaves: hash of (centroid ‖ count ‖ metadata_bounds).
    pub hash: [u8; 32],

    /// Total number of vectors in the subtree.
    pub count: u64,

    /// Children — either in-memory references or disk offsets.
    pub children: NodeChildren,

    /// Optional metadata bounds for topological pruning
    /// (only present when topological tracking is enabled).
    pub metadata_bounds: Option<MetadataBounds>,
}

pub enum NodeChildren {
    /// In-memory tree nodes (during construction / merge).
    InMemory(Vec<Box<MerkleNode>>),
    /// Disk-persisted node references (offset, length in bytes).
    Persisted(Vec<(u64, u64)>),
}

/// Bounding box for metadata within a subtree, used for
/// topological pruning during search.
pub struct MetadataBounds {
    /// Per categorical field: set of values present.
    pub categorical: HashMap<String, HashSet<String>>,
    /// Per numerical field: min/max range.
    pub numerical: HashMap<String, (f64, f64)>,
}
```

### 4.2 HotBuffer

```rust
/// In-memory buffer for recent inserts, sitting between the WAL
/// and the Merkle tree merge path.
pub struct HotBuffer {
    /// Buffered vectors with their metadata.
    vectors: Vec<VectorEntry>,
    /// Maximum buffer size before merge is triggered.
    max_size: usize,
    /// Current buffer size in bytes (approximate).
    size_bytes: AtomicU64,
}

pub struct VectorEntry {
    pub id: VectorId,
    pub vector: Vec<f64>,
    pub metadata: serde_json::Value,
    pub created_at: u64,
}
```

### 4.3 MergePolicy

```rust
/// Determines when a HotBuffer → Merkle tree merge should occur.
pub enum MergePolicy {
    /// Merge when buffer reaches N vectors.
    SizeTiered { max_buffer: usize },
    /// Merge at fixed intervals regardless of buffer size.
    TimeBased { interval_secs: u64 },
    /// Merge when either condition is met.
    Hybrid { max_buffer: usize, interval_secs: u64 },
}
```

### 4.4 MerkleTree (top-level structure)

```rust
pub struct MerkleTree {
    /// Root node of the tree.
    root: Option<Box<MerkleNode>>,
    /// All centroids (leaf nodes), indexed for fast nearest-centroid lookup.
    /// Uses a simple flat array + brute-force for small C, or a separate
    /// HNSW index over centroids for large C.
    centroids: CentroidsIndex,
    /// The hot buffer accepting new inserts.
    buffer: HotBuffer,
    /// Merge policy configuration.
    merge_policy: MergePolicy,
    /// Current root hash (cryptographically signed on merge).
    root_hash: Option<[u8; 32]>,
    /// Configuration.
    config: MerkleTreeConfig,
}

pub struct MerkleTreeConfig {
    /// Maximum number of centroids before splitting leaf.
    pub max_vectors_per_cluster: usize,  // default: 10,000
    /// Minimum number of vectors to form a centroid.
    pub min_vectors_per_cluster: usize,  // default: 100
    /// Distance metric for centroid assignment.
    pub distance_metric: DistanceMetric,
}
```

## 5. Algorithms

### 5.1 Insert

```
Input:  vector v, metadata m, id
Output: acknowledgment (written to WAL + HotBuffer)

1. Append (id, v, m) to WAL (durable or fast, per WriteMode).
2. Update topological bias trackers (centroid, correlation, bucket) — same as current.
3. Push (id, v, m) into HotBuffer.
4. If HotBuffer.len() >= merge_policy.max_buffer:
       spawn_async_merge()
5. Return Ok.

Performance: O(1) — no index traversal.
Freshness: v is immediately queryable via HotBuffer linear scan.
```

**Integration with current `StorageEngine::insert()`**:

The current code does `self.hot.insert(record)` into HotStore AND `idx.insert(id, &vector)` into HNSW. With PMT:

- Remove the direct HNSW insert from the write path.
- HotStore remains as the primary query tier.
- Add `self.merkle_tree.buffer.push(record)`.
- The HNSW index is updated during the async merge, not inline.

### 5.2 Merge (async, background)

```
Input:  HotBuffer snapshot B (frozen for this merge)
        Current Merkle tree T
Output: Updated Merkle tree T' with merged centroids

Phase 1: Assign vectors to centroids
-------------------------------------
For each v in B:
    c_id = argmin distance(v, T.centroids)   // nearest centroid
    if distance < THRESHOLD:
        // Add to existing cluster
        centroid_update(c_id, v, op="add")
        centroid[c_id].count += 1
    else:
        // Create new centroid
        T.centroids.push(Centroid { sum: v, count: 1 })

Phase 2: Recompute affected leaf centroids
------------------------------------------
For each c_id that received vectors:
    old_centroid = T.leaf[c_id].centroid
    new_centroid = weighted_average(old_centroid, v_batch, old_count, new_count)
    T.leaf[c_id].centroid = new_centroid
    T.leaf[c_id].count = old_count + new_count

Phase 3: Propagate hash updates
--------------------------------
For each modified leaf:
    leaf.hash = sha256(leaf.centroid ‖ leaf.count ‖ leaf.metadata_bounds)
    parent = leaf.parent
    while parent is not None:
        parent.centroid = weighted_average(children.centroids)
        parent.count = sum(children.count)
        parent.hash = sha256(concat(children.hashes))
        parent = parent.parent

Phase 4: Split centroids if over capacity
------------------------------------------
For each centroid c where c.count > max_vectors_per_cluster:
    // Run mini-k-means(2) to split into two centroids
    (c1, c2) = split_centroid(c)
    Replace c with c1, c2 in parent
    Propagate hash updates up to root

Phase 5: Update centroid index
-------------------------------
Rebuild or update the nearest-centroid search structure
(if using HNSW over centroids, insert new centroids).

Phase 6: Persist and sign
--------------------------
root_hash = T.root.hash
sign(root_hash, private_key)  // optional
T.root_hash = Some(root_hash)
serialize(T) to disk
```

**Weighted average formula**:

```
centroid_new = (centroid_old * count_old + sum_of_new_vectors) / (count_old + count_new)
```

This is the same online update used by `CentroidTracker::update()` in the existing topological code. The PMT reuses the same mathematical approach.

### 5.3 Search

```
Input:  query vector q, k, optional filter
Output: top-k (VectorId, distance)

Phase 1: HotBuffer scan (parallel)
-----------------------------------
results_H = linear_scan(HotBuffer, q, k, filter)
// O(|HotBuffer|) distance computations. Configurable max.

Phase 2: Merkle tree traversal (parallel)
-----------------------------------------
results_T = merkle_search(root=T.root, q, k, filter)

function merkle_search(node, q, k, filter):
    if node is leaf:
        return hnsw_search(cluster[node], q, k, filter)
    
    // Compute distance from q to each child's centroid
    distances = [(distance(q, child.centroid), child) for child in node.children]
    
    // Prune: skip children where min_possible_distance > best_known + margin
    // min_possible_distance = |distance(q, child.centroid) - centroid_radius|
    
    // Also apply topological pruning:
    // Skip children whose metadata_bounds don't overlap with filter
    if filter is not None:
        distances = [(d, c) for (d, c) in distances 
                     if metadata_overlaps(c.metadata_bounds, filter)]
    
    sort(distances by d ascending)
    
    results = []
    for (d, child) in distances:
        if results.len() >= k AND d > results.last().distance + margin:
            break  // prune remaining children
        child_results = merkle_search(child, q, k, filter)
        results.merge(child_results)
    
    return results.top(k)

Phase 3: Merge and rank
------------------------
combined = merge_sorted(results_H, results_T)
deduplicate(combined)  // by VectorId
return combined.top(k)
```

**HotBuffer search access patterns**:

The HotBuffer linear scan is O(buffer_size) and runs in parallel with the tree search. For typical buffer sizes (1k–100k vectors) this is fast. The buffer can also maintain an **optional** lightweight index (e.g., a flat LSH or a tiny HNSW) for faster lookup at the cost of memory.

**Topological pruning integration**:

Each MerkleNode's `metadata_bounds` stores the set of metadata values present in its subtree. When a query includes a filter (e.g., `category = "science"`), the tree traversal skips subtrees whose `metadata_bounds.categorical` does not include "science". This is strictly more powerful than post-filtering because the pruning happens during tree traversal, not after.

### 5.4 Proof of Freshness (Merkle property)

Each merge produces a signed root hash. The proof system enables:

```
Client request: "Was vector v included at root hash H?"

Prover response:
    leaf_hash = sha256(v ‖ centroid_of_cluster ‖ count)
    sibling_hashes = [h1, h2, ..., h_log_c]
    proof = (leaf_hash, sibling_hashes, path_indices)

Verifier:
    current_hash = leaf_hash
    for (sibling_hash, direction) in zip(sibling_hashes, path_indices):
        if direction == LEFT:
            current_hash = sha256(sibling_hash ‖ current_hash)
        else:
            current_hash = sha256(current_hash ‖ sibling_hash)
    assert current_hash == H
```

This enables:
- **Auditability**: Prove the state of the index at any point in time.
- **Replication verification**: Followers can verify they have the same state as the leader by comparing root hashes.
- **Time-travel queries**: Query the index as it was at a specific root hash (requires archived tree snapshots).

## 6. Integration with StorageEngine

### 6.1 Current Architecture

```
                ┌─────────────────────────────────┐
                │         StorageEngine            │
                │                                  │
  insert() ────►│  WAL ──► HotStore ──► Lifecycle ──► ColdStore  │
                │            │                     │
                │            ▼                     │
                │        HNSW Index                │
                │                                  │
                └─────────────────────────────────┘
```

### 6.2 Proposed Architecture

```
                ┌─────────────────────────────────────────┐
                │             StorageEngine                │
                │                                          │
  insert() ────►│  WAL ──► HotStore ──► Lifecycle ──► ColdStore  │
                │            │                             │
                │            ▼                             │
                │     HotBuffer (new)                      │
                │            │                             │
                │            ▼ (async merge)               │
                │     MerkleTree ──► HNSW sub-graphs       │
                │            │                             │
                │            ▼                             │
                │     Tree disk storage                    │
                │                                          │
                │     Topological trackers (unchanged)      │
                └─────────────────────────────────────────┘
```

### 6.3 Impact on existing code

**`HotStore` (`tesseract-storage/src/hot_store.rs`)**:
- Stays as the primary query tier for recently merged data.
- The HotBuffer is a **separate** structure — HotStore holds demoted data from lifecycle, HotBuffer holds unmerged inserts.

**`ColdStore` (`tesseract-storage/src/cold_store.rs`)**:
- Remains for archival storage.
- May be deprecated in favor of Merkle tree disk storage in a future phase.

**`StorageEngine` (`tesseract-storage/src/engine.rs`)**:
- New field: `merkle_tree: Option<MerkleTree>`.
- `insert()` changes: remove inline HNSW insert, add HotBuffer push.
- `search()` changes: add hybrid search (HotBuffer scan + Merkle tree traversal).
- New method: `trigger_merge()` — explicitly request a background merge.
- New config: `MerkleTreeConfig` in `StorageConfig`.

**`HnswIndex` (`tesseract-index/src/hnsw.rs`)**:
- No structural changes needed.
- PMT uses HNSW per cluster, which means multiple HNSW instances instead of one giant one.
- Each HNSW instance is smaller (O(max_vectors_per_cluster) ≈ 10k), so inserts/edge management are cheaper.

**`TopologicalIndex` trait (`tesseract-index/src/topological_index.rs`)**:
- Unchanged. The PMT uses the existing trait for per-cluster ANN search.

### 6.4 Merge scheduler

```rust
/// Background task that monitors HotBuffer and triggers merges.
pub struct MergeScheduler {
    merkle_tree: Arc<Mutex<MerkleTree>>,
    wal: Arc<WriteAheadLog>,
    config: MergePolicy,
}

impl MergeScheduler {
    pub async fn run(&self) {
        let mut interval = time::interval(self.config.check_interval());
        loop {
            interval.tick().await;
            if self.merkle_tree.lock().await.buffer.len() >= self.config.max_buffer {
                self.merkle_tree.lock().await.merge().await?;
            }
        }
    }
}
```

## 7. Query Planner Integration

### 7.1 New PlanNode

```rust
/// Hybrid plan node that searches both the HotBuffer and the Merkle tree.
pub struct MergeScan {
    /// Linear scan through HotBuffer (handles recent inserts).
    pub hot_scan: HotScan,
    /// Tree-guided HNSW search (handles merged data).
    pub tree_search: AnnScan,
    /// Limit for final results.
    pub limit: usize,
    /// Optional metadata filter for topological pruning.
    pub filter: Option<LogicalFilter>,
}
```

### 7.2 Execution Plan

```
Query: SELECT id, vector FROM vectors ORDER BY vector <-> query LIMIT 10
       WHERE category = 'science'

Plan:
  TopN(limit=10)
    Merge
      HotScan(buffer, query, k=10, filter=category='science')
      AnnScan(merkle_tree, query, k=10, filter=category='science')
```

The executor runs both branches in parallel:

```rust
async fn execute_merge_scan(node: MergeScan) -> Result<Vec<(VectorId, f32)>> {
    let (hot_results, tree_results) = tokio::join!(
        execute_hot_scan(node.hot_scan),
        execute_ann_scan(node.tree_search),
    );
    
    let mut combined = merge_sorted(hot_results?, tree_results?);
    combined.dedup_by_key(|(id, _)| id.clone());
    combined.truncate(node.limit);
    Ok(combined)
}
```

### 7.3 Cost-based optimization

The query planner should estimate:

- **HotBuffer size**: current buffer length affects whether linear scan is acceptable.
- **Tree height**: affects tree traversal cost (O(log C) node visits).
- **Filter selectivity**: affects topological pruning effectiveness (more selective = more pruning).
- **ef_search tuning**: the tree search may use lower ef because topological pruning narrows the candidate pool.

For very small buffers (< 1000 vectors), the planner could skip the Merkle tree entirely and do brute-force over (HotBuffer + last merged snapshot). For large buffers, both paths run concurrently.

## 8. Open Questions

### 8.1 Should the Merkle tree replace ColdStore entirely, or complement it?

**Arguments for replacement**:
- The tree already stores all data (vectors at leaves, centroids at internal nodes).
- ColdStore's batch-file structure becomes redundant.
- Single storage format reduces code complexity.

**Arguments for complement**:
- ColdStore provides a simpler, proven archival path.
- Tree disk storage is more complex (random access vs sequential batches).
- ColdStore can serve as a write-ahead backup for the tree.

**Initial recommendation**: Complement. Keep ColdStore for archival. The Merkle tree is the primary query structure; ColdStore is the recovery/backup layer. The tree can be rebuilt from ColdStore + WAL if corrupted.

### 8.2 How to handle deletes/updates in a tree designed for append-only?

**Approach A: Tombstones in leaf vectors**.
- Each leaf tracks a tombstone bitmap (like HNSW's `deleted: Vec<bool>`).
- Deletes mark the id as tombstoned; the count decrements lazily.
- Merge reaps tombstones when a centroid is recomputed.

**Approach B: Update-in-place**.
- Re-insert a vector with the same id replaces the vector in the HotBuffer.
- If the vector is already merged, the update is buffered and applied during the next merge.

**Approach C: Compaction merge**.
- Periodic compaction merges that rebuild affected centroids from scratch, excluding tombstones.
- Similar to LSM-Tree compaction.

**Initial recommendation**: Approach A (tombstones) + Approach B (hot-buffer overwrite). Tombstones are already used in the HNSW index, so the pattern is familiar. Compaction merges (C) can come later if tombstone accumulation degrades performance.

### 8.3 Merge frequency vs query latency tradeoff?

Factors:
- **Frequent merges** (every 1k vectors): HotBuffer stays small, search is fast, but merge overhead increases.
- **Infrequent merges** (every 100k vectors): HotBuffer grows, linear scan becomes expensive.

The sweet spot depends on workload:
- Write-heavy: accept larger buffer, batch aggressively.
- Read-heavy: keep buffer small, merge frequently.

**Recommendation**: Make HotBuffer size configurable per-collection. Default to 10k vectors. Monitor the 99th percentile query latency and adjust automatically.

### 8.4 Memory-mapped vs serialized tree nodes?

**Memory-mapped**:
- Zero-copy reads for internal nodes during tree traversal.
- Pages faulted in on demand.
- OS manages eviction.
- Risk: page faults during query execution add latency spikes.

**Serialized (bincode / custom format)**:
- Full control over deserialization.
- Can cache frequently accessed nodes.
- Higher per-read overhead (allocation + copy).

**Recommendation**: Serialized with an LRU node cache for hot path (root + top 2–3 levels). Memory-mapped storage can be evaluated as a future optimization. The root and near-root nodes are accessed on every query and fit easily in cache.

### 8.5 Centroid initialization strategy

When the first vectors arrive and no centroids exist:
- Start with all vectors in the HotBuffer.
- First merge runs k-means or random sampling to produce initial centroids.
- Configurable: `initial_centroids` count (default: √expected_dataset_size).

### 8.6 How to handle dimensionality mismatch for metadata bounds

If metadata bounds include categorical fields with high cardinality (e.g., UUIDs), storing a `HashSet<String>` per node is expensive. Options:
- Cap the set size (e.g., store only the most frequent values).
- Use a Bloom filter per node for categorical fields.
- Skip categorical bounds for high-cardinality fields.

**Recommendation**: Bloom filters for high-cardinality fields, with a configurable false-positive rate. Exact sets for low-cardinality fields.

## 9. Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Merge latency blocks writes | Write throughput dips during merge | Snapshot the buffer atomically; merge runs on snapshot, writes continue to new buffer |
| Centroid drift reduces recall | Stale centroids route queries poorly | Track centroid staleness (last-update timestamp); trigger split/recluster when drift exceeds threshold |
| HotBuffer linear scan too slow | Read latency spikes for large buffers | Add optional lightweight index on buffer; make buffer size adaptive |
| Tree serialization too slow | Recovery time increases | Prioritize root + top levels; lazy-load leaf centroids |
| Memory overhead of metadata bounds | Tree uses more memory than expected | Make bounds configurable per field; skip for unindexed metadata fields |

## 10. Implementation Phases

### Phase 1: Core Data Structures
- `MerkleNode`, `MerkleTree`, `HotBuffer`, `MergePolicy`
- In-memory tree with serialization
- Basic insert → buffer → merge cycle
- Tests with synthetic data

### Phase 2: Integration with StorageEngine
- Add `MerkleTree` field to `StorageEngine`
- Modify `insert()` to use HotBuffer
- Modify `search()` to hybrid search
- `MergeScheduler` background task

### Phase 3: Query Planner Integration
- `MergeScan` plan node
- Parallel execution of hot scan + tree search
- Topological pruning during tree traversal

### Phase 4: Proof System
- Root hash signing
- Inclusion proof generation and verification
- Replication verification via root hash comparison

### Phase 5: Optimizations
- LRU node cache
- Adaptive merge frequency
- Centroid health monitoring (drift detection)
- Bloom filters for metadata bounds
- Memory-mapped disk storage
- Split/merge heuristics for cluster balancing
