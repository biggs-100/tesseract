# Query Planner Specification

## Purpose

The query planner converts a parsed VQL Query AST into an executable `QueryPlan` with cost estimation, WeightMask derivation from metadata predicates, and latency budget enforcement.

## Requirements

### Requirement: AST to QueryPlan conversion

The planner MUST convert a VQL `Query` AST into a `QueryPlan` struct containing field, query vector, WeightMask, ef parameter, limit, scoring function, sort order, latency budget, and estimated cost.

#### Scenario: Full query plan

- GIVEN a parsed `Query` AST with `SIMILARITY`, `WITH METADATA WHERE`, `ORDER BY`, `LIMIT`, and `WITHIN` clauses
- WHEN `planner.plan(query, context)` is called
- THEN a complete `QueryPlan` is returned with all fields populated
- AND the plan is ready for executor consumption

### Requirement: WeightMask derivation

The planner MUST derive a `WeightMask` from `WITH METADATA WHERE` predicates. Equality predicates produce low-selectivity estimates; range predicates produce medium-selectivity; IN predicates scale with value count.

#### Scenario: Metadata WHERE produces WeightMask

- GIVEN a `Query` with `WITH METADATA WHERE category = 'science'`
- WHEN the planner processes the metadata predicate
- THEN a `WeightMask` is produced from the predicate field and value
- AND the mask is attached to the `QueryPlan`

#### Scenario: Empty WHERE produces no mask

- GIVEN a `Query` with no `WITH METADATA WHERE` clause
- WHEN the planner processes the AST
- THEN `QueryPlan.weight_mask` is `None`
- AND the search operates on the full vector space

### Requirement: Cost estimation

The planner MUST estimate the cost of each plan node including HNSW search cost, metadata filter cost, and scoring cost. The estimate MUST use the formula: `total = hnsw_search_cost + metadata_filter_cost + scoring_cost`.

#### Scenario: Cost estimated from query parameters

- GIVEN an index of N vectors, dimension D, ef parameter E, and M connections per node
- WHEN `planner.estimate_cost(plan)` is called
- THEN the estimated cost is computed as `E × M × max(1, log2(N)) × D × 10ns + selectivity × candidates + limit × scoring_ns`
- AND the result is stored in `QueryPlan.estimated_cost_ms`

### Requirement: Latency budget enforcement

The planner MUST reject a query if no plan can meet the `WITHIN` latency budget. The planner SHOULD reduce `ef` parameter to meet the budget when possible, trading recall for latency.

#### Scenario: Plan fits within budget

- GIVEN a `Query` with `WITHIN 200ms` and a plan estimated at 150ms
- WHEN the planner validates the budget
- THEN the plan is accepted and `QueryPlan.latency_budget_ms = 200`

#### Scenario: Plan cannot meet budget

- GIVEN a `Query` with `WITHIN 10ms` and even minimum `ef` exceeds the budget
- WHEN the planner optimizes by reducing `ef` to the minimum
- AND the minimum-cost plan still exceeds the budget
- THEN the planner returns an error indicating the query cannot be satisfied within the budget

#### Scenario: Budget optimization reduces ef

- GIVEN a `Query` with `WITHIN 100ms` and the default `ef=200` is estimated at 300ms
- WHEN the planner computes `target_ef = (100 - fixed_overhead) / cost_per_ef_step`
- THEN the plan uses the reduced `ef`
- AND the estimated cost is within the budget

### Requirement: Absence of WITHIN clause

If no `WITHIN` clause is specified, the planner MUST use a default `ef` from configuration (default: 200).

#### Scenario: No WITHIN uses default ef

- GIVEN a `Query` with no `WITHIN` clause
- WHEN the planner creates the plan
- THEN `QueryPlan.ef = 200` (or the configured default)
- AND no latency budget validation is performed
