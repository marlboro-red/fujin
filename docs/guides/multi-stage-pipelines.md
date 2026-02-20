# Multi-Stage Pipelines

This guide covers how to design effective multi-stage pipelines — choosing what goes in each stage, how context flows between them, and how to pick the right model and tools for each step.

## Why multiple stages?

A single-stage pipeline works for simple tasks, but most real work benefits from decomposition:

- **Separation of concerns.** An architect thinks differently than a coder, who thinks differently than a reviewer. Giving each role its own stage with a tailored system prompt produces better results.
- **Focused tool access.** A design stage doesn't need shell access. A review stage shouldn't be able to write files. Restricting tools per stage prevents unintended side effects.
- **Checkpointing.** If stage 3 fails, you resume from stage 3 instead of starting over. With a single monolithic stage, any failure means a full restart.
- **Cost control.** Use Opus for the hard parts (architecture, complex reasoning) and Sonnet or Haiku for the mechanical parts (formatting, documentation, simple edits).

## Designing your stage sequence

Think of stages as a team of specialists working in sequence. A good rule of thumb:

1. **Plan/Design** — Understand the problem, create a design doc or spec
2. **Implement** — Write the actual code based on the plan
3. **Validate** — Run tests, linting, type checking
4. **Review/Refine** — Have a senior reviewer examine the output
5. **Document** — Write user-facing docs

You don't need all five. Pick the stages that match your task. A two-stage pipeline (implement → test) is perfectly fine for straightforward work.

## Context passing in detail

When stage N finishes, fujin builds context for stage N+1 through three channels:

### 1. Summarized output — `{{prior_summary}}`

The full text output from the previous stage gets summarized by the configured summarizer model (Haiku by default). The summary focuses on what was accomplished, what files were changed, and any key decisions.

```yaml
user_prompt: |
  Previous stage summary: {{prior_summary}}
  Now implement the architecture described above.
```

**When to use it:** Most of the time. It's concise and gives the next stage enough context to continue without being overwhelmed by raw output.

**When to skip it:** When you need the exact wording or full output — use `{{stages.<id>.response}}` instead.

**Auto-injection:** Even if you don't include `{{prior_summary}}` in your prompt template, the agent runtime automatically prepends a "Context from previous stage" section when prior output exists. Using `{{prior_summary}}` explicitly gives you control over _where_ it appears — the runtime detects it's already present and skips the automatic prepend.

### 2. File artifacts — `{{artifact_list}}` and `{{all_artifacts}}`

Fujin tracks which files each stage creates, modifies, or deletes using `git status`.

- `{{artifact_list}}` — A newline-separated list of file paths changed by the previous stage
- `{{all_artifacts}}` — The full content of each changed file, formatted as blocks:

```
=== src/main.rs ===
fn main() {
    println!("Hello");
}

=== Cargo.toml ===
[package]
name = "my-project"
...
```

```yaml
user_prompt: |
  Previous stage summary: {{prior_summary}}

  The following files were created or modified:
  {{artifact_list}}

  Review them for correctness and style issues.
```

**When to use `{{artifact_list}}`:** When the agent just needs to know _what_ files exist so it can read them itself.

**When to use `{{all_artifacts}}`:** When you want the file contents directly in the prompt — useful for review stages that should see everything at once without making tool calls. Be mindful of prompt length with large codebases.

### 3. Shared working directory

All stages operate in the same directory. Files written by stage 1 are on disk when stage 2 runs. The agent can `read` any file directly — you don't need to pass everything through template variables.

This means `{{prior_summary}}` combined with the agent's ability to read files is often sufficient. Use artifact variables when you want to be explicit about what changed.

### 4. Named stage references — `{{stages.<id>.*}}`

