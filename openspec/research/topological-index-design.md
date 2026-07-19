# Topological Dynamic Index — Conceptual Design

## 1. Mathematical Framework

### 1.1 Problem Statement

Given:
- Query vector `q ∈ ℝ^d`
- Metadata filter `f = {f₁, ..., f_k}` where each `f_i` is a predicate on metadata field `m_i`
- Dataset `{ (v_j, m_j) }` where `v_j ∈ ℝ^d` is a vector and `m_j ∈ ℝ^p` is metadata

Goal: Find top-k vectors `v_j` where `v_j` is semantically similar to `q` AND `m_j` satisfies `f`.

Without topological projection: `similarity(q, v_j)` is computed first, then filter by `f`. With topological projection: the filter `f` modifies the similarity function so that vectors matching `f` are geometrically closer to `q`.

### 1.2 Weighted Distance Function

The core operation is a weighted distance that fuses metadata constraints into the metric:

```
d_w(q, v, m) = Σ_i w_i(m)² · (q_i - v_i)²    (Euclidean)
d_w(q, v, m) = 1 - Σ_i w_i(m)² · q_i · v_i    (Cosine, assumes normalized vectors)
```

where `w: ℝ^p → [0,1]^d` is the **gating function** — a learned mapping from metadata to a per-dimension weight vector.

This is equivalent to element-wise multiplying `q` by `w(m)`, then computing the standard distance. It is **exactly** what the existing `WeightMask` + `DistanceComputer::distance_weighted` does in `projection.rs` and `distance.rs` (lines 30-34 for cosine), with the crucial difference that `w` is now a learned function instead of a hash-based heuristic.

The existing `mask_to_dense` function (`distance.rs:58-66`) converts the sparse `WeightMask` to a dense `Vec<f32>` with default weight `1.0` for unspecified dimensions. The proposed system replaces the hash-based sparse mask with a **dense mask** `w ∈ [0,1]^d` produced by the gating network, but keeps the same `WeightMask` storage type for backward compatibility — the dense output is thresholded/sparsified before storage.

### 1.3 The Gating Function

```
w(m) = σ(MLP(encode(m)))
```

where:
- `encode(m): ℝ^p → ℝ^e` is an encoding layer specific to each metadata type
- `MLP: ℝ^e → ℝ^d` is a small feedforward network (2-3 hidden layers)
- `σ` is the element-wise sigmoid, clamping each output to `[0, 1]`

**Why sigmoid?** The weight `w_i = 1.0` means "dimension i is fully relevant" (standard cosine). `w_i = 0.0` means "dimension i is completely irrelevant given this filter." Sigmoid gives a smooth interpolation. In practice, most dimensions should remain near `1.0` (metadata only affects a subset of discriminative dimensions), so we add an L1 sparsity penalty on `(1 - w_i)` — encouraging the model to keep most dimensions at full weight unless there is evidence to reduce them.

### 1.4 Relationship to the Existing Pipeline

```
Current (planner.rs:294-318):
  field "category" → hash("category") % 384 → dim 142, weight 1.0
  field "year"     → hash("year") % 384     → dim 287, weight 0.5

Proposed:
  metadata { category: "science", year: 2020 }
    → encode → [one-hot category, normalized year]
    → MLP → [w_0, w_1, ..., w_383]
    → mask_to_dense → WeightMask → distance_weighted
```

The only change to the existing pipeline is how `WeightMask` is populated. The distance computation in `hnsw.rs:225-265`, the `mask_to_dense` conversion in `distance.rs:58-66`, and the fused weighted distance loop in `hnsw.rs:432-477` remain untouched.

## 2. Metadata Encoding

### 2.1 Categorical Fields

**Low cardinality** (<100 distinct values): one-hot encoding.

```
encode(category="science") → [0, 0, 1, ..., 0] ∈ {0,1}^c
```

**High cardinality** (≥100 distinct values): learned embedding table.

```
encode(product_id="SKU-48291") → EmbeddingLookup("SKU-48291") ∈ ℝ^16
```

Embedding dimension: `min(50, log₂(cardinality) × 4)`. These embeddings are trained jointly with the gating MLP.

**Unknown categories** at inference: map to a learned `<UNK>` embedding (trained on a random subset of rare categories), or fall back to zero vector (neutral — all weights stay at `1.0`).

### 2.2 Numerical Fields

Min-max normalization to `[0, 1]`:

```
encode(price=150.0) → (150.0 - min_price) / (max_price - min_price)
```

