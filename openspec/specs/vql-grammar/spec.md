# VQL — Vector Query Language Specification

> **Status**: Formal Specification v1.0
> **Domain**: vql-grammar
> **Applies to**: Tesseract semantic-relational layered database engine

---

## 1. Introduction

VQL (Vector Query Language) is the native query language for Tesseract. It is **not SQL** — it is a purpose-built language for expressing semantic similarity search combined with structured metadata filtering, latency constraints, personalization, and topological projection.

This document is the **source of truth** for VQL syntax, semantics, and the bridge to execution algebra. It governs both the parser implementation and the query planner contract.

### 1.1 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **FIND-first** | Every query is a search. There is no `SELECT`. The FIND keyword anchors the intent. |
| **Clause-order flexibility** | After the mandatory FIND clause, all other clauses may appear in any order. The planner assembles the pipeline. |
| **Latency as a first-class constraint** | WITHIN / EN are not hints — they are budgets the planner MUST respect or reject. |
| **Topological projection** | Metadata dimensions can be embedded into the vector space, making filtering a geometric constraint (O(log n)) instead of a post-filter (O(n)). |

---

## 2. Grammar (EBNF)

```
(* Top-level entry point *)
query             = find_clause, { clause } ;

(* FIND clause — mandatory, exactly once *)
find_clause       = "FIND", find_type, "(", similarity_args, ")" ;
find_type         = "SIMILARITY"
                  | "SEMANTIC" ;
similarity_args   = field, ",", query_source ;
query_source      = string_literal          (* text to embed at query time *)
                  | "VECTOR", "(", float_list, ")" ;  (* pre-computed vector *)

(* Clauses — any order after FIND, zero or more *)
clause            = metadata_where_clause
                  | project_on_clause
                  | bias_clause
                  | order_by_clause
                  | limit_clause
                  | within_clause
                  | en_clause ;

(* ── Metadata filter ── *)
metadata_where_clause = "WITH", "METADATA", "WHERE", predicate_expression ;

predicate_expression = predicate, { "AND", predicate } ;
predicate         = comparison_predicate
                  | in_predicate
                  | between_predicate
                  | like_predicate ;
comparison_predicate = field, comparison_op, literal ;
in_predicate      = field, "IN", "(", literal_list, ")" ;
between_predicate = field, "BETWEEN", literal, "AND", literal ;
like_predicate    = field, "LIKE", string_literal ;
comparison_op     = "=" | "!=" | "<" | ">" | "<=" | ">=" ;

(* ── Topological projection ── *)
project_on_clause = "PROJECT", "ON", projection_list ;
projection_list   = projection, { ",", projection } ;
projection        = field
                  | field, "AS", alias
                  | function, "(", field, ")" ;

(* ── Bias / personalization ── *)
bias_clause       = "BIAS", scoring_fn, "(", args, ")" ;

(* ── Ordering ── *)
order_by_clause   = "ORDER", "BY", scoring_fn, "(", args, ")", [ "DESC" | "ASC" ] ;

(* ── Pagination ── *)
limit_clause      = "LIMIT", integer, [ "OFFSET", integer ] ;

(* ── Latency budget (two syntax variants) ── *)
within_clause     = "WITHIN", integer, "ms" ;
en_clause         = "EN", integer, "ms" ;

(* ── Scoring functions ── *)
scoring_fn        = identifier ;
args              = [ identifier, { ",", identifier } ] ;

(* ── Literals ── *)
literal           = string_literal
                  | integer_literal
                  | float_literal
                  | boolean_literal
                  | null_literal ;
literal_list      = literal, { ",", literal } ;
float_list        = float, { ",", float } ;
field             = identifier ;
alias             = identifier ;
function          = identifier ;

(* ── Lexical tokens ── *)
identifier        = ( letter | "_" ), { letter | digit | "_" } ;
string_literal    = "'", { character }, "'" ;
integer_literal   = digit, { digit } ;
float_literal     = digit, { digit }, ".", digit, { digit } ;
boolean_literal   = "true" | "false" ;
null_literal      = "null" ;
letter            = "A" .. "Z" | "a" .. "z" ;
digit             = "0" .. "9" ;
float             = digit, { digit }, ".", digit, { digit } ;
```