For non-sequential access (e.g., stage 4 referencing stage 1's output directly), use named references:

```yaml
stages:
  - id: "spec"
    name: "Write Spec"
    ...
  - id: "implement"
    name: "Implement"
    ...
  - id: "review"
    name: "Review"
    user_prompt: |
      Original spec: {{stages.spec.summary}}
      Implementation summary: {{stages.implement.summary}}

      Compare the implementation against the spec. Flag any deviations.
```

Available per-stage variables:
- `{{stages.<id>.summary}}` — Summarized output of that stage
- `{{stages.<id>.response}}` — Full raw output of that stage

This is especially important in **branching pipelines** where some stages may be skipped, making `{{prior_summary}}` unpredictable.

## Choosing models per stage

Different stages have different complexity requirements. Matching the model to the task saves money and often improves results (simpler models are less prone to overthinking mechanical tasks).

```yaml
stages:
  - id: "architecture"
    name: "Design Architecture"
    model: "claude-opus-4-6"           # Complex reasoning, trade-off analysis
    system_prompt: |
      You are a senior software architect. Consider scalability,
      maintainability, and the team's existing patterns.
    user_prompt: |
      Design the architecture for a real-time notification system
      that handles 10k concurrent WebSocket connections. Document
      the design in docs/architecture.md.
    allowed_tools: ["read", "write"]
  - id: "implement"
    name: "Implement Core"
    model: "claude-sonnet-4-6"         # Solid coding, good balance
    system_prompt: |
      You are a Rust developer. Follow the architecture document
      exactly. Write idiomatic Rust with proper error handling.
    user_prompt: |
      Architecture: {{prior_summary}}
      Read docs/architecture.md and implement the core modules.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "tests"
    name: "Write Tests"
    model: "claude-sonnet-4-6"         # Tests are structured, Sonnet handles well
    system_prompt: |
      You are a test engineer. Write integration tests that verify
      the WebSocket connection handling under concurrent load.
    user_prompt: |
      Implementation summary: {{prior_summary}}
      Files: {{artifact_list}}
      Write tests and run them.
    allowed_tools: ["read", "write", "bash"]
  - id: "docs"
    name: "Write README"
    model: "claude-sonnet-4-6"         # Documentation doesn't need Opus
    system_prompt: |
      You are a technical writer. Write clear, developer-friendly docs.
    user_prompt: |
      Project summary: {{prior_summary}}
      Write a README.md covering setup, usage, and architecture overview.
    allowed_tools: ["read", "write"]
```

### Model selection guidelines

| Model | Best for | Typical stages |
|-------|----------|---------------|
| `claude-opus-4-6` | Complex reasoning, architecture decisions, nuanced trade-offs | Design, complex refactoring, security review |
| `claude-sonnet-4-6` | General-purpose coding, test writing, documentation | Implementation, tests, docs, code review |
| `claude-haiku-4-5-20251001` | Fast mechanical tasks, simple edits | Formatting, simple migrations, summaries |

You can also mix runtimes. Use Claude Code for most stages and switch to Copilot CLI for a specific stage that benefits from a different model:

```yaml
  - id: "alternate-review"
    name: "Second Opinion Review"
    runtime: "copilot-cli"
    model: "gpt-5"
    system_prompt: "You are a code reviewer."
    user_prompt: |
      Review the implementation for correctness.
    allowed_tools: ["read"]
```

## Controlling agent behavior with tools

The `allowed_tools` field is your primary lever for constraining what an agent can do:

```yaml
# Read-only stage — the agent can inspect but not modify
- id: "review"
  allowed_tools: ["read", "glob", "grep"]

# Full access — needed for implementation stages
- id: "implement"
  allowed_tools: ["read", "write", "edit", "bash"]

# Shell only — for running builds and tests
- id: "validate"
  allowed_tools: ["read", "bash"]
```

Common tool combinations by stage type:

| Stage type | Tools | Rationale |
|-----------|-------|-----------|
| Design/Planning | `read`, `write` | Creates docs, reads existing code for context |
| Implementation | `read`, `write`, `edit`, `bash` | Needs full access to build things |
| Review | `read`, `glob`, `grep` | Should examine but not modify |
| Testing | `read`, `write`, `bash` | Writes test files, runs test commands |
| Documentation | `read`, `write` | Creates markdown files |

## Mixing command stages and agent stages

Command stages run shell commands directly, without an AI agent. They're perfect for deterministic build and test steps:

```yaml
stages:
  - id: "implement"
    name: "Implement Feature"
    system_prompt: "You are a Rust developer."
    user_prompt: "Add pagination to the list endpoint."
    allowed_tools: ["read", "write", "edit", "bash"]
  - id: "build"
    name: "Build and Test"
    commands:
      - "cargo build --release 2>&1"
      - "cargo test 2>&1"
      - "cargo clippy -- -D warnings 2>&1"
  - id: "fix"
    name: "Fix Issues"
    system_prompt: |
      You are a Rust developer. Fix any build errors, test failures,
      or clippy warnings from the previous step.
    user_prompt: |
      Build output: {{prior_summary}}

      Fix all issues. Run `cargo test` and `cargo clippy` again to verify.
    allowed_tools: ["read", "edit", "bash"]
```

This pattern — implement → build → fix — is powerful because the build step gives deterministic feedback that the fix stage can act on.

Commands support template variables:

```yaml
variables:
  target: "x86_64-unknown-linux-gnu"

stages:
  - id: "cross-build"
    name: "Cross Compile"
    commands:
      - "cargo build --release --target {{target}}"
```

## Configuring the summarizer

The summarizer condenses stage output for `{{prior_summary}}`. The defaults work well for most cases:

```yaml
summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 1024
```

Adjust for specific needs:

- **More detailed summaries**: Increase `max_tokens` to 2048. Useful when stages produce complex output that the next stage needs to understand in detail.
- **Very long stage output**: Haiku handles this fine — it gets the full output and summarizes. The `max_tokens` controls the summary length, not the input.
- **Cost-sensitive pipelines**: Haiku is already the cheapest option. The summarizer runs once per stage transition, so the cost is minimal.

## Realistic example: Full-stack feature implementation

Here's a complete pipeline that takes a feature spec and produces implemented, tested, reviewed code:

```yaml
name: "Feature Implementation Pipeline"

variables:
  repo_context: "This is an existing Express.js + React app with PostgreSQL. The backend is in server/, the frontend in client/."
  feature: "Add user profile pages with avatar upload, bio editing, and activity history"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 2048

stages:
  - id: "plan"
    name: "Plan Implementation"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior full-stack engineer. You plan implementations
      by reading the existing codebase and producing a detailed
      implementation plan. You identify which files to modify,
      what new files to create, and what the key technical decisions are.
    user_prompt: |
      Context: {{repo_context}}

      Feature request: {{feature}}

      Read the existing codebase to understand the patterns. Then create
      an implementation plan at docs/plan.md covering:
      - Database schema changes (migrations)
      - New API endpoints
      - Frontend components and routes
      - File upload handling approach
      - List of files to create/modify
    allowed_tools: ["read", "write", "glob", "grep"]
  - id: "backend"
    name: "Implement Backend"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a backend developer working on an Express.js API with
      PostgreSQL. Follow the existing patterns in server/ for routes,
      controllers, and models. Use the existing middleware and error
      handling conventions.
    user_prompt: |
      Implementation plan: {{prior_summary}}
      Read docs/plan.md for the full plan.

      Implement the backend portions:
      - Database migration
      - API routes and controllers
      - File upload middleware
      - Input validation

      Run `npm test` in server/ when done.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "frontend"
    name: "Implement Frontend"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a frontend developer working on a React app with
      TypeScript. Follow the existing component patterns in client/src/.
      Use the existing API client and state management approach.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Backend changes: {{prior_summary}}

      Implement the frontend:
      - Profile page component
      - Avatar upload component
      - Bio editor
      - Activity history feed
      - Route registration

      Run `npm test` in client/ when done.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "integration-test"
    name: "Run Full Test Suite"
    commands:
      - "cd server && npm test 2>&1"
      - "cd client && npm test 2>&1"
  - id: "review"
    name: "Code Review"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior engineer doing a thorough code review.
      Check for security issues (especially in file upload handling),
      missing error handling, SQL injection, XSS, accessibility issues,
      and adherence to existing codebase patterns. Fix any issues you find.
    user_prompt: |
      Original plan: {{stages.plan.summary}}
      Test results: {{prior_summary}}

      Review all changes made in this pipeline:
      {{artifact_list}}

      Fix any issues directly. Run the test suite again to verify.
    allowed_tools: ["read", "edit", "bash"]

```

Key patterns in this example:

- **Opus for planning and review**, Sonnet for implementation — matches complexity to capability
- **`{{stages.plan.summary}}`** used in the frontend stage to reference the plan directly, not just the backend output
- **Command stage** for integration tests — deterministic, no AI needed
- **Review stage at the end** catches issues across all implementation stages
- **Generous `timeout_secs`** can be set for complex implementation stages if you want a safety limit

## What to read next

- **[Branching and Conditions](branching-and-conditions.md)** — Skip stages or route to different paths based on prior output
- **[Exports and Dynamic Variables](exports-and-dynamic-variables.md)** — Let agents set variables at runtime for downstream stages
- **[Pipeline Patterns](pipeline-patterns.md)** — Ready-to-use patterns for common workflows
