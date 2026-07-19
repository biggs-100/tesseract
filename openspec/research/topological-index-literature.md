# Estado del Arte: Índice Topológico Dinámico

## 1. Definición del Problema

Given a collection of vectors `V = {v₁, ..., vₙ} ⊂ ℝᵈ` and associated metadata `M = {m₁, ..., mₙ}` where each `mᵢ` is a tuple of structured attributes (category, year, price, status, etc.), we want to answer queries of the form:

```
FIND SIMILAR TO q (BY cosine) WHERE metadata_filter
```

The fundamental challenge: **the set of vectors satisfying `metadata_filter` may be small relative to `V`, but standard ANN algorithms (HNSW, IVF, DiskANN) index the entire collection without metadata awareness.** Three failure modes exist:

1. **Recall failure**: Top-k ANN results may contain zero vectors matching the filter. Even with high ef_search, the required exploration radius grows with filter selectivity.
2. **Latency failure**: Post-filtering with high ef_search compensates for recall but increases distance computations proportionally.
3. **Index fragmentation**: Metadata-aware partitioning creates indices per metadata partition, increasing memory and management overhead.

The Tesseract vision approaches this from a fundamentally different angle: instead of treating metadata as an orthogonal filter, **learn a transformation that embeds metadata constraints into the vector space itself**, so that the ANN search naturally prefers vectors matching the filter through geometric proximity rather than post-hoc filtering.

## 2. Enfoques Existentes

### 2.1 Post-filtering (ANN + WHERE)

Run ANN, then discard results not matching the filter. If fewer than `k` survive, increase ef or re-run.

**Used by**: pgvector (default), Chroma, early Pinecone.

**Strengths**: Simple, no index changes, any filter expression supported.

**Weaknesses**: If filter selectivity is high (e.g., `year = 2020 AND category = 'rare'` with 0.1% match rate), ANN must return ~`k × 1000` candidates to find `k` valid results. Theoretical guarantee: expected candidates needed = `k / selectivity`.

**Mathematical bound**: For selectivity `s ∈ (0, 1]`, the probability that < k valid results appear in top-K is the CDF of Hypergeometric(n, s·n, K). To guarantee at least k valid results with probability `1 - δ`:

```
K ≥ (k · n) / (s · n) · (1 + √(2 · ln(1/δ) / (k/s))   [approximation]
```

For `s = 0.001, k = 10, n = 10⁶, δ = 0.01`: K ≈ 14,500 candidates needed.

### 2.2 Pre-filtering (WHERE + ANN)

Apply the metadata filter first to obtain a candidate set `V' ⊆ V`, then run ANN on `V'`.

**Used by**: Vespa (inverted index pre-filter), Weaviate (pre-filter mode).

**Strengths**: Exact filter guarantee — zero recall loss on metadata. Works well when `|V'|` is small.

**Weaknesses**: If filter is non-selective, `|V'|` is large and ANN must either rebuild a graph subset (slow) or brute-force (O(n·d)). Requires an inverted index per filterable field.

**Key insight**: For selective filters (`|V'| ≪ n`) this is optimal. For broad filters, it degrades to brute-force. The crossover point depends on the cost ratio of ANN distance computation vs brute-force scan.

### 2.3 Filtered-ANN (Pushdown en grafo)

Modify the ANN graph traversal to skip edges/vertices whose metadata doesn't match the filter. In HNSW, this means only adding a candidate to the result heap if its metadata satisfies the filter.

**Used by**: Qdrant (oversampling + scoring in HNSW), Weaviate (filtered HNSW), Milvus (bitset filter in IVF).

**Strengths**: No recall loss from post-filtering. Works within existing graph structure.

**Weaknesses**: The greedy descent in HNSW may still converge to a region of the graph where no nodes match the filter. If filter-compliant nodes are clustered in a different region, the search may miss them entirely. Qdrant mitigates this with oversampling — searching ef×oversampling_factor candidates then filtering.

**Implementation variants**:
- **Oversampling + Filter**: Run HNSW with ef_multiplier, filter results. Used by Qdrant.
- **Bitset Filtering**: Maintain a bitset for each metadata value; during traversal, check bitset before adding to heap. Used by Milvus.
- **Graph Bundling**: Build multiple HNSW graphs, each indexed by metadata partition. Route queries to the correct graph.