### 2.1 Grammar Notes

- Keywords are **case-insensitive**: `find`, `Find`, `FIND` are all valid. Identifiers and string literals are case-sensitive.
- Whitespace is flexible: spaces, tabs, and newlines are allowed between any tokens.
- Single quotes (`'...'`) delimit string literals. There is no escape sequence — the first unescaped `'` terminates the literal.
- The `VECTOR(...)` argument accepts a comma-separated list of floats inside parentheses.
- `WITHIN` and `EN` are strict aliases: identical semantics, different keyword.

---

## 3. Semantics

### 3.1 FIND SIMILARITY(field, source)

Semantic vector search against the embedding field `field`. The `source` is either:
- A **string literal**: embedded at query time using the configured embedding model.
- A **VECTOR(...)** literal: a pre-computed embedding used directly (no model call).

Returns scored records ranked by **cosine similarity** between the query vector and each stored vector in the field. The score is a `f32` in the range `[-1.0, 1.0]` (cosine similarity of normalized vectors) or `[0.0, 1.0]` if the index normalizes internally.

### 3.2 FIND SEMANTIC(field, source)

Higher-level variant of `FIND SIMILARITY` with three behavioral differences:

1. **Episodic memory biasing is active by default**: if a user context (`user_id`) is present in the session, the query vector is biased by the user's episodic footprint before search.
2. **Automatic field selection**: if the embedding field name is omitted or set to `*`, Tesseract auto-selects the best embedding field based on the query type.
3. **Default scoring**: `BIAS` is implied with the `personal(user_id)` function when user context is available.

All other clauses behave identically to `FIND SIMILARITY`.

### 3.3 WITH METADATA WHERE predicate

Filters search results by structured metadata. The predicate expression supports:

| Predicate | Syntax | Semantics |
|-----------|--------|-----------|
| Equality | `field = literal` | Exact match |
| Inequality | `field != literal` | Not equal |
| Less than | `field < literal` | Numeric/string comparison |
| Greater than | `field > literal` | Numeric/string comparison |
| Less or equal | `field <= literal` | Numeric/string comparison |
| Greater or equal | `field >= literal` | Numeric/string comparison |
| IN list | `field IN (v1, v2, ...)` | Field value matches any in list |
| BETWEEN | `field BETWEEN low AND high` | Field value in closed interval `[low, high]` |
| LIKE | `field LIKE pattern` | SQL-style `%` wildcard pattern match |

Multiple predicates combine with `AND`. There is no `OR` in VQL v1.

**Critical: these are NOT post-filters by default.** When a field is registered via `PROJECT ON`, filtering on it becomes a geometric constraint applied during HNSW traversal (O(log n)). When a field is **not** projected, the planner falls back to post-filtering (O(n)).

### 3.4 PROJECT ON field1, field2, ...

Declares which metadata dimensions are topologically projected into the HNSW vector space. For each projected field, Tesseract:

1. Allocates one or more dimensions in the embedding space.
2. Learns (or accepts a pre-configured) mapping from the field's values to a region in that subspace.
3. At search time, a `WITH METADATA WHERE` predicate on a projected field becomes a **geometric constraint**: the HNSW traversal only visits nodes whose projected coordinates fall within the constraint bounds.

**Constraints on PROJECT ON**:
- Can only reference metadata fields, not vector fields.
- A field with high cardinality or continuous values (e.g., `price`) requires more dimensions for accurate projection.
- The index topology is rebuilt when `PROJECT ON` fields change. This is an index-time operation, not a query-time operation.

### 3.5 BIAS scoring_fn(args)

Applies a scoring function that adjusts ranking based on user context. Overrides the default similarity ranking.

