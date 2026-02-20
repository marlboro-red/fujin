# Branching and Conditional Execution

This guide covers how to make stages conditional — skipping them based on prior output, or routing to different execution paths. You'll learn when to use `when` vs `branch`/`on_branch`, and how to design pipelines with convergence points.

## The linear model with skips

Fujin pipelines always iterate stages top-to-bottom. Conditional execution doesn't change this — it just marks certain stages as "skipped" when their conditions aren't met. Skipped stages produce no output and make no changes. The pipeline continues to the next stage.

This means you can reason about your pipeline as a flat list. The conditions just control which stages actually execute.

## `when` — Simple regex gating

The simplest form of conditional execution. A `when` condition checks a prior stage's response text against a regex pattern. If the pattern doesn't match, the stage is skipped.

### Use case: Conditional test writing

You have a pipeline that analyzes a codebase and only writes new tests if coverage is insufficient:

```yaml
name: "Smart Test Writer"

stages:
  - id: "analyze"
    name: "Analyze Test Coverage"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a test coverage analyst. Examine the project's test
      suite and source code to assess coverage quality.
    user_prompt: |
      Analyze this project's test coverage:
      1. Count the test files and test functions
      2. Identify source modules with no corresponding tests
      3. Check for edge case coverage in existing tests

      End your response with exactly one verdict line:
      VERDICT: NEEDS_TESTS — if significant gaps exist
      VERDICT: ADEQUATE — if coverage is reasonable
    allowed_tools: ["read", "glob", "grep"]
  - id: "write-tests"
    name: "Write Missing Tests"
    when:
      stage: "analyze"
      output_matches: "NEEDS_TESTS"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a test engineer. Write focused, high-value tests
      for the modules identified as uncovered.
    user_prompt: |
      Coverage analysis: {{stages.analyze.summary}}

      Write tests for the uncovered modules. Focus on:
      - Public API contracts
      - Error paths and edge cases
      - Integration between modules

      Run the tests to verify they pass.
    allowed_tools: ["read", "write", "bash"]
  - id: "lint"
    name: "Lint and Format"
    commands:
      - "cargo fmt --check 2>&1"
      - "cargo clippy -- -D warnings 2>&1"
```

### How it works

1. The `analyze` stage runs and produces a response that ends with `VERDICT: NEEDS_TESTS` or `VERDICT: ADEQUATE`
2. Before running `write-tests`, fujin checks: does the `analyze` stage's response match the regex `NEEDS_TESTS` (case-insensitive)?
3. If yes, `write-tests` runs normally. If no, it's skipped.
4. The `lint` stage has no `when`, so it always runs.

### Tips for `when`

**Use structured verdicts.** Ask the agent to output a specific keyword or structured suffix. Vague patterns like `"should write tests"` are fragile — the agent might phrase it differently each time. Explicit verdicts like `VERDICT: NEEDS_TESTS` are reliable.

**The regex is case-insensitive.** `output_matches: "NEEDS_TESTS"` matches `NEEDS_TESTS`, `needs_tests`, and `Needs_Tests`.

**The regex searches the full response.** It doesn't need to match the entire response — just appear somewhere in it. This means a verdict at the end of a long analysis will still be found.

**`when.stage` must reference an earlier stage.** Fujin validates this at config load time.

### Use case: Security gate

Run a security audit first, and only proceed with deployment if no critical issues are found:

```yaml
stages:
  - id: "security-scan"
    name: "Security Audit"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a security auditor. Review the code for vulnerabilities
      including injection flaws, authentication issues, data exposure,
      and dependency vulnerabilities.
    user_prompt: |
      Audit this project for security issues. Classify severity as
      CRITICAL, HIGH, MEDIUM, or LOW.

      End with:
      SECURITY_STATUS: PASS — if no CRITICAL or HIGH issues
      SECURITY_STATUS: FAIL — if any CRITICAL or HIGH issues exist
    allowed_tools: ["read", "glob", "grep", "bash"]
  - id: "fix-security"
    name: "Fix Security Issues"
    when:
      stage: "security-scan"
      output_matches: "SECURITY_STATUS: FAIL"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a security engineer. Fix the vulnerabilities identified
      in the security audit. Prioritize CRITICAL issues first.
    user_prompt: |
      Security audit findings: {{stages.security-scan.summary}}
      Fix all CRITICAL and HIGH severity issues.
    allowed_tools: ["read", "edit", "bash"]
  - id: "deploy-prep"
    name: "Prepare Deployment"
    when:
      stage: "security-scan"
      output_matches: "SECURITY_STATUS: PASS"
    commands:
      - "cargo build --release"
      - "docker build -t myapp:latest ."
```

Note how `fix-security` and `deploy-prep` are mutually exclusive — one triggers on `FAIL`, the other on `PASS`. The pipeline takes different paths depending on the audit result.

## `branch`/`on_branch` — AI-driven routing

For more sophisticated routing where you want an AI to classify the situation and pick from multiple paths, use `branch`.

### How it works

1. A stage with `branch` runs normally (producing its output)
2. After the stage completes, fujin makes a separate AI call with the branch classifier
3. The classifier reads the stage output and the `branch.prompt`, then selects one of the `routes`
4. Downstream stages with `on_branch` matching the selected route will execute; others are skipped
5. Stages without `on_branch` always execute (convergence points)

### Use case: Language-specific pipeline

A pipeline that detects the project's language and routes to a language-appropriate implementation path:

```yaml
name: "Polyglot Feature Builder"

variables:
  feature: "Add a rate limiter middleware that limits requests to 100/minute per IP"

stages:
  - id: "detect"
    name: "Detect Stack"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a project analyst. Examine the repository structure,
      configuration files, and source code to determine the
      technology stack.
    user_prompt: |
      Analyze this project and determine:
      - Primary programming language
      - Web framework
      - Project structure conventions
      - Testing framework
    allowed_tools: ["read", "glob", "grep"]
    branch:
      model: "claude-haiku-4-5-20251001"
      prompt: |
        Based on the project analysis, what is the primary tech stack?
        Choose the route that best matches.
      routes: [rust-actix, node-express, python-flask, go-chi]
      default: node-express
  - id: "rust-impl"
    name: "Rust Implementation"
    on_branch: "rust-actix"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert Rust developer. Follow the project's existing
      patterns for middleware and error handling.
    user_prompt: |
      Stack analysis: {{stages.detect.summary}}
      Implement: {{feature}}

      Use actix-web middleware patterns. Store rate limit state
      in a DashMap or similar concurrent structure.
    allowed_tools: ["read", "write", "edit", "bash"]
  - id: "node-impl"
    name: "Node.js Implementation"
    on_branch: "node-express"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert Node.js developer. Follow the project's
      existing patterns for Express middleware.
    user_prompt: |
      Stack analysis: {{stages.detect.summary}}
      Implement: {{feature}}

      Use Express middleware patterns with in-memory rate tracking.
    allowed_tools: ["read", "write", "edit", "bash"]
  - id: "python-impl"
    name: "Python Implementation"
    on_branch: "python-flask"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert Python developer. Follow the project's
      existing patterns for Flask middleware and decorators.
    user_prompt: |
      Stack analysis: {{stages.detect.summary}}
      Implement: {{feature}}

      Use Flask before_request hooks with Redis or in-memory storage.
    allowed_tools: ["read", "write", "edit", "bash"]
  - id: "go-impl"
    name: "Go Implementation"
    on_branch: "go-chi"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert Go developer. Follow the project's existing
      patterns for chi middleware.
    user_prompt: |
      Stack analysis: {{stages.detect.summary}}
      Implement: {{feature}}

      Use chi middleware patterns with sync.Map for rate state.
    allowed_tools: ["read", "write", "edit", "bash"]
  - id: "test"
    name: "Run Tests"
    # No on_branch — always runs (convergence point)
    system_prompt: |
      You are a test engineer. Write and run tests for the rate
      limiter that was just implemented.
    user_prompt: |
      Implementation summary: {{prior_summary}}
      Files changed: {{artifact_list}}

      Write tests covering:
      - Requests under the limit succeed
      - Requests over the limit are rejected with 429
      - Rate limits reset after the window expires
      - Different IPs have independent limits

      Run the tests.
    allowed_tools: ["read", "write", "bash"]
```