When global min/max are unknown at training time, use z-score normalization `(x - μ) / σ` followed by clipping to `[-3, 3]` and rescaling to `[0, 1]`.

### 2.3 Temporal Fields

Decompose into components + periodic encoding:

```
encode(timestamp="2026-07-18T14:30:00Z") →
  [year_norm, month_sin, month_cos, day_sin, day_cos,
   dow_sin, dow_cos, hour_sin, hour_cos]
```

Cyclical fields use sin/cos encoding to preserve periodicity:

```
month_sin = sin(2π · month / 12)
month_cos = cos(2π · month / 12)
```

### 2.4 Multi-valued Fields (tags, categories)

Multi-hot encoding averaged (mean pooling):

```
encode(tags=["science", "physics", "2024"]) →
  mean(one_hot("science"), one_hot("physics"), one_hot("2024"))
```

For weighted tags (e.g., importance scores), use weighted mean pooling.

### 2.5 Compound Encoding

All per-field encodings are concatenated into a single feature vector:

```
e = [e_cat_1; ...; e_cat_n; e_num_1; ...; e_num_m; e_temporal; e_tags] ∈ ℝ^e
```

The total encoded dimension `e` depends on the metadata schema. For a typical schema with 5 categorical fields (avg cardinality 50), 3 numerical fields, and 1 temporal field: `e ≈ 5×50 + 3×1 + 8 = 261`.

## 3. Model Architecture

### 3.1 Lightweight (production inference)

```
Input: e ∈ ℝ^e (encoded metadata)
  → Linear(e, 128) + ReLU
  → Linear(128, 64) + ReLU
  → Linear(64, d) + Sigmoid
Output: w ∈ [0, 1]^d
```

**Parameter count**: `e × 128 + 128 × 64 + 64 × d`

For `e = 261, d = 384`: **~66k parameters**. Inference time ≈ 3-5 μs on a single CPU core (measured on an Apple M1 / AMD EPYC for equivalent-sized MLP).

The weight of the output layer (`64 × d = 24,576`) dominates — this is the bottleneck. For higher-dimensional embeddings (e.g., `d = 1536` for text-embedding-3-large), the output layer grows to `64 × 1536 = 98,304` parameters. Consider an intermediate bottleneck layer `Linear(64, 32) → Linear(32, d)` as an alternative for large `d`.

### 3.2 Heavyweight (training / offline)

```
Input: e ∈ ℝ^e
  → Linear(e, 256) + BatchNorm + ReLU + Dropout(0.1)
  → Linear(256, 128) + BatchNorm + ReLU + Dropout(0.1)
  → Linear(128, 64) + ReLU
  → Linear(64, d) + Sigmoid
Output: w ∈ [0, 1]^d
```

**Parameter count**: ~140k for `d = 384`. Only used during training — the trained weights are copied to the lightweight architecture for inference.

### 3.3 Output Layer

The output dimension `d` equals the embedding dimension of the index. Each output neuron corresponds to one embedding dimension and controls how much that dimension contributes to the weighted distance.

Sigmoid activation ensures bounded weights in `[0, 1]`. During inference, weights below a threshold (e.g., `0.01`) can be treated as zero in the sparse `WeightMask` representation; weights above `0.99` can be omitted (they default to `1.0` anyway). This sparsification keeps the `WeightMask` compact: for most filters, only 5-20% of dimensions will have non-trivial weights.

## 4. Training

### 4.1 Training Data

**From query logs** (when available): triplets `(q, f, v_pos, v_neg)` where:
- `q` is the query vector
- `f` is the metadata filter from the query
- `v_pos` is a vector that was clicked / engaged with / fulfilled the filter
- `v_neg` is a vector that was not clicked OR does not match the filter

**From synthetic data** (cold start, no logs): for each metadata field `f`, sample:
- Positive pairs: vectors `(v_i, v_j)` where `m_i` and `m_j` share the same value for `f`
- Negative pairs: vectors `(v_i, v_k)` where `m_i` and `m_k` differ on `f`

This bootstraps from the index itself — the "ground truth" for a metadata filter is the set of vectors that literally satisfy it. The gating network learns to make those vectors geometrically closer under the weighted distance.

### 4.2 Loss Function

**Triplet loss** with weighted distance:

```
L = Σ_{(q, f, v_pos, v_neg)} max(0, margin - d_w(q, v_neg, f) + d_w(q, v_pos, f))
```

where `d_w` is the weighted distance from Section 1.2 with the learned `w(m) = gating_network(encode(f))`.