**Built-in bias functions**:

| Function | Signature | Behavior |
|----------|-----------|----------|
| `recency()` | `recency()` | Boosts scores of recently inserted/updated records. Decay curve: `score * exp(-days / 30)`. |
| `popularity()` | `popularity()` | Boosts scores by global popularity score (from click tracking). |
| `relevance_clicks(user_id)` | `relevance_clicks(identifier)` | Uses the user's click history to boost results the user has engaged with. |
| `personal(user_id)` | `personal(identifier)` | Episodic memory footprint: element-wise multiplies the query vector by the user's accumulated footprint. |

**Mutual exclusion rule**: `BIAS` and `ORDER BY` MUST NOT appear together. If both are present, the planner returns an error. They are two ways of defining ranking, and allowing both would create an ambiguity about which takes precedence.

### 3.6 ORDER BY scoring_fn(args) [DESC | ASC]

Explicit ordering of results. Default direction is `DESC` (highest score first).

**Built-in order functions**:

| Function | Signature | Behavior |
|----------|-----------|----------|
| `similarity()` | `similarity([field])` | Cosine similarity score. Optional field name for multi-field indexes. |
| `recency()` | `recency()` | Recency score (same as bias variant). |
| `score()` | `score()` | Combined score after all adjustments (similarity + bias + metadata weight). |
| `relevance_clicks(user_id)` | `relevance_clicks(identifier)` | Personal relevance score from click history. |
| `popularity()` | `popularity()` | Global popularity score. |

### 3.7 LIMIT n [OFFSET m]

Returns at most `n` results, skipping the first `m`. 

- Default `LIMIT` is **10** when not specified.
- `LIMIT` cannot exceed **10,000** without an explicit server-side override flag.
- `OFFSET` requires `LIMIT`. If `OFFSET` is present without `LIMIT`, the parser returns an error.
- OFFSET combined with large LIMIT values degrades performance because HNSW must scan `n + m` candidates internally.

### 3.8 WITHIN n ms / EN n ms

Latency budget. The planner MUST select a strategy (`ef_search`, index tier, fallback) that completes within `n` milliseconds.

- If the cost model predicts the query cannot complete within the budget, the planner returns an error **before any execution**.
- `WITHIN` and `EN` are exact aliases. Choice is stylistic: `EN` (from Spanish *en* "in") provides a shorter keyword.
- The planner uses a cost model: `cost = ef_search × dim × 2 × log(n) × cost_per_distance_ms`.
- `ef_search` is computed from the budget using inverse scaling: `ef = clamp(default_ef × (budget / 100), 10, 200)`.

---

## 4. Extended Relational Algebra

VQL maps to a set of algebra operators that bridge syntax and execution:

| VQL Clause | Algebra Operator | Notation | Execution Strategy |
|---|---|---|---|
| `FIND SIMILARITY(f, src)` | Semantic Join | ⨝⨝ | Embed src (if text) → HNSW search with `ef_search` |
| `FIND SEMANTIC(f, src)` | Semantic Join + Bias | ⨝⨝ + φ | Like SIMILARITY but with implicit episodic biasing |
| `WITH METADATA WHERE p` | Selection | σ | Projected fields: geometric constraint in HNSW. Others: post-filter. |
| `PROJECT ON f1, f2` | Topological annotation | — | Registers dimensions for topological projection at index time |
| `BIAS fn(args)` | Bias | φ | Apply episodic footprint to query vector before search |
| `ORDER BY fn(args) [D]` | Sort | τ | Re-rank results using scoring function |
| `LIMIT n [OFFSET m]` | Limit | λ | Take `n` after skipping `m` |
| `WITHIN n ms` / `EN n ms` | Deadline | ⏱ | Compute `ef_search` from cost model; reject if infeasible |

### 4.1 Pipeline Composition

The canonical execution pipeline composes operators left-to-right, with the FIND driving the first data flow:

```
query_vector = embed(src)                        (* Step 1: Vectorization *)
if BIAS present: query_vector = φ(query_vector)  (* Step 2: Episodic bias *)
if PROJECT ON:  select index tier by topology     (* Step 3: Index routing *)
R = ⨝⨝(query_vector, ef)                        (* Step 4: ANN search *)
R = σ(R, predicates)                              (* Step 5: Metadata filter *)
if ORDER BY:    R = τ(R, scoring_fn)              (* Step 6: Re-rank *)
R = λ(R, n, m)                                    (* Step 7: Paginate *)
⏱(R, budget)                                      (* Step 8: Deadline check *)
```

### 4.2 Cost Model

```
cost = ef_search × dim × 2 × log(estimated_n) × cost_per_distance_ms × (1 + buffer)

where:
  ef_search           = clamp(default_ef × (budget / 100), 10, 200)
  dim                 = embedding dimension (default 384)
  estimated_n         = estimated vector count in the index
  cost_per_distance_ms = hardware-dependent constant (default 0.001)
  buffer              = safety margin (default 0.2 = 20%)
```

### 4.3 Projection-Aware Execution

When `PROJECT ON` is active, the algebra changes:

```
Without PROJECT ON:
  R = σ_metadata(⨝⨝(query_vector))   ← post-filter: O(n)

With PROJECT ON:
  R = σ_geometric(⨝⨝(query_vector))  ← in-filter: O(log n)
```

The `σ_geometric` operator is pushed into the HNSW graph traversal: nodes whose projected coordinates fall outside the constraint are pruned during the search, not after it.

---

## 5. Constraints & Validation Rules

### 5.1 Structural Constraints

| # | Rule | Error |
|---|------|-------|
| 1 | `FIND` is mandatory. At most one `FIND` per query. | `Missing FIND clause` |
| 2 | `FIND SIMILARITY` requires a `similarity_args` with a valid field and source. | `Invalid similarity expression` |
| 3 | `BIAS` and `ORDER BY` are mutually exclusive. | `BIAS and ORDER BY are mutually exclusive` |
| 4 | `OFFSET` requires `LIMIT`. | `OFFSET requires LIMIT` |
| 5 | `PROJECT ON` can only reference metadata fields, not vector fields. | `Cannot project on vector field: {field}` |
| 6 | `LIMIT` defaults to 10. Maximum 10,000 without server-side override. | `LIMIT exceeds maximum (10000)` |
| 7 | `WITHIN` / `EN` value must be ≥ 1 ms. | `Latency budget must be at least 1ms` |

### 5.2 Semantic Constraints

| # | Rule |
|---|------|
| 8 | String literals in predicates are compared lexicographically for `<`, `>`, `<=`, `>=`. |
| 9 | Type mismatches in predicates (e.g., comparing a numeric field to a string literal) produce a runtime warning but are not parse errors. Comparison semantics follow the literal type. |
| 10 | The `LIKE` predicate supports `%` as a multi-character wildcard. There is no single-character wildcard in VQL v1. |
| 11 | `PROJECT ON` is a hint to the index topology. If the index does not support topological projection for the specified fields, the planner degrades to post-filtering with a warning. |
| 12 | Episodic memory biasing (via `BIAS personal(user_id)` or `FIND SEMANTIC`) requires a `user_id` to be provided at execution time. If none is provided, the bias is silently skipped. |

### 5.3 Planner-Defined Constraints

| # | Rule |
|---|------|
| 13 | If the cost model predicts execution exceeds the `WITHIN`/`EN` budget, the planner MUST return an error before execution. |
| 14 | The planner MAY downgrade `ef_search` below the minimum (10) only if the budget is < 1ms and the query would be rejected otherwise. |
| 15 | `ORDER BY similarity()` re-ranks results using the same cosine similarity metric as the search. Since HNSW returns approximate results, this can change the ordering of borderline results. |

---

## 6. Examples

### Example 1: Basic semantic search

```vql
FIND SIMILARITY(emb, 'quantum computing') LIMIT 20
```