**Practical note**: Filtered-ANN with oversampling factor `f` has asymptotic cost `O(f · ef · log n)`. For selective filters, `f` must be large to compensate for graph structure not aligning with metadata boundaries.

### 2.4 Learned Partitioning (SPANN, FAISS IVF)

At index time, partition vectors by metadata or learned clusters; route queries to the relevant partition(s).

**SPANN (Microsoft, 2021)**: Two-tier index: (1) balanced k-means clustering of the entire vector space, (2) per-cluster inverted index with product quantization. At search time, find nearest centroids and search only the corresponding clusters. Extension: metadata-aware routing that only searches clusters whose centroid satisfies the metadata constraint.

**FAISS IVF with Metadata**: IVF stores centroids + inverted lists. Metadata-aware variant: at search time, scan only inverted lists whose metadata matches the filter.

**Strengths**: Scales to billion-scale datasets. SPANN achieves <1ms latency at 1B vectors.

**Weaknesses**: Partition boundaries are static — adding vectors with new metadata profiles may imbalance partitions. Metadata routing works well for categorical but poorly for range queries (e.g., `price < 100`).

**Theoretical insight**: Partitioning creates a hard boundary on search space. If the filter is `year > 2020`, and vectors from 2021 are spread across all clusters, metadata routing doesn't reduce the search space.

### 2.5 Hybrid Index (ANN + Inverted Index merge)

Maintain two independent indices: ANN for vector similarity, inverted index for metadata. Merge results using score normalization (e.g., reciprocal rank fusion, weighted linear combination).

**Used by**: Elasticsearch (dense_vector + boolean query), Vespa (weakAnd + nearestNeighbor), Milvus (hybrid search).

**Strengths**: Full expressivity — any filter supported, any score combination. Well-understood ranking theory (BM25 + vector similarity).

**Weaknesses**: Two index maintenance overhead. Score normalization is heuristic — no principled way to balance vector similarity and metadata relevance without training data. Recency bias: either signal can dominate.

**Score fusion approaches**:
- **Reciprocal Rank Fusion (RRF)**: `score = 1 / (k + rank_vec) + 1 / (k + rank_filter)`. Simple but discards distance magnitude.
- **Linear combination**: `score = α · sim_vec + (1 - α) · sim_filter`. Requires tuning α.
- **Learned combination**: Train a model to predict relevance from candidate features.

### 2.6 Product Quantization with Side Information

Extend product quantization (PQ) to encode metadata jointly with vector information. Instead of quantizing sub-vectors independently, allocate some codebook bits to metadata dimensions.

**Research**: Jegou et al. (2010) proposed PQ; extensions by André et al. (2017) incorporate side information into the codebook learning objective.

**Mechanism**: Given vector `x ∈ ℝᵈ` and metadata `m ∈ ℝᵐ` (one-hot encoded categories, normalized numeric), form augmented vector `x' = [x; α·m] ∈ ℝ^(d+m)` and quantize via PQ. Metadata columns share codebooks with vector columns.

**Strengths**: Metadata filtering is fused into the distance computation — no separate filter step. Minimal overhead during search (same PQ lookup table construction).

**Weaknesses**: Metadata dimensions compete with vector dimensions for codebook capacity. If `m` is large (many categories), the reconstruction error on vector dimensions increases. Creates false coupling: metadata values modulate vector distances at query time.

### 2.7 Manifold Alignment

Learn an aligned embedding space where vector similarity in a low-dimensional manifold correlates with metadata proximity. This is the research space closest to "topological index."