### The branch classifier

The `branch` object configures the classifier call:

```yaml
branch:
  model: "claude-haiku-4-5-20251001"   # Optional — defaults to summarizer model
  prompt: "What type of work is needed?" # Sent alongside the stage output
  routes: [frontend, backend, fullstack] # Valid route names
  default: fullstack                     # Fallback if classifier is ambiguous
```

- **`model`**: Use Haiku for classification — it's fast and cheap, and route selection is a simple task.
- **`prompt`**: Be specific. "Classify the primary work needed" is better than "What should we do?"
- **`routes`**: Short, descriptive names. They're matched against the classifier's response.
- **`default`**: Important safety net. If the classifier produces an unexpected response, this route is selected.

### `on_branch` — matching routes

`on_branch` accepts a single string or a list:

```yaml
# Runs only if "frontend" was selected
on_branch: "frontend"

# Runs if either "frontend" or "fullstack" was selected
on_branch: ["frontend", "fullstack"]
```

List form gives you OR semantics — the stage runs if any of the listed routes match. This is useful for stages that apply to multiple paths.

### Convergence points

Stages without `on_branch` always run, regardless of which route was selected. Place them after your branched sections to converge the pipeline:

```yaml
  # ... branched implementation stages above ...
  - id: "test"
    name: "Integration Tests"
    # No on_branch = convergence point
    commands:
      - "npm test"
  - id: "review"
    name: "Code Review"
    # Also a convergence point
    system_prompt: "You are a code reviewer."
    user_prompt: "Review all changes: {{artifact_list}}"
    allowed_tools: ["read"]
```

### Use case: PR triage pipeline

A pipeline that classifies incoming PRs and routes to appropriate review workflows:

```yaml
name: "PR Review Router"

variables:
  pr_description: "Add WebSocket support for real-time notifications"

stages:
  - id: "triage"
    name: "Triage PR"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior engineer triaging a pull request. Analyze the
      code changes to understand scope and risk.
    user_prompt: |
      PR description: {{pr_description}}

      Read the changed files and analyze:
      - What areas of the codebase are affected
      - Whether there are database changes
      - Whether there are API surface changes
      - Risk level (does it touch authentication, payments, etc.)
    allowed_tools: ["read", "glob", "grep"]
    branch:
      prompt: |
        Based on the PR analysis, what level of review is appropriate?
        - quick-review: Small, low-risk changes (typos, docs, config)
        - standard-review: Normal feature work, no high-risk areas
        - deep-review: Touches security, data, or core infrastructure
      routes: [quick-review, standard-review, deep-review]
      default: standard-review
  - id: "quick"
    name: "Quick Review"
    on_branch: "quick-review"
    model: "claude-sonnet-4-6"
    system_prompt: "You are a code reviewer doing a quick pass."
    user_prompt: |
      Triage: {{stages.triage.summary}}
      Check for obvious issues: typos, formatting, broken links.
    allowed_tools: ["read"]
  - id: "standard"
    name: "Standard Review"
    on_branch: "standard-review"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior code reviewer. Check for correctness,
      test coverage, error handling, and code quality.
    user_prompt: |
      Triage: {{stages.triage.summary}}
      Do a thorough code review. Check test coverage.
    allowed_tools: ["read", "glob", "grep"]
  - id: "deep"
    name: "Deep Security Review"
    on_branch: "deep-review"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a security-focused reviewer. This PR touches sensitive
      areas. Check for injection, auth bypass, data leaks, and
      race conditions.
    user_prompt: |
      Triage: {{stages.triage.summary}}
      Do a deep security review of all changes.
    allowed_tools: ["read", "glob", "grep", "bash"]

  - id: "summary"
    name: "Write Review Summary"
    # Convergence — always runs
    system_prompt: "You are a technical writer."
    user_prompt: |
      Review findings: {{prior_summary}}
      Write a concise review summary in REVIEW.md.
    allowed_tools: ["read", "write"]
```