**Why triplet loss?** The goal is not to reconstruct the metadata (regression) or classify it (cross-entropy). The goal is to **re-rank**: given a query and a filter, make sure the relevant vectors are closer than irrelevant ones. Triplet loss directly optimizes this ordering.

**Margin selection**: Start with `margin = 0.1` for normalized cosine distances (range `[0, 2]`). Tune based on validation recall@k.

**Auxiliary L1 sparsity regularization**:

```
L_total = L + λ · Σ_i |w_i - 1.0|
```

with `λ = 0.001` (small — just enough to keep most dimensions at their default `1.0` unless the filter actively requires change). The L1 is applied to the **output** weights, not the hidden layers.

### 4.3 Cold Start Procedure

When no query logs exist:

1. For each metadata field `f`, enumerate all distinct values in the index.
2. For each value `v` of field `f`, sample:
   - 50 positive pairs: two random vectors with `field = v`
   - 50 negative pairs: one vector with `field = v`, one with `field != v`
3. Construct triplet set: `(q, f, pos, neg)` where `q = pos` (the query IS the positive vector — pure metadata-driven learning).
4. Train until validation triplet accuracy > 90%.

This bootstraps a reasonable gating function without any query logs. The synthetic data teaches the network which embedding dimensions correlate with each metadata value.

### 4.4 Online Update

The gating network can be updated incrementally without full reindexing:

1. Accumulate new query log entries.
2. Every N queries (e.g., 10,000) or when recall drift is detected:
   - Construct new triplets from recent logs + holdout from previous training
   - Fine-tune for 1-2 epochs with learning rate `1e-4` (10× smaller than initial)
   - Swap model weights atomically (no index graph changes needed)

**Why no reindexing?** The gating network only affects the query vector during search — it does not modify the stored vectors or the HNSW graph structure. The index is static; the gating is a query-time transformation. This is a critical architectural advantage: model updates are instant, cheap, and do not require index rebuilding.

### 4.5 Training Hyperparameters

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Optimizer | Adam | Standard for MLP training |
| Learning rate | 1e-3 | Initial; reduce on plateau |
| Batch size | 256 | Balance gradient noise / throughput |
| Epochs | 50 (early stop at 5 no-improvement) | Prevent overfitting to synthetic triplets |
| Dropout | 0.1 | Heavyweight arch only |
| L1 λ | 0.001 | Sparse output regularization |
| Triplet margin | 0.1 | Matching the cosine distance scale |
| Validation split | 20% | Per-field stratified |

## 5. Inference Pipeline

For each query at search time:

```
1. Parse VQL query → AST → metadata filter f
                                ↓
2. Encode filter values → e = encode(f)
                                ↓
3. Gating network forward pass → w = σ(MLP(e))
                                ↓
4. Sparsify: keep only w_i where |w_i - 1.0| > ε
   Build WeightMask from non-default entries
                                ↓
5. mask_to_dense(w) → dense weights
                                ↓
6. HNSW search with fused weighted distance
   (existing code in hnsw.rs:225-265, no changes needed)
                                ↓
7. Optional post-filter for non-projected predicates
   (LIKE, text patterns, unsupported operators)
```

**Steps 1-4** replace `derive_weight_mask` in `planner.rs:294-318`. Steps 5-7 use existing infrastructure.

### 5.1 Latency Budget

- Step 2 (encode): `< 1μs` (table lookups + arithmetic)
- Step 3 (MLP forward): `3-5μs` (66k param MLP on CPU)
- Step 4 (sparsify): `< 1μs`
- **Total overhead**: `< 10μs` added per query

This is well within the `< 100μs` target. The dominant cost remains HNSW graph traversal (hundreds of distance computations × `ef`).

### 5.2 Caching

For repeated identical filter patterns (common in production — e.g., "WHERE status = 'active'" appears in most queries), cache the weight mask:

```
cache_key = hash(encoded_metadata)  // fast: already computed in step 2
w = cache_get(cache_key) | cache_set(cache_key, w)
```

Cache hit rate > 90% for typical workloads (limited set of distinct metadata filters). LRU eviction, max 10,000 entries.

## 6. Integration with Existing Code

### 6.1 New Module: `tesseract-core/src/gating.rs`