- **Clauses**: `FIND SIMILARITY`, `LIMIT`
- **Semantics**: Embed `"quantum computing"` → search HNSW on field `emb` → return top 20
- **Algebra**: `λ(⨝⨝(embed("quantum computing"), ef=50), 20)`

---

### Example 2: Metadata filter (equality)

```vql
FIND SIMILARITY(emb, 'quantum computing') WITH METADATA WHERE year >= 2020 LIMIT 10
```

- **Clauses**: `FIND SIMILARITY`, `WITH METADATA WHERE`, `LIMIT`
- **Semantics**: Filter results where `year >= 2020`. If `year` is projected, the filter is geometric.
- **Algebra**: `λ(σ_year≥2020(⨝⨝(embed("quantum computing"))), 10)`

---

### Example 3: Pre-computed vector + latency budget

```vql
FIND SIMILARITY(emb, VECTOR(0.1, 0.2, 0.3)) WITHIN 50ms
```

- **Clauses**: `FIND SIMILARITY` (with VECTOR source), `WITHIN`
- **Semantics**: Use the pre-computed vector directly (no embedding call). Planner computes `ef_search` for 50ms budget.
- **Algebra**: `λ(⨝⨝(VECTOR[0.1, 0.2, 0.3], ef=25), 10)` (budget 50ms → ef ~25)

---

### Example 4: FIND SEMANTIC with bias

```vql
FIND SEMANTIC(emb, 'machine learning') BIAS recency() LIMIT 10
```

- **Clauses**: `FIND SEMANTIC`, `BIAS`, `LIMIT`
- **Semantics**: Higher-level search with implicit personalization. Bias by recency.
- **Algebra**: `λ(φ_recency(⨝⨝(embed("machine learning"))), 10)`
- **Key difference vs SIMILARITY**: `FIND SEMANTIC` also applies episodic footprint if `user_id` is available.

---

### Example 5: Full topological projection

```vql
FIND SIMILARITY(emb, 'climate') PROJECT ON year, category
  WITH METADATA WHERE year BETWEEN 2020 AND 2025
    AND category IN ('science', 'policy')
  LIMIT 20
```

- **Clauses**: `FIND SIMILARITY`, `PROJECT ON`, `WITH METADATA WHERE` (BETWEEN + IN), `LIMIT`
- **Semantics**: Both `year` and `category` are topologically projected. The BETWEEN and IN filters are geometric constraints during HNSW traversal, not post-filters.
- **Algebra**: `λ(σ_geometric(⨝⨝(embed("climate"))), 20)`

---

### Example 6: LIKE text filter

```vql
FIND SIMILARITY(emb, 'recipe') WITH METADATA WHERE cuisine LIKE 'ita%' LIMIT 5
```

- **Clauses**: `FIND SIMILARITY`, `WITH METADATA WHERE` (LIKE), `LIMIT`
- **Semantics**: Filter `cuisine` fields starting with `"ita"` (matches "italian", "italian-fusion", etc.)
- **Note**: LIKE is always a post-filter since text pattern matching cannot be geometrically projected.

---

### Example 7: Personalized ordering with latency

```vql
FIND SIMILARITY(emb, 'history') ORDER BY relevance_clicks(current_user) DESC
  LIMIT 5 WITHIN 200ms
```

- **Clauses**: `FIND SIMILARITY`, `ORDER BY`, `LIMIT`, `WITHIN`
- **Semantics**: Search semantically, then re-rank by the user's click history. Planner ensures completion within 200ms.
- **Algebra**: `⏱(λ(τ_relevance(⨝⨝(embed("history")), current_user), 5), 200ms)`

---

### Example 8: Pagination with OFFSET

```vql
FIND SIMILARITY(emb, 'music') LIMIT 50 OFFSET 100
```

- **Clauses**: `FIND SIMILARITY`, `LIMIT` + `OFFSET`
- **Semantics**: Skip the first 100 results, return the next 50. HNSW must internally scan at least 150 candidates.
- **Algebra**: `λ(⨝⨝(embed("music")), 50, 100)`

