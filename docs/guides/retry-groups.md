# Retry Groups

This guide covers **retry groups** — how to make stages automatically retry when they fail, and how to add a verification agent that judges whether the output is correct before moving on.

## The problem retry groups solve

AI agents are non-deterministic. A stage might fail because:

- The generated code doesn't compile
- Tests fail on the first attempt
- The agent misunderstands the prompt and produces the wrong output
- A network timeout occurs mid-execution

Without retry groups, a failure stops the pipeline. You'd need to `--resume` manually, hoping the agent does better next time. Retry groups automate this loop: fail, roll back, try again — with optional verification to catch subtler issues where the stage "succeeds" but the output is wrong.

## Basic retry group

A retry group is a set of consecutive stages that retry together as a unit when any stage in the group fails.

```yaml
name: "Implement with Retries"

retry_groups:
  build:
    max_retries: 3

stages:
  - id: "implement"
    name: "Implement Feature"
    retry_group: "build"
    system_prompt: |
      You are a Rust developer. Write clean, compiling code.
    user_prompt: |
      Add a rate limiter middleware to the API.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "test"
    name: "Build and Test"
    retry_group: "build"
    commands:
      - "cargo build 2>&1"
      - "cargo test 2>&1"
```

### What happens

1. The `implement` stage runs and writes code
2. The `test` stage runs `cargo build` and `cargo test`
3. **If `test` fails** (non-zero exit): the entire `build` group rolls back to `implement`
4. The `implement` stage runs again — this time with `{{verify_feedback}}` containing the error output, so it knows what went wrong
5. This loop repeats up to `max_retries` times (3 in this example)
6. After 3 failed attempts, fujin prompts the user: continue with more retries, or abort?

### Why stages retry together

When a test stage fails, the fix needs to happen in the implementation stage. Rolling back to just the test stage would re-run the same failing tests. The group restarts from its **first** stage so the agent can fix the root cause.

## Adding a verify agent

Sometimes a stage "succeeds" (no error) but the output is wrong — the code compiles but doesn't meet requirements, or tests pass but miss edge cases. A **verify agent** catches these cases.

```yaml
name: "Verified Implementation"

retry_groups:
  quality:
    max_retries: 3
    verify:
      model: "claude-haiku-4-5-20251001"
      system_prompt: |
        You are a code quality verifier. Check whether the implementation
        meets the requirements and the tests actually cover the specified
        behavior. Be strict.
      user_prompt: |
        Requirements: Add pagination to the GET /users endpoint with
        cursor-based pagination, supporting page_size and cursor parameters.

        Check:
        1. Does the endpoint accept page_size and cursor query parameters?
        2. Does it return a next_cursor in the response?
        3. Do tests cover pagination behavior (not just that the endpoint responds)?

        Respond with PASS if all checks pass, or FAIL with a description
        of what's wrong.
      allowed_tools: ["read", "bash"]

stages:
  - id: "implement"
    name: "Implement Pagination"
    retry_group: "quality"
    system_prompt: |
      You are a backend developer. {{verify_feedback}}
    user_prompt: |
      Add cursor-based pagination to GET /users. Support page_size
      (default 20, max 100) and cursor query parameters. Return
      next_cursor in the response body.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "test"
    name: "Run Tests"
    retry_group: "quality"
    commands:
      - "cargo test 2>&1"
```

### The verify flow

1. `implement` runs → `test` runs → both succeed
2. The **verify agent** runs automatically after the last stage in the group
3. The verify agent reads the code and tests, checks the requirements, and responds with `PASS` or `FAIL`
4. **If `PASS`**: the group is done, pipeline continues to the next stage
5. **If `FAIL`**: the verify agent's response is stored as `{{verify_feedback}}` and the group restarts from `implement`

The verify agent is deliberately separate from the implementation agent — it's a cheap, fast model (Haiku) that acts as an independent checker. It can read files and run commands but shouldn't write code itself.

### Verify agent configuration

