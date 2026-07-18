# VQL Grammar Specification

## Purpose

Define the VQL (Vector Query Language) grammar as a nom-based parser that produces a typed AST. This spec covers syntax only — no query planning or execution.

## Requirements

### Requirement: `SIMILARITY(embedding, 'text')` expressions

The parser MUST accept `SIMILARITY(embedding, 'text')` as a valid clause where `embedding` is an identifier and `text` is a string literal enclosed in single quotes.

#### Scenario: Valid SIMILARITY clause

- GIVEN the query `SIMILARITY(emb, 'hello world')`
- WHEN the parser processes it
- THEN it produces an AST node with embedding reference `emb` and text `"hello world"`

#### Scenario: Missing closing parenthesis

- GIVEN the query `SIMILARITY(emb, 'hello'`
- WHEN the parser processes it
- THEN it returns an error indicating a missing closing parenthesis

### Requirement: `WITH METADATA WHERE` with comparison operators

The parser MUST support `WITH METADATA WHERE` clauses with operators `=`, `!=`, `<`, `>`, `<=`, `>=`, `IN`, and `BETWEEN`. Multiple conditions MUST be combinable with `AND`.

#### Scenario: Single equality filter

- GIVEN the query `... WITH METADATA WHERE color = 'red'`
- WHEN the parser processes it
- THEN it produces a metadata condition AST node for `color = 'red'`

#### Scenario: IN and BETWEEN combined

- GIVEN the query `... WITH METADATA WHERE price BETWEEN 10 AND 50 AND category IN ('a', 'b')`
- WHEN the parser processes it
- THEN it produces AST nodes for both conditions combined by AND

#### Scenario: Malformed operator rejected

- GIVEN the query `... WITH METADATA WHERE color =< 5`
- WHEN the parser processes it
- THEN it returns a descriptive error at the malformed operator

### Requirement: `WITHIN` latency budget clause

The parser MUST support `WITHIN <number>ms` to specify a query latency budget in milliseconds.

#### Scenario: Valid WITHIN clause

- GIVEN the query `... WITHIN 100ms`
- WHEN the parser processes it
- THEN it produces an AST node with a latency budget of 100ms

#### Scenario: Missing unit suffix

- GIVEN the query `... WITHIN 100`
- WHEN the parser processes it
- THEN it returns an error indicating the missing `ms` suffix

### Requirement: `ORDER BY` with scoring function

The parser MUST support `ORDER BY <scoring_function>(<args>)` to rank results by a scoring expression.

#### Scenario: ORDER BY with default direction

- GIVEN the query `... ORDER BY similarity(emb, 'query')`
- WHEN the parser processes it
- THEN it produces an AST node for the scoring function `similarity` with arguments `emb` and `'query'`

### Requirement: `LIMIT <number>`

The parser MUST support `LIMIT <number>` to cap the number of returned results.

#### Scenario: Valid LIMIT clause

- GIVEN the query `... LIMIT 10`
- WHEN the parser processes it
- THEN it produces an AST node with limit value 10

### Requirement: Typed AST nodes

All AST node types MUST be enumerations or structs defined in a dedicated `ast` module. Each node MUST carry typed fields specific to its clause.

#### Scenario: AST node carries typed fields

- GIVEN a parsed `SIMILARITY` clause
- WHEN inspecting the AST node
- THEN its embedding field is a `String` and its text field is a `String`

### Requirement: Span-level error locations

The parser SHOULD use `nom_locate` to annotate AST nodes and errors with source position (line and column).

#### Scenario: Error includes line and column

- GIVEN a malformed query
- WHEN the parser returns an error
- THEN the error includes the line number and column where parsing failed

### Requirement: Descriptive error messages

The parser MUST reject malformed queries with a human-readable error message describing what was expected versus what was found.

#### Scenario: Unexpected token

- GIVEN the query `SIMILARITY emb 'text'` (missing parentheses)
- WHEN the parser processes it
- THEN it returns an error message like "expected '(' after SIMILARITY, found 'emb'"

### Requirement: Grammar implemented with nom combinators

All grammar rules MUST be implemented using nom combinators (`tag`, `delimited`, `separated_list0`, etc.). No hand-written recursive descent.

#### Scenario: Nom combinator tree

- GIVEN a valid query
- WHEN the parser processes it
- THEN it resolves via nominal combinator composition without panicking

### Requirement: AST types implement Debug, Clone, PartialEq

Every AST type MUST derive or implement `Debug`, `Clone`, and `PartialEq`.

#### Scenario: Derive macros present

- GIVEN an AST node struct
- WHEN inspecting its source
- THEN it carries `#[derive(Debug, Clone, PartialEq)]` or equivalent manual implementations
