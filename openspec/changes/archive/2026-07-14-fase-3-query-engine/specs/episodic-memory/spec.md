# Episodic Memory Specification

## Purpose

Episodic memory stores per-user footprint vectors that bias search results toward user preferences, updated via implicit feedback from query interactions.

## Requirements

### Requirement: Per-user footprint vector

The episodic memory system MUST store a per-user footprint vector of approximately 1KB. The footprint MUST be persisted in HotStore using the existing persistence mechanism.

#### Scenario: Footprint stored per user

- GIVEN a user identifier `"user-abc123"` and a vector of 768 f32 values (~3KB)
- WHEN the system records an interaction for that user
- THEN the footprint is stored in HotStore keyed by user ID
- AND the footprint survives restarts via HotStore persistence

### Requirement: Footprint combines with query vector

On query, the footprint MUST be combined with the query vector via element-wise multiplication: `effective_query = normalize(query_vector × footprint_vector)`.

#### Scenario: Query biased by footprint

- GIVEN a user with an existing footprint vector
- WHEN a query is executed for that user
- THEN the executor loads the footprint from HotStore
- AND the query vector is multiplied element-wise with the footprint
- AND the result is normalized before HNSW search

### Requirement: Implicit feedback update

The footprint MUST update via implicit feedback when a user clicks on a result. The update uses confidence decay: `α = min(1.0, interaction_count / 6.0)`, `new_footprint = (1 - α) × old + α × click_vector`.

#### Scenario: Click updates footprint

- GIVEN a user with `interaction_count = 2` and an existing footprint
- WHEN the user clicks on a result with vector V
- THEN the new footprint is `(1 - 2/6) × old + (2/6) × V`
- AND the interaction count increments to 3

#### Scenario: Initial click creates footprint

- GIVEN a new user with no existing footprint
- WHEN the user clicks on a result
- THEN a footprint is initialized from the clicked result's vector
- AND the interaction count is set to 1

### Requirement: Convergence after interactions

The footprint SHOULD converge after approximately 5-6 interactions, meaning additional clicks have diminishing influence.

#### Scenario: Footprint stabilizes

- GIVEN a user with `interaction_count = 6`
- WHEN the user clicks on a new result
- THEN `α = 1.0` and the new footprint fully reflects the latest click
- AND the interaction count saturates at 6

### Requirement: Scoring function

Episodic memory MUST provide a scoring function `relevance(user_id, result) → f32` that measures the relevance of a result to a user's footprint.

#### Scenario: Relevance scored against footprint

- GIVEN a user with footprint F and a candidate result with vector V
- WHEN `relevance(user_id, result_vector)` is called
- THEN the score is computed as cosine similarity between F and V
- AND higher similarity indicates stronger relevance to the user's preferences