---

### Example 9: IN filter + score ordering

```vql
FIND SIMILARITY(emb, 'AI') WITH METADATA WHERE tags IN ('deep-learning', 'nlp')
  ORDER BY score() LIMIT 10
```

- **Clauses**: `FIND SIMILARITY`, `WITH METADATA WHERE` (IN), `ORDER BY`, `LIMIT`
- **Semantics**: Filter by tag membership, order by combined score (similarity + any implicit adjustments).
- **Algebra**: `λ(τ_score(σ_tags(⨝⨝(embed("AI")))), 10)`

---

### Example 10: EN latency (alias variant)

```vql
FIND SIMILARITY(emb, 'robotics') EN 100ms LIMIT 25
```

- **Clauses**: `FIND SIMILARITY`, `EN` (alias for WITHIN), `LIMIT`
- **Semantics**: Identical to `WITHIN 100ms`. The `EN` keyword is a shorter alternative.
- **Algebra**: `⏱(λ(⨝⨝(embed("robotics"), ef=50), 25), 100ms)`

---

## 7. AST Reference

The parser produces a typed AST with these node types (reflecting the current Rust implementation in `tesseract-vql/src/ast.rs`):

```
Query
├── find: String                    ("SIMILARITY" | "SEMANTIC")
├── similarity: Option<SimilarityExpr>
│   ├── field: String
│   ├── query_text: String
│   └── vector: Option<Vec<f64>>
├── metadata_where: Option<MetadataWhere>
│   └── predicates: Vec<Predicate>
│       ├── Comparison { field, operator: ComparisonOp, value: Literal }
│       │   operator ∈ { Eq, Neq, Lt, Gt, Lte, Gte }
│       ├── In { field, values: Vec<Literal> }
│       ├── Between { field, low: Literal, high: Literal }
│       └── And(Vec<Predicate>)
├── order_by: Option<OrderBy>
│   ├── scoring_fn: String
│   ├── args: Vec<String>
│   └── descending: bool
├── limit: Option<Limit>
│   └── count: u64
├── offset: Option<Offset>          (* NEW in this spec *)
│   └── count: u64
└── within: Option<Within>
    └── millis: u64
```

### 7.1 AST Extension Points (Future Versions)

The following nodes are reserved for future VQL versions and should parse syntactically but produce an "unsupported in VQL v1" error:

- `project_on: Option<Vec<Projection>>` — reserved for the PROJECT ON clause
- `bias: Option<Bias>` — reserved for the BIAS clause
- Subqueries / nested FIND — reserved for future composition

---

## 8. Migration Notes

### 8.1 From VQL v0 (Current Implementation) to This Spec

| Change | Impact |
|--------|--------|
| New `FIND SEMANTIC` keyword | Parser must accept `SEMANTIC` as an alternative to `SIMILARITY` |
| New `OFFSET` clause | `LIMIT n OFFSET m` parsing; `Limit.offset` field in AST |
| New `EN` alias | `EN n ms` parsed identically to `WITHIN n ms` |
| New `PROJECT ON` clause | Parsed but produces "not yet supported" error in VQL v1 |
| New `BIAS` clause | Parsed but produces "not yet supported" error in VQL v1 |
| New `LIKE` predicate | Added to `and_expression` parser combinator |
| New `null` literal | Added to `literal` parser combinator |
| `Semantic` variant in `find` field | `Query.find` can now be `"SEMANTIC"` not just `"SIMILARITY"` |

### 8.2 Future Capabilities (VQL v2+)

Not specified in this grammar, but architecturally预留:

- Recursive subqueries / nested `FIND` inside `IN`
- `INSERT` for data manipulation (currently REST API)
- `OR` combinator in predicate expressions
- Named parameters and prepared statements
- Multi-vector queries (multiple similarity expressions combined)
- `GROUP BY` aggregation over semantic clusters