```yaml
verify:
  model: "claude-haiku-4-5-20251001"    # Fast, cheap model for checking
  system_prompt: "You are a verifier."   # Sets the verifier's persona
  user_prompt: "Check the output."       # What to verify
  allowed_tools: ["read", "bash"]        # Tools the verifier can use
  timeout_secs: 60                       # Optional timeout
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | `"claude-haiku-4-5-20251001"` | Model for the verification agent |
| `system_prompt` | string | *required* | System prompt for the verifier |
| `user_prompt` | string | *required* | What the verifier should check |
| `allowed_tools` | list | `["read", "bash"]` | Tools the verifier can use |
| `timeout_secs` | integer | — | Optional timeout for the verification call |

The verify agent must include `PASS` or `FAIL` in its response. Fujin parses the response looking for these keywords.

## Using `{{verify_feedback}}`

When a group retries (due to a stage failure or a verify `FAIL`), the feedback is injected as `{{verify_feedback}}` into the template variables for all stages in the group. This tells the agent what went wrong.

For **stage failures**, `{{verify_feedback}}` contains the error output:
```
Stage 'test' failed with error:
cargo test exited with status 101
error[E0308]: mismatched types
  --> src/main.rs:42:5
```

For **verify failures**, it contains the verify agent's full response:
```
FAIL

The pagination implementation has issues:
1. The cursor parameter is accepted but ignored — the query always starts from offset 0
2. Tests only check that the endpoint returns 200, not that pagination actually works
3. Missing test for page_size > 100 boundary
```

Use it in your system or user prompt:

```yaml
system_prompt: |
  You are a developer. If there is feedback from a previous attempt,
  address those issues first.
  {{verify_feedback}}
```

On the first run, `{{verify_feedback}}` is empty. On retries, it contains the failure details.

## Single-stage groups with verify

A retry group normally requires at least 2 stages. The exception is when you have a `verify` agent — then a single stage works:

```yaml
retry_groups:
  gen:
    max_retries: 5
    verify:
      model: "claude-haiku-4-5-20251001"
      system_prompt: "You are a code reviewer."
      user_prompt: |
        Check that the generated code compiles and includes error handling
        for all edge cases. Respond with PASS or FAIL.
      allowed_tools: ["read", "bash"]

stages:
  - id: "generate"
    name: "Generate Code"
    retry_group: "gen"
    system_prompt: |
      You are a Rust developer. {{verify_feedback}}
    user_prompt: "Implement the CSV parser."
    allowed_tools: ["read", "write", "bash"]
```

The verify agent acts as the "second stage" — it checks the output and triggers a retry if needed.

## Rules and constraints

**Stages must be consecutive.** All stages in a retry group must be adjacent in the stages list. You can't interleave stages from different groups.

```yaml
# VALID — stages are consecutive
stages:
  - id: a
    retry_group: fix
  - id: b
    retry_group: fix
  - id: c   # different group or no group — fine

# INVALID — gap between group stages
stages:
  - id: a
    retry_group: fix
  - id: b               # no group — breaks the "fix" group
  - id: c
    retry_group: fix     # error: non-consecutive
```

**At least 2 stages (or verify).** A retry group with a single stage and no verify agent doesn't make sense — there's nothing to trigger the retry.

**Command stages and verify feedback.** Command stages in a retry group work for deterministic checks, but they don't have access to `{{verify_feedback}}` (it's a template variable for agent prompts). Fujin warns about this at validation time.

**`max_retries` behavior.** After `max_retries` failed attempts, fujin doesn't immediately abort — it prompts the user (via the TUI or CLI) to either grant another batch of retries or stop the pipeline. This prevents silent infinite loops while still allowing the user to keep trying.

**Checkpoints during retries.** Each retry attempt updates the checkpoint. If you interrupt during a retry loop and `--resume`, the pipeline restarts from the group's first stage at the current retry count.

## Realistic example: Verified API implementation

A pipeline that implements an API feature, verifies it meets a spec, and only proceeds when the verification passes.

```yaml
name: "Verified Feature Pipeline"

variables:
  feature: "Add WebSocket support for real-time notifications"
  spec_file: "docs/SPEC.md"

