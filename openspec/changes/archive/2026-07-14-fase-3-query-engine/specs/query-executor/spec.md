# Query Executor Specification

## Purpose

The query executor takes a `QueryPlan` and executes it against the `StorageEngine`, producing scored, sorted, and limited results.

## Requirements

### Requirement: Plan execution against StorageEngine

The executor MUST execute a `QueryPlan` against the `StorageEngine`, performing HNSW search with the derived `WeightMask`.

#### Scenario: Execute plan with mask

- GIVEN a `QueryPlan` with a query vector and a `WeightMask`
- WHEN `executor.execute(plan, engine)` is called
- THEN the executor calls `engine.search(query_vector, ef, Some(mask))`
- AND the raw `(VectorId, f32)` candidates are returned

### Requirement: Text-to-embedding via EmbeddingService

When `SIMILARITY` contains text instead of a pre-computed vector, the executor MUST use the `EmbeddingService` to generate the embedding before search.

#### Scenario: Text query generates embedding

- GIVEN a plan where `query_text` is `"quantum computing"`
- WHEN the executor begins execution
- THEN the executor calls `EmbeddingService::embed("quantum computing", model)`
- AND uses the resulting vector for the HNSW search

### Requirement: Pre-computed vector acceptance

The executor MUST accept pre-computed vectors directly when `SIMILARITY` contains vector data, skipping the embedding step.

#### Scenario: Direct vector query skips embedding

- GIVEN a plan where `query_vector` is pre-computed (no text-to-embedding needed)
- WHEN the executor begins execution
- THEN no embedding call is made
- AND the vector is used directly for HNSW search

### Requirement: Result sorting and LIMIT

The executor MUST sort results by distance ascending (closest first), then apply the `LIMIT` clause to truncate the result set.

#### Scenario: Results sorted and limited

- GIVEN a plan with `limit = 10` and 200 raw candidates from HNSW
- WHEN the executor processes candidates
- THEN results are sorted by raw distance ascending
- AND only the top 10 results are returned

### Requirement: ScoredRecord return type

The executor MUST return `Vec<ScoredRecord>` where each record contains `{ id: VectorId, score: f32, metadata: Option<Value> }`.

#### Scenario: Returns ScoredRecords

- GIVEN a completed execution pipeline
- WHEN the executor constructs the result
- THEN each result is a `ScoredRecord` with `id`, `score` (converted from distance), and `metadata` from `VectorRecord`
- AND metadata is fetched via `engine.batch_get(ids)`

### Requirement: WITHIN deadline enforcement

If the plan has a latency budget, the executor MUST check elapsed time and truncate remaining candidates if the budget is exceeded.

#### Scenario: Budget exceeded truncates results

- GIVEN a plan with `latency_budget_ms = 100`
- WHEN the elapsed time exceeds 100ms during result processing
- THEN the executor truncates remaining candidates
- AND logs a warning about the budget being exceeded