**Research**: Co-regularized spectral clustering (Kumar et al., 2011), manifold alignment (Wang & Mahadevan, 2009), and more recently contrastive learning with metadata supervision (Google's SCaLE, 2023).

**Mechanism**: Train a neural encoder `f: ℝᵈ → ℝᵏ` such that `||f(vᵢ) - f(vⱼ)||` is small when `mᵢ ≈ mⱼ` for query-relevant metadata attributes. The encoder is trained with a contrastive loss: positive pairs share metadata labels, negative pairs differ.

**Strengths**: Learned alignment can generalize to unseen metadata combinations. Natural fit for semantic search where metadata (user preferences, categories) is embedded alongside content.

**Weaknesses**: Requires training data (query-metadata relevance pairs). Cold-start for new metadata axes. The manifold may overfit to training metadata distributions. Static encoder requires retraining when metadata schema changes.

## 3. Análisis Matemático del Problema

Let the embedding space be `ℝᵈ` with distance metric `d(vᵢ, vⱼ)`. Each vector `vᵢ` has metadata `mᵢ ∈ M`, where `M` is the space of metadata tuples.

Define a **metadata distance function** `d_M: M × M → ℝ` that captures how "far apart" two metadata tuples are for query purposes. For structured data:

```
d_M(mᵢ, mⱼ) = Σⱼ wⱼ · δⱼ(mᵢ, mⱼ)
```

where `wⱼ` is the query-time importance weight and `δⱼ` is a per-attribute distance (e.g., 0 if equal, 1 if different for categories; normalized absolute difference for numerics).

The **ideal embedding** satisfies:

```
∀i,j,k: d(vᵢ, vⱼ) ≤ d(vᵢ, vₖ)  ⇔  d_M(mᵢ, mⱼ) ≤ d_M(mᵢ, mₖ)
```

i.e., the metric structure of the embedding space is **isomorphic** to the metric structure of the metadata space for any query context. This is unattainable in general (the metadata space may have higher cardinality than the embedding space can capture), but we can **approximate** it.

**The Tesseract approach** (from planner.rs + projection.rs): Represent metadata constraints as a **weight mask** `W ∈ [0,1]ᵈ` where `Wⱼ` amplifies (close to 1) or attenuates (close to 0) dimension `j` in the distance computation. The weighted distance `d_W(vᵢ, vⱼ) = d(W ⊙ vᵢ, W ⊙ vⱼ)` biases the search toward vectors in the metadata-compliant region.

**Key mathematical insight**: If metadata attributes correlate with specific embedding dimensions (e.g., `category = 'science'` activates dimensions 12, 45, 78), then zeroing out dimensions NOT associated with `science` (or amplifying those that are) creates a pseudo-manifold where vectors from different categories are farther apart, reducing the need for explicit filtering.

**Open problem**: The correlation between metadata attributes and embedding dimensions is an empirical property of the embedding model — not guaranteed. Foundation models (BERT, CLIP, text-embedding-3) produce distributed representations where attributes are spread across dimensions. The hash-based mapping in `field_to_dim` is a heuristic; learned mappings (Candidates B and D) may work better.

## 4. Candidatos para Tesseract

### Candidato A: Learned Weight Mask (lo que ya existe en planner.rs)

**Current implementation** (`planner.rs:294-318`):
- Hash metadata field name to a dimension index: `hash(field) % dim`
- Assign weight `1.0` for equality, `0.8` for ≠, `0.5` for range operators
- Build sparse WeightMask `Vec<(usize, f32)>`
- Fuse into HNSW distance computation via `mask_to_dense` → `distance_weighted`

**Status**: Already implemented and tested in the codebase.

**Strengths**: Zero additional index storage. Fused computation (no separate filter pass). Simple and predictable.

**Weaknesses**: Hash-based dimension mapping has no semantic basis — two related metadata fields (e.g., `year` and `decade`) map to random dimensions. Weight values are hardcoded heuristics. No support for `AND`/`OR` combinators in weight derivation.

**Mathematical limitation**: For cosine distance, `d_W(vᵢ, vⱼ) = 1 - Σⱼ Wⱼ² · vᵢⱼ · vⱼⱼ`. This is equivalent to projecting both vectors through a diagonal linear transformation `diag(W)`. The projection can only **shrink** dimensions (make them less important), not **remap** the space to align with metadata structure.

**Upgrade path**: Replace `field_to_dim` with a learnable mapping `f: field → (dim₁, α₁), (dim₂, α₂), ...` that distributes a metadata field's influence across multiple dimensions with learned weights.

### Candidato B: Conditional Dimension Gating

**Idea**: Learn a gating network `G(masked_fields) → [0,1]ᵈ` that produces a dense mask from the set of metadata field-value pairs in the query. Instead of hashing individual fields, the gating network learns which embedding dimensions are discriminative for each metadata attribute.

**Architecture**:
```
Query metadata: [(field: "category", op: "eq", value: "science"),
                 (field: "year", op: "gt", value: 2020)]

                    ↓ Embed each field-value into a vector eⱼ
                    ↓
               [e₁; e₂; ...] ∈ ℝᵏ
                    ↓
              Gating MLP: ℝᵏ → ℝᵈ (sigmoid output)
                    ↓
              Mask ∈ [0,1]ᵈ (fused into distance computation)
```

**Strengths**: Learns which dimensions actually matter for each metadata constraint. Can model interactions between metadata fields. Training objective: maximize recall@k on validation queries with metadata filters.

**Weaknesses**: Requires training data (queries + metadata filters + known relevant results). Cold-start for new metadata field combinations. MLP overhead at query time.

**Relationship to Candidate A**: This is a strict generalization — replace the hash-based `field_to_dim` with a learned dense mask.

**Training data**: Can bootstrap from existing logs: for each query with metadata filter, record the set of vectors that match the filter as positive examples. Train the gating network to minimize weighted distance for positive pairs vs negative pairs.

### Candidato C: Metadata-Concatenated Embedding

**Idea**: At index time, append metadata values to the embedding vector. At query time, append the desired metadata filter values to the query vector. Distance computation naturally accounts for metadata because both query and indexed vectors encode metadata in the same dimensions.

**Mechanism**:
```
Index time:
  v'_i = [v_i; one_hot(category_i); normalized(year_i); ...] ∈ ℝ^(d + m)

Query time:
  q' = [q; one_hot("science"); normalized(2020); ...]

Search: d(v'_i, q') = d_emb(v_i, q) + d_meta(metadata_i, query_metadata)
```

**Strengths**: Conceptually simple. Works with any ANN algorithm (no modifications needed). Metadata matching is implicit in the distance computation.

**Weaknesses**: Embedding dimension grows with number of metadata fields + cardinality. For high-cardinality categoricals (e.g., product IDs), one-hot encoding is infeasible. The distance function mixes embedding similarity and metadata similarity with equal weight — may reduce embedding sensitivity.

**Key design decision**: The relative weight `α` between embedding similarity and metadata similarity: `d_total = d_emb + α · d_meta`. If metadata dimensions dominate the total dimension count `(m ≫ d)`, metadata similarity dominates search results regardless of α.

**Practical variant**: Use a learned embedding for metadata (a small metadata encoder `E(m) → ℝᵖ` with `p ≪ m`), then `v'_i = [v_i; E(m_i)]`. This avoids the dimension explosion.

### Candidato D: Hyperplane Partitioning by Metadata

**Idea**: Partition the embedding space into regions defined by metadata-value hyperplanes. Each metadata value defines a decision boundary (hyperplane) in the embedding space. Vectors on the same side of the hyperplane as the query's desired metadata value are prioritized during search.

**Mechanism**:
- For each metadata field `f`, learn a hyperplane `w_f · x + b_f = 0` that separates vectors with different values of `f`.
- At index time, store for each vector its signed distance to each metadata hyperplane: `sᵢ,𝒻 = w_f · vᵢ + b_f`.
- At query time, given filter `f = value`, compute a bias score `biasᵢ = (sᵢ,𝒻 - τ_value) · sign` where `τ_value` is the centroid of vectors with that metadata value.
- Modify HNSW traversal: prefer edges where the bias score aligns with the filter.

**Strengths**: Learned decision boundaries are semantically grounded (optimally separates metadata values). Natural for binary/categorical filters. The bias score can be a small perturbation to the distance, avoiding hard partitioning.

**Weaknesses**: Requires per-field hyperplane learning (supervised). Metadata with many distinct values (e.g., `year` with 50 distinct values) needs multi-class separation (multiple hyperplanes or a softmax classifier). The bias integration into HNSW greedy search needs careful design — incorrect bias may misdirect the search.

**Similar to**: SphereFace / CosFace / ArcFace for face recognition, where learned angular margins separate identity classes. In Tesseract, metadata values are the "classes."

**Mathematical connection**: If the learned hyperplanes are orthogonal to embedding dimensions (i.e., `w_f = e_d` for some canonical basis vector), this reduces to Candidate A with hard alignment (weight = 0 or 1). The hyperplane approach is more general.

## 5. Recomendación

Immediate (Phase 3):

1. **Keep Candidate A as the baseline** — the current hash-based WeightMask in `planner.rs` is simple, tested, and solves the "metadata as geometric constraint" problem for simple equality filters with zero additional storage. The heuristic weights (1.0 for Eq, 0.8 for Neq, 0.5 for range) are a pragmatic starting point.

2. **Instrument recall metrics** — before upgrading, measure recall@k with and without WeightMask for various filter selectivities. The gap between theoretical optimal (pre-filter + ANN) and actual (WeightMask + ANN) determines whether improvements are needed.

Next phase:

3. **Implement Candidate B (Conditional Dimension Gating)** as the primary upgrade path. The gating MLP replaces the hash-based `field_to_dim` with a learned dense mask that uses actual data distribution to map metadata fields to discriminating dimensions. Training data can be bootstrapped from the index itself (for each metadata value, the "ground truth relevant" set is the set of vectors matching that value).

4. **Training pipeline**: For each metadata field `f`:
   - Positive pairs: vectors where `f` matches
   - Negative pairs: vectors where `f` differs
   - Loss: contrastive margin loss over weighted cosine distance
   - Gating network input: field embedding + value embedding
   - Gating network output: `[0,1]ᵈ` mask

5. **Keep Candidate D (Hyperplane Partitioning) as long-term research** — it requires supervised training with metadata labels, which adds operational complexity. Revisit when Tesseract has a production query log large enough to train reliable hyperplanes.

Not recommended:

- **Candidate C (Metadata-Concatenated Embedding)** — adds too much noise to the embedding space, increases dimension, and mixes semantics arbitrarily. The learned approach (B or D) is strictly superior because it controls the weight between embedding and metadata similarity.

## 6. Referencias

- Malkov, Y. A., & Yashunin, D. A. (2016). "Efficient and robust approximate nearest neighbor search using Hierarchical Navigable Small World graphs." *IEEE TPAMI*, 42(4), 824-836. — HNSW base algorithm used by Tesseract.

- Johnson, J., Douze, M., & Jégou, H. (2019). "Billion-scale similarity search with GPUs." *IEEE TBD*, 7(3), 535-547. — FAISS IVF with product quantization.

- Jégou, H., Douze, M., & Schmid, C. (2010). "Product quantization for nearest neighbor search." *IEEE TPAMI*, 33(1), 117-128. — PQ foundation.

- Chen, Q., Zhao, B., Wang, H., et al. (2021). "SPANN: Highly efficient billion-scale approximate nearest neighbor search." *NeurIPS 2021*. — Learned partitioning for large-scale ANN.

- André, F., Kermarrec, A.-M., & Le Scouarnec, N. (2017). "Cache locality is not enough: high-performance nearest neighbor search with product quantization fast scan." *VLDB 2017*. — PQ with side information.

- Kumar, A., Rai, P., & Daumé, H. (2011). "Co-regularized multi-view spectral clustering." *NeurIPS 2011*. — Manifold alignment for multi-view learning.

- Wang, C., & Mahadevan, S. (2009). "Manifold alignment without correspondence." *IJCAI 2009*. — Unsupervised manifold alignment.

- Google Research (2023). "SCaLE: Towards Semantic Caching by Aligning Learned Embeddings." — Contrastive learning with metadata supervision.

- Musgrave, K., Belongie, S., & Lim, S.-N. (2020). "A metric learning reality check." *ECCV 2020*. — Critical analysis of learned embeddings and margin-based losses.

- Bernhardsson, E. (2018). "Annoy: Approximate Nearest Neighbors Oh Yeah." Spotify. — Random projection trees, precursor to learned partitioning.

- Baranchuk, D., Persiyanov, D., Sinitsin, A., & Babenko, A. (2019). "Learning to route in similarity search." *ICML 2019*. — Learned routing in partitioned ANN.

- Wu, Z., Xiong, Y., Yu, S., & Lin, D. (2019). "Unsupervised feature learning via non-parametric instance-level discrimination." *CVPR 2018*. — Contrastive learning for embeddings, theoretical basis for metadata-contrastive losses.

- Douze, M., Sablayrolles, A., & Jégou, H. (2021). "Link and code: Fast indexing with graphs and compact coding." *CVPR 2021*. — Combined graph + PQ, relevant for weighted HNSW.

- Tesseract internal: `tesseract-core/src/projection.rs` — `WeightMask` and `Projection` trait definitions.
- Tesseract internal: `tesseract-vql/src/planner.rs` — `derive_weight_mask()`, `field_to_dim()`, `PlanNode::AnnScan.weight_mask`.
- Tesseract internal: `tesseract-index/src/distance.rs` — `mask_to_dense()`, `DistanceComputer::distance_weighted()`.
- Tesseract internal: `tesseract-index/src/hnsw.rs` — HNSW `search()` with fused weighted distance.