retry_groups:
  impl:
    max_retries: 4
    verify:
      model: "claude-haiku-4-5-20251001"
      system_prompt: |
        You are a strict QA verifier. Read the spec and the implementation,
        then verify every requirement is met. Check that tests exist for
        each requirement. Be thorough — missing a requirement is worse
        than a false negative.
      user_prompt: |
        Spec: {{spec_file}}
        Read the spec file and the implementation. Verify:
        1. Every endpoint in the spec is implemented
        2. Every error case in the spec is handled
        3. Tests exist for each endpoint and error case
        4. The WebSocket connection lifecycle matches the spec

        Respond with PASS if everything matches, or FAIL with a numbered
        list of what's missing or wrong.
      allowed_tools: ["read", "glob", "grep", "bash"]
      timeout_secs: 120

stages:
  - id: "implement"
    name: "Implement Feature"
    retry_group: "impl"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a backend developer. Read the spec carefully and implement
      exactly what it describes. If there is verification feedback from
      a previous attempt, address every issue listed.
      {{verify_feedback}}
    user_prompt: |
      Implement: {{feature}}
      Spec is at {{spec_file}} — read it first.

      Write implementation code and tests.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "build"
    name: "Build and Test"
    retry_group: "impl"
    commands:
      - "cargo build 2>&1"
      - "cargo test 2>&1"
      - "cargo clippy -- -D warnings 2>&1"

  - id: "docs"
    name: "Write Documentation"
    # Not in the retry group — only runs after impl group passes
    system_prompt: |
      You are a technical writer. Document the WebSocket API.
    user_prompt: |
      Previous stage summary: {{prior_summary}}
      Write API documentation for the WebSocket feature.
    allowed_tools: ["read", "write"]
```

The flow:

1. `implement` writes code based on the spec
2. `build` compiles and runs tests
3. If `build` fails → retry from `implement` with the error as feedback
4. If `build` succeeds → the **verify agent** checks the implementation against the spec
5. If verify says `FAIL` → retry from `implement` with the verify feedback listing what's missing
6. If verify says `PASS` → the `docs` stage runs (outside the retry group)

This pattern ensures the implementation matches the spec before the pipeline moves on to documentation.

## Realistic example: Self-correcting migration

A migration pipeline where each file migration is verified before proceeding.

```yaml
name: "Verified Migration"

variables:
  migration: "Convert all class components to functional components with hooks"

retry_groups:
  migrate:
    max_retries: 3
    verify:
      model: "claude-haiku-4-5-20251001"
      system_prompt: |
        You are a React migration verifier. Check that:
        1. No class component syntax remains in migrated files
        2. All lifecycle methods are converted to useEffect hooks
        3. State is managed with useState
        4. The component's public API (props, exports) is unchanged
      user_prompt: |
        Verify the migration is complete and correct.
        Search for any remaining class components.
        Run the test suite to check for regressions.
        Respond with PASS or FAIL.
      allowed_tools: ["read", "grep", "bash"]

stages:
  - id: "migrate"
    name: "Apply Migration"
    retry_group: "migrate"
    system_prompt: |
      You are a React developer performing a codebase migration.
      {{verify_feedback}}
    user_prompt: |
      Migration: {{migration}}
      Apply the migration to all files. Run tests after each batch of changes.
    allowed_tools: ["read", "edit", "bash", "glob", "grep"]

  - id: "test"
    name: "Run Full Suite"
    retry_group: "migrate"
    commands:
      - "npm test 2>&1"
      - "npm run lint 2>&1"

  - id: "summary"
    name: "Write Migration Report"
    system_prompt: "You are a technical writer."
    user_prompt: |
      Summarize the migration that was performed. List files changed
      and any notable decisions. Write to docs/migration-report.md.
    allowed_tools: ["read", "write", "glob"]
```

## What to read next

- **[Multi-Stage Pipelines](multi-stage-pipelines.md)** — Context passing and stage composition
- **[Pipeline Patterns](pipeline-patterns.md)** — More complete pipeline examples
- **[Pipeline Authoring Reference](../pipeline-authoring.md)** — Full field reference and validation rules
