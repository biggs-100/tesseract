# Index Benchmarks Specification

## Purpose

The benchmark suite measures HNSW index performance — recall, latency, build time, and memory — against a brute-force baseline and a FAISS HNSW implementation. It validates that the custom implementation is competitive with established libraries.

## Requirements

### Requirement: Criterion Measurement Framework

The benchmark suite MUST use the criterion crate for all measurements. Every benchmark MUST be a criterion group with at least 10 warmup iterations and 100 measurement iterations.

#### Scenario: Criterion report generated

- GIVEN the benchmark suite is run via `cargo bench`
- WHEN all benchmarks complete
- THEN a criterion HTML report MUST be written to target/criterion/

### Requirement: Recall@k Against Brute Force

The suite MUST measure recall@k as: `|correct_ann ∩ brute_force_topk| / k`. Brute-force MUST compute exact distances by scanning all indexed vectors. Recall MUST be reported for k ∈ {1, 10, 100}.

#### Scenario: Recall measured for multiple k values

- GIVEN a built HNSW index with 10K synthetic vectors
- WHEN recall is computed for k=1, k=10, and k=100
- THEN recall@100 >= recall@10 >= recall@1 (monotonic property)

### Requirement: Latency Percentiles

The suite MUST measure search latency in microseconds and report P50, P95, and P99 across all query runs. Latency MUST be measured from query submission to sorted result return.

#### Scenario: Latency reported per ef value

- GIVEN a built index
- WHEN latency is measured at ef ∈ {64, 128, 256}
- THEN P50(ef=256) >= P50(ef=128) >= P50(ef=64) (higher ef = slower)

### Requirement: Build Time Measurement

The suite MUST measure wall-clock build time for inserting N vectors sequentially. Build time MUST be reported for N ∈ {1K, 10K, 100K} where dataset size permits.

#### Scenario: Build time scales with N

- GIVEN a synthetic dataset
- WHEN build time is measured for 1K, 10K, and 100K vectors
- THEN build time MUST increase sub-quadratically with N (expected O(N log N) behavior)

### Requirement: Memory Usage

The suite MUST measure peak RSS memory usage during index construction and after construction. Memory MUST be reported in MB.

#### Scenario: Memory measured during construction

- GIVEN a 100K vector dataset
- WHEN the index is being built
- THEN peak memory usage MUST be reported in the criterion output

### Requirement: FAISS HNSW Baseline Comparison

The suite MUST compare the custom HNSW against a FAISS HNSW index (IndexHNSWFlat) on the same dataset with the same parameters (M, ef_construction, ef_search). The comparison MUST report the recall and latency ratio (custom / FAISS).

#### Scenario: FAISS comparison outputs ratio

- GIVEN a dataset and identical parameters for both custom HNSW and FAISS
- WHEN both indices are built and queried
- THEN the benchmark MUST report recall_custom, recall_faiss, and the latency ratio

### Requirement: Multiple Datasets

The suite MUST benchmark against at least two datasets:

| Dataset | Type | Source |
|---------|------|--------|
| Synthetic | 10K random 128-d vectors | Generated inline via rand |
| SIFT1M | 1M 128-d vectors | Downloaded fixture (fixtures/siftsmall/siftsmall_base.fvecs) |

#### Scenario: SIFT1M fixture downloaded before run

- GIVEN the benchmark binary is executed
- WHEN the SIFT1M benchmark group is reached
- THEN the fixture MUST be downloaded if not cached locally

#### Scenario: Both datasets produce valid results

- GIVEN both synthetic and SIFT1M benchmarks
- WHEN both complete
- THEN recall for SIFT1M MUST exceed recall for synthetic at same parameters (real data is more structured)

### Requirement: Weighted and Unweighted Query Modes

The suite MUST benchmark both standard (unweighted) distance queries and weighted distance queries. Weighted queries MUST use a non-uniform WeightMask. The weighted benchmark MUST measure the same metrics (recall, latency).

#### Scenario: Weighted query latency is comparable

- GIVEN a built index
- WHEN unweighted and weighted latency are measured with identical ef
- THEN weighted latency SHOULD be within 20% of unweighted latency (fused inline cost is minimal)
