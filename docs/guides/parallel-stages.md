# Parallel Stages with `dependencies`

This guide covers how to declare stage dependencies explicitly so that independent stages can run in parallel, reducing total pipeline execution time.

## Overview

By default, fujin pipelines execute stages sequentially from top to bottom — each stage implicitly depends on the one before it. The `dependencies` field lets you override this behavior, declaring exactly which stages a given stage needs to wait for. When two stages share no dependency relationship, the DAG scheduler can run them concurrently.

## Quick example

```yaml
name: "Full-Stack Builder"
stages:
  - id: analyze
    name: Analyze Requirements
    system_prompt: "You are a senior architect."
    user_prompt: "Read SPEC.md and create an implementation plan."
    allowed_tools: ["read", "write"]

  - id: frontend
    name: Build Frontend
    dependencies: [analyze]
    system_prompt: "You are a frontend developer."
    user_prompt: |
      Plan: {{stages.analyze.summary}}
      Build the React components.
    allowed_tools: ["read", "write", "bash"]

  - id: backend
    name: Build Backend
    dependencies: [analyze]
    system_prompt: "You are a backend developer."
    user_prompt: |
      Plan: {{stages.analyze.summary}}
      Build the API endpoints.
    allowed_tools: ["read", "write", "bash"]

  - id: integrate
    name: Integration Tests
    dependencies: [frontend, backend]
    commands:
      - "npm test 2>&1"
```

This creates a **diamond** dependency graph:

```
    analyze
    /     \
frontend  backend
    \     /
   integrate
```

`frontend` and `backend` both depend on `analyze`, but not on each other — they can run in parallel. `integrate` waits for both to finish.

## How `dependencies` works

### The three cases

| Config | Behavior |
|--------|----------|
| `dependencies` absent (default) | Stage implicitly depends on the previous stage in YAML order. This preserves the original sequential behavior. |
| `dependencies: [stage_a, stage_b]` | Stage waits for all listed stages to complete before starting. |
| `dependencies: []` | Stage has **no** dependencies and can start immediately (in parallel with the first stage). |

### Backward compatibility

Pipelines without any `dependencies` fields work exactly as before. The implicit rule — each stage depends on the one above it — produces a linear dependency chain that the runner executes sequentially. No changes are needed to existing configs.

### DAG construction

When the pipeline starts, fujin builds a Directed Acyclic Graph (DAG) from the stage list. If all dependencies form a linear chain (every stage has at most one parent and one child), the runner uses the original sequential execution path. Otherwise, it switches to the DAG scheduler that can launch independent stages concurrently.

## Dependency topologies

### Linear (default)

```yaml
stages:
  - id: design
    ...
  - id: implement
    ...
  - id: test
    ...
```

```
design → implement → test
```

No `dependencies` needed. Each stage implicitly depends on the previous one.

### Fan-out

Multiple stages depend on the same parent, creating parallel branches:

```yaml
stages:
  - id: analyze
    ...
  - id: frontend
    dependencies: [analyze]
    ...
  - id: backend
    dependencies: [analyze]
    ...
  - id: docs
    dependencies: [analyze]
    ...
```

```
         analyze
        /   |   \
frontend backend docs
```

All three stages start as soon as `analyze` completes.

### Fan-in

A stage waits for multiple upstream stages to converge:

```yaml
stages:
  - id: a
    dependencies: []
    ...
  - id: b
    dependencies: []
    ...
  - id: merge
    dependencies: [a, b]
    ...
```

```
a   b
 \ /
merge
```

`a` and `b` start immediately (no dependencies). `merge` waits for both to finish.

### Diamond

The most common parallel pattern — fan-out followed by fan-in:

```yaml
stages:
  - id: plan
    ...
  - id: frontend
    dependencies: [plan]
    ...
  - id: backend
    dependencies: [plan]
    ...
  - id: review
    dependencies: [frontend, backend]
    ...
```

```
    plan
   /    \
frontend backend
   \    /
   review
```

### Fully parallel (independent stages)

Use `dependencies: []` to declare stages with no dependencies at all:

```yaml
stages:
  - id: lint
    dependencies: []
    commands: ["cargo clippy 2>&1"]

  - id: format-check
    dependencies: []
    commands: ["cargo fmt --check 2>&1"]

  - id: test
    dependencies: []
    commands: ["cargo test 2>&1"]

  - id: report
    dependencies: [lint, format-check, test]
    system_prompt: "You are a CI reporter."
    user_prompt: |
      Summarize the results from all checks.
    allowed_tools: ["read"]
```

```
lint  format-check  test
  \       |        /
       report
```

All three check stages start immediately and run concurrently.

## Context passing with DAG dependencies

In a sequential pipeline, `{{prior_summary}}` always refers to the immediately preceding stage. In a DAG pipeline, context passing is based on **direct parents** (the stages listed in `dependencies`):

### What changes

| Variable | Sequential behavior | DAG behavior |
|----------|-------------------|--------------|
| `{{prior_summary}}` | Summary of the previous stage | Combined summaries of all direct parent stages, joined with `---` separators |
| `{{artifact_list}}` | Files changed by the previous stage | Union of files changed by all direct parent stages |
| `{{all_artifacts}}` | Content of files from the previous stage | Content of files from all direct parent stages |
| `{{stages.<id>.*}}` | Same | Same — always references a specific stage by ID |