## Combining `when` and `branch`

You can use both mechanisms in the same pipeline. They serve different purposes:

| Mechanism | Best for | Cost |
|-----------|----------|------|
| `when` | Binary yes/no decisions based on structured output | Free (regex check) |
| `branch`/`on_branch` | Multi-way routing with nuanced classification | One extra AI call |

Example — a pipeline that first checks if any work is needed (`when`), then routes the work to the appropriate path (`branch`):

```yaml
stages:
  - id: "check"
    name: "Check for Issues"
    system_prompt: "You are a code quality analyzer."
    user_prompt: |
      Scan the codebase for issues: bugs, security problems,
      performance bottlenecks, code smells.

      If you find issues, end with: STATUS: ISSUES_FOUND
      If the code looks good, end with: STATUS: ALL_CLEAR
    allowed_tools: ["read", "glob", "grep"]
  - id: "classify"
    name: "Classify Issues"
    when:
      stage: "check"
      output_matches: "ISSUES_FOUND"
    system_prompt: "You are a code analyst."
    user_prompt: |
      Analysis: {{stages.check.summary}}
      Categorize the issues found and prioritize them.
    allowed_tools: ["read"]
    branch:
      prompt: "What category of fix is primarily needed?"
      routes: [security-fix, performance-fix, refactor]
      default: refactor
  - id: "security"
    name: "Fix Security Issues"
    on_branch: "security-fix"
    system_prompt: "You are a security engineer."
    user_prompt: "Fix the security issues: {{stages.check.summary}}"
    allowed_tools: ["read", "edit", "bash"]
  - id: "performance"
    name: "Fix Performance"
    on_branch: "performance-fix"
    system_prompt: "You are a performance engineer."
    user_prompt: "Fix the performance issues: {{stages.check.summary}}"
    allowed_tools: ["read", "edit", "bash"]
  - id: "refactor"
    name: "Refactor Code"
    on_branch: "refactor"
    system_prompt: "You are a senior developer."
    user_prompt: "Refactor the code smells: {{stages.check.summary}}"
    allowed_tools: ["read", "edit", "bash"]
  - id: "verify"
    name: "Run Tests"
    # Convergence point — always runs if we got here
    commands:
      - "cargo test 2>&1"
```

If the `check` stage finds no issues (`ALL_CLEAR`), the `classify` stage is skipped. Since `classify` is skipped, it never runs its `branch` classifier, so no route is selected. The `on_branch` stages are all skipped too. The `verify` stage (no `on_branch`) still runs.

## Validation rules for conditions

Fujin validates your conditions at config load time:

**Errors:**
- `when.stage` must reference a stage ID that appears earlier in the list
- `when.output_matches` must be a valid regex
- `branch.routes` must be non-empty
- `branch.default` (if set) must be one of the defined routes
- `on_branch` values must match a route from an earlier `branch`
- A stage cannot have both `branch` and `on_branch`

**Warnings:**
- A `branch` route has no downstream `on_branch` stage referencing it (orphaned route)

## What to read next

- **[Exports and Dynamic Variables](exports-and-dynamic-variables.md)** — Let agents discover project properties and pass them to downstream stages
- **[Pipeline Patterns](pipeline-patterns.md)** — Complete examples combining branching with other features