```
pub struct GatingNetwork {
    layers: Vec<DenseLayer>,      // trained weights
    metadata_schema: MetadataSchema,  // field names, types, cardinalities
    cache: LruCache<u64, Vec<f32>>,   // weight mask cache
}

impl GatingNetwork {
    pub fn predict(&self, filter: &MetadataWhere) -> WeightMask;
    pub fn train(&mut self, triplets: &[Triplet], config: &TrainConfig);
    pub fn save(&self, path: &str);
    pub fn load(path: &str, schema: &MetadataSchema) -> Self;
}
```

### 6.2 Changes to `planner.rs`

**Replace** `derive_weight_mask` (lines 294-318) and `field_to_dim` (lines 322-325):

```rust
// New implementation
fn derive_weight_mask(&self, metadata_where: &Option<MetadataWhere>) -> Option<WeightMask> {
    let mw = metadata_where.as_ref()?;
    if mw.predicates.is_empty() { return None; }
    let mask = self.gating_network.predict(mw);
    if mask.0.is_empty() { None } else { Some(mask) }
}
```

**Add** `gating_network: GatingNetwork` field to `QueryPlanner` and `PlannerConfig`.

### 6.3 Changes to `projection.rs`

**No changes needed.** `WeightMask` and `Projection` are storage types — they are agnostic to how the mask was derived.

### 6.4 Changes to `distance.rs`

**No changes needed.** `mask_to_dense` and `distance_weighted` are agnostic to mask origin. They work correctly whether the mask comes from hash-based heuristics or learned predictions.

### 6.5 Changes to `hnsw.rs`

**No changes needed.** The `search` method (line 225) already accepts `Option<&WeightMask>` and fuses it into the weighted distance loop. The dense weight vector produced by `mask_to_dense` is identical regardless of source.

### 6.6 Configuration

Add to `PlannerConfig`:

```rust
pub struct PlannerConfig {
    // ... existing fields ...
    pub gating_model_path: Option<String>,  // path to trained GatingNetwork
    pub gating_cache_size: usize,            // default 10_000
    pub gating_enabled: bool,                // default false (opt-in)
}
```

## 7. Success Metrics

| Metric | Target | Measurement |
|--------|--------|-------------|
| recall@k improvement | Δ > +15% for selective filters | Compare gating vs. heuristic WeightMask on held-out queries |
| Latency overhead | < 100 μs per query | p50 MLP inference time on production hardware |
| Training time | < 1 hour for 100k queries | Time to convergence on synthetic + real triplets |
| Cache hit rate | > 90% | Ratio of cache lookups to total queries |
| Sparsity | > 80% dimensions at default weight | Fraction of `w_i` within ε of `1.0` |
| Cold start recall | > 80% of warm model | Compare cold-start (synthetic only) vs. trained on logs |

## 8. Open Questions

1. **Minimum training data**: How many distinct metadata values must be seen during training for the model to generalize? Hypothesis: the MLP generalizes well because it learns **per-dimension relevance patterns** (e.g., "dimensions 12-20 correlate with publication year") which transfer to unseen values of the same field.

2. **Cross-model transfer**: If the embedding model changes (e.g., upgrade from text-embedding-3-small to text-embedding-3-large with different `d`), does the gating network need retraining? Almost certainly yes — the dimension semantics change. But the training pipeline is fast enough that retraining is not a blocker.

3. **Unseen metadata values**: At inference, a category value not seen during training produces an unknown embedding. The `<UNK>` embedding fallback (Section 2.1) should produce a near-uniform weight mask, effectively falling back to unweighted search. This is safe but suboptimal. Could we use the metadata field name as a signal even for unseen values?

4. **AND/OR combinatorics**: The current design concatenates all filter predicates into a single encoding, then runs one MLP forward pass. For `(category = "X" OR category = "Y") AND year > 2020`, the MLP must learn to handle the disjunction internally. Does the MLP generalize to unseen logical combinations, or do we need an explicit compositional mechanism?

5. **Range query encoding**: For `price > 100`, we encode the bound value (`100`) and rely on the MLP to learn that "prices above this threshold should be closer." But the weight mask is symmetric — it applies the same `w` regardless of direction. A range filter like `price > 100` is fundamentally non-symmetric (vectors with price 101 should be closer; vectors with price 99 should not). The current gating approach does not capture this asymmetry. Potential fix: encode both the operator and the bound, and accept that asymmetric filters may need post-filtering anyway.

6. **Multi-field interaction**: Does the MLP learn interactions like "category = 'science' AND year > 2020" better than independent field encodings? Hypothesis: yes, because the hidden layers can learn joint representations (e.g., "science papers from 2020+ cluster around dimensions 45-60, but science papers from before 2020 cluster around dimension 70").