### Prefer named stage references

In DAG pipelines, `{{prior_summary}}` combines output from multiple parents which can be verbose or ambiguous. Prefer explicit references:

```yaml
# Instead of relying on {{prior_summary}} which merges multiple parents:
user_prompt: |
  Frontend work: {{stages.frontend.summary}}
  Backend work: {{stages.backend.summary}}

  Run integration tests against both components.
```

### Single-parent stages

When a stage has exactly one dependency, the behavior is identical to the sequential model — `{{prior_summary}}` contains just that one parent's summary.

## Validation rules

Fujin validates `dependencies` before execution:

**Errors (prevent execution):**
- `dependencies` references an undefined stage ID
- `dependencies` contains a self-reference
- Circular dependencies detected (e.g., A depends on B, B depends on A)
- `when.stage` references a stage that is not a dependency (direct or transitive) of the current stage
- `on_branch` references a branch defined by a stage that is not a dependency (direct or transitive)
- Non-first stages in a retry group have `dependencies` pointing outside the group

**Allowed:**
- `dependencies: []` (empty list) — stage has no dependencies
- Mixing `dependencies` with implicit dependencies — stages without `dependencies` still implicitly depend on the previous stage
- A stage with `dependencies` can reference any stage, not just earlier ones in YAML order (as long as there are no cycles)

## Interaction with other features

### Retry groups

Stages within the same retry group are always executed sequentially within that group, even in a DAG pipeline. The group as a whole can run in parallel with other stages, but internal ordering is preserved.

```yaml
retry_groups:
  build:
    max_retries: 3
    verify:
      system_prompt: "Check the build."
      user_prompt: "Run tests and verify. PASS or FAIL."

stages:
  - id: analyze
    ...

  - id: implement
    dependencies: [analyze]
    retry_group: build
    ...

  - id: test
    dependencies: [implement]
    retry_group: build
    commands: ["cargo test 2>&1"]

  - id: docs
    dependencies: [analyze]
    ...
```

Here `implement` and `test` form a retry group that runs sequentially. Meanwhile, `docs` can run in parallel since it only depends on `analyze`.

Non-first stages in a retry group cannot have `dependencies` pointing outside the group — the retry mechanism needs to re-run the group as a contiguous unit.

### Branching (`branch`/`on_branch`)

Branching works with DAG dependencies. If a stage uses `on_branch`, the stage that defines the matching branch must be a transitive dependency:

```yaml
stages:
  - id: analyze
    branch:
      prompt: "Classify the work"
      routes: [frontend, backend]
    ...

  - id: frontend-impl
    dependencies: [analyze]
    on_branch: frontend
    ...

  - id: backend-impl
    dependencies: [analyze]
    on_branch: backend
    ...
```

### Conditional execution (`when`)

The `when.stage` must be a direct or transitive dependency of the stage using the `when` condition:

```yaml
stages:
  - id: check
    ...

  - id: fix
    dependencies: [check]
    when:
      stage: check
      output_matches: "FAIL"
    ...
```

### Checkpoints and resume

The checkpoint system tracks both completed and skipped stages. When resuming a DAG pipeline, the scheduler reconstructs which stages are satisfied and picks up where it left off, correctly handling both completed and skipped stages from the prior run.

## Realistic example: Full-stack feature pipeline

```yaml
name: "Full-Stack Feature"

variables:
  feature: "Add real-time notifications with WebSocket support"

summarizer:
  max_tokens: 2048

stages:
  - id: plan
    name: Plan Architecture
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior architect. Design the feature across both
      frontend and backend. Write the plan to docs/plan.md.
    user_prompt: |
      Feature: {{feature}}
      Read the existing codebase, then create an implementation plan.
    allowed_tools: ["read", "write", "glob", "grep"]

  - id: backend
    name: Implement Backend
    dependencies: [plan]
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a backend developer. Implement the server-side
      components described in docs/plan.md.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Implement the backend: API endpoints, WebSocket handlers,
      database models. Run tests when done.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: frontend
    name: Implement Frontend
    dependencies: [plan]
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a frontend developer. Implement the client-side
      components described in docs/plan.md.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Implement the frontend: notification components, WebSocket
      client, UI state management. Run tests when done.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: integration
    name: Integration Tests
    dependencies: [frontend, backend]
    commands:
      - "npm run test:integration 2>&1"

  - id: review
    name: Code Review
    dependencies: [integration]
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior engineer reviewing the full feature
      implementation. Check for security issues, race conditions
      in WebSocket handling, and consistency between frontend
      and backend.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Backend: {{stages.backend.summary}}
      Frontend: {{stages.frontend.summary}}
      Integration test results: {{stages.integration.response}}

      Review all changes and fix any issues.
    allowed_tools: ["read", "edit", "bash"]
```

Execution flow:
1. `plan` runs first
2. `backend` and `frontend` run in parallel (both depend only on `plan`)
3. `integration` waits for both to finish
4. `review` runs last

## What to read next

- **[Multi-Stage Pipelines](multi-stage-pipelines.md)** — Context passing, model selection, and stage design
- **[Pipeline Patterns](pipeline-patterns.md)** — Copy-pasteable recipes including parallel patterns
- **[Branching and Conditions](branching-and-conditions.md)** — Combine parallel execution with conditional routing
- **[Pipeline Authoring Reference](../pipeline-authoring.md)** — Complete field reference
