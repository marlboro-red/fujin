# Best Practices

This guide covers practical advice for getting the most out of fujin — writing efficient pipelines, managing costs, keeping your workspace clean, and avoiding common mistakes.

## Clean up after your pipelines

Pipelines leave artifacts behind. Some are intentional (the code you wanted), others are not (intermediate files, design docs, exports). A clean workspace prevents confusion and avoids committing junk.

### Remove intermediate files

Pipelines often create temporary artifacts — spec documents, migration plans, codebase maps — that are useful between stages but shouldn't live in your repository permanently. Add a cleanup stage at the end:

```yaml
  - id: "cleanup"
    name: "Clean Up"
    commands:
      - "rm -f docs/spec.md docs/migration-plan.md docs/codebase-map.md"
      - "rm -f docs/approach-a.md docs/approach-b.md"
```

Alternatively, write intermediate files to a dedicated directory and remove it in one shot:

```yaml
variables:
  scratch_dir: ".fujin-scratch"

stages:
  - id: "spec"
    user_prompt: |
      Write the spec to {{scratch_dir}}/spec.md
    ...

  - id: "implement"
    user_prompt: |
      Read the spec at {{scratch_dir}}/spec.md and implement it.
    ...

  - id: "cleanup"
    name: "Clean Up Scratch Files"
    commands:
      - "rm -rf {{scratch_dir}}"
```

### Clear checkpoints

Checkpoints accumulate over time. They live outside your repository (in your platform data directory), so they won't show up in `git status`, but they still consume disk space.

```bash
# See what checkpoints exist
fujin checkpoint list

# Remove all checkpoints
fujin checkpoint clean
```

Get in the habit of cleaning checkpoints after a pipeline completes successfully and you've verified the results.

### Mind your `.gitignore`

If your pipelines create files you never want committed, add them to `.gitignore`:

```gitignore
# Fujin scratch files
.fujin-scratch/

# Common pipeline intermediates
docs/spec.md
docs/approach-*.md
```

## Write effective prompts

The quality of your pipeline output depends heavily on your prompts. A few principles go a long way.

### Be specific in system prompts

Bad:
```yaml
system_prompt: "You are a helpful assistant."
```

Good:
```yaml
system_prompt: |
  You are a senior Go developer. You follow the project's existing
  patterns for error handling (wrapped errors with %w), logging
  (structured slog), and testing (table-driven tests).
  Do not introduce new dependencies without justification.
```

The system prompt sets the agent's persona and constraints. Include the language, framework, conventions to follow, and things to avoid.

### Give the agent enough context, but not too much

- Use `{{prior_summary}}` when the agent just needs to understand what happened before.
- Use `{{stages.<id>.summary}}` when referencing a specific earlier stage — especially in branching pipelines where `{{prior_summary}}` might come from a skipped stage.
- Use `{{artifact_list}}` when the agent needs to know what files exist.
- Avoid `{{all_artifacts}}` for large codebases — it inlines every changed file into the prompt, which can blow up context windows and cost.
- Let agents use `read` and `glob` to explore files themselves instead of cramming everything into the prompt.

### Tell the agent what *not* to do

Agents are eager to help. Without constraints, they'll refactor code you didn't ask about, add features beyond scope, or introduce new dependencies. Set boundaries:

```yaml
system_prompt: |
  You are fixing build failures. Make minimal, targeted fixes.
  Do not refactor surrounding code. Do not add new features.
  Do not change code that isn't related to the failing tests.
```

### End analysis stages with structured verdicts

When a stage's output drives conditional logic (via `when`), end with a clear, parseable verdict:

```yaml
user_prompt: |
  Analyze the test coverage.
  End with exactly one of:
  VERDICT: NEEDS_TESTS
  VERDICT: ADEQUATE
```

This gives `output_matches` a reliable pattern to match against, rather than hoping the agent uses a particular phrase somewhere in its output.

## Control costs

AI API calls are the primary cost in fujin pipelines. A few choices make a big difference.

### Match model to task complexity

| Task | Model | Why |
|------|-------|-----|
| Architecture, design decisions | `claude-opus-4-6` | Needs deep reasoning |
| Code implementation, tests | `claude-sonnet-4-6` | Good balance of quality and cost |
| Classification, summarization | `claude-haiku-4-5-20251001` | Fast, cheap, sufficient |
| Build, test, lint | Command stage | Free — no API call |

Don't default to Opus for everything. Sonnet handles most coding tasks well, and using Opus where it's not needed costs more without proportional benefit.

### Use command stages for deterministic work

Any step with a known command belongs in a command stage:

```yaml
  - id: "test"
    commands:
      - "cargo test 2>&1"
      - "cargo clippy -- -D warnings 2>&1"
```

Command stages are free, fast, and produce exact output. Don't pay for an agent to run `cargo test` when a command stage does it better.

### Skip stages with `when` instead of branching

`when` is a regex check — free and instant. `branch` runs an AI classifier — one extra API call. Use `when` for binary decisions:

```yaml
# Free: regex check
when:
  stage: "analyze"
  output_matches: "NEEDS_WORK"

# Costs an API call: AI classifier
branch:
  prompt: "What type of work is needed?"
  routes: [frontend, backend, infra]
```

Reserve `branch` for multi-way routing where a regex can't capture the decision.

### Keep summarizer tokens reasonable

The default `max_tokens: 1024` works for most cases. Only increase it if you're seeing downstream stages miss important context. Larger summaries mean more tokens consumed in every subsequent stage's prompt.

### Validate before you run

```bash
# Check config structure
fujin validate -c pipeline.yaml

# Preview rendered prompts without API calls
fujin run -c pipeline.yaml --dry-run
```

Catching a typo in `--dry-run` is free. Catching it after three Opus stages have already run is not.

## Restrict tools to the minimum needed

Every tool you grant is a capability the agent might misuse. Be deliberate:

```yaml
# Design stage: read the codebase, write a spec. No shell access needed.
allowed_tools: ["read", "write", "glob", "grep"]

# Review stage: examine code, don't modify it.
allowed_tools: ["read", "glob", "grep"]

# Implementation: needs full access.
allowed_tools: ["read", "write", "edit", "bash"]
```

Specific concerns:

- **Don't give `bash` to review stages.** A review stage should examine code and report findings — not execute arbitrary commands.
- **Don't give `write` to review stages** unless you want the reviewer to fix issues directly. Sometimes you do (reviewer-as-fixer pattern), but be intentional about it.
- **Omit `edit` from stages that only create new files.** `write` creates files; `edit` modifies existing ones. If a stage is only scaffolding new code, it doesn't need `edit`.

## Design stages with single responsibilities

Each stage should do one thing well. Resist the temptation to combine everything into one mega-stage.

### Bad: monolithic stage

```yaml
  - id: "do-everything"
    user_prompt: |
      Read the codebase, design the architecture, implement the feature,
      write tests, run them, fix any failures, update the docs, and
      format the code.
```

This fails because: no checkpointing between steps, no cost optimization (one model for everything), too many instructions for the agent to track, and if anything fails you start over.

### Good: decomposed stages

```yaml
  - id: "design"
    model: "claude-opus-4-6"
    user_prompt: "Design the architecture..."
    allowed_tools: ["read", "write", "glob"]

  - id: "implement"
    model: "claude-sonnet-4-6"
    user_prompt: "Implement the design..."
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "test"
    commands: ["cargo test 2>&1"]

  - id: "fix"
    model: "claude-sonnet-4-6"
    user_prompt: "Fix test failures..."
    allowed_tools: ["read", "edit", "bash"]
```

Each stage checkpoints independently, uses the right model, and has focused tool access.

### But don't over-decompose

Five stages for a one-file script is overkill. A two-stage pipeline (implement → test) is perfectly fine for simple tasks. Add stages when they provide clear value — a different model, a deterministic check, a distinct role.

## Use `--resume` effectively

Checkpoints are your safety net. Use them well.

### Resume after failures

If a stage fails (network timeout, agent error, bad output), fix the issue and resume:

```bash
fujin run -c pipeline.yaml --resume
```

Fujin skips completed stages and picks up where it left off.

### Don't modify the config between resume

Fujin checksums the config file. If you change the YAML and try `--resume`, it will reject the checkpoint because the config hash no longer matches. This is intentional — the checkpoint was created under different assumptions.

If you need to modify the config after a partial run, either:
- Start fresh (lose completed work)
- Fix the issue without changing the config (e.g., fix the workspace state manually)

## Redirect command output with `2>&1`

Always capture stderr in command stages:

```yaml
commands:
  - "cargo test 2>&1"
```

Without `2>&1`, only stdout is captured. Many tools write errors, warnings, and progress to stderr. The downstream fix stage needs to see those errors to act on them.

## Name variables consistently

Pick a naming convention and stick to it across the pipeline:

```yaml
# Consistent: all snake_case, clear purpose
variables:
  project_name: "myapp"
  target_language: "rust"
  build_cmd: "cargo build"
  test_cmd: "cargo test"

# Inconsistent: mixed naming, ambiguous
variables:
  name: "myapp"
  lang: "rust"
  buildCommand: "cargo build"
  testcmd: "cargo test"
```

If an `exports` stage discovers a value, use the same key name that downstream stages expect. Don't have one stage export `language` and another reference `lang`.

## Provide defaults for exported variables

Exports are discovered at runtime — if the discovery stage gets it wrong, downstream stages break. Define sensible defaults in `variables`:

```yaml
variables:
  language: "python"
  test_cmd: "pytest"

stages:
  - id: "discover"
    exports:
      keys: [language, test_cmd]
    user_prompt: |
      Analyze the project and write to {{exports_file}}:
      {"language": "...", "test_cmd": "..."}
    ...

  - id: "test"
    commands:
      - "{{test_cmd}} 2>&1"
```

If the discover stage fails or produces unexpected output, the pipeline falls back to the YAML defaults instead of crashing with an empty variable.

## Build pipelines incrementally

Don't write a ten-stage pipeline from scratch. Start small and grow:

1. **Start with one stage.** Get the core task working.
2. **Add a command stage.** Validate the output with a build or test step.
3. **Add a fix stage.** Let an agent respond to failures.
4. **Iterate.** Each addition should solve a real problem you observed in previous runs.

Use `--dry-run` at each step to verify templates render correctly before spending API credits.

## Use parallel stages for independent work

When stages don't depend on each other, run them concurrently:

```yaml
  - id: "lint"
    depends_on: []
    commands: ["cargo clippy 2>&1"]

  - id: "test"
    depends_on: []
    commands: ["cargo test 2>&1"]

  - id: "fix"
    depends_on: [lint, test]
    user_prompt: "Fix all issues from lint and test results..."
```

But don't force parallelism where it doesn't fit. If the backend stage needs to know the API contract to implement it, and the frontend stage also needs that contract, they both need the plan stage to finish first — that's a natural fan-out, not full parallelism.

### Use explicit stage references in parallel pipelines

When multiple stages converge, `{{prior_summary}}` merges summaries from all parents. This can be ambiguous. Prefer explicit references:

```yaml
  - id: "integrate"
    depends_on: [frontend, backend]
    user_prompt: |
      Frontend changes: {{stages.frontend.summary}}
      Backend changes: {{stages.backend.summary}}
      Run integration tests.
```

## Keep retry groups focused

Retry groups are powerful but can waste API credits if scoped too broadly.

### Good: tight retry loop

```yaml
retry_groups:
  build:
    max_retries: 3

stages:
  - id: "implement"
    retry_group: build
    ...
  - id: "test"
    retry_group: build
    commands: ["cargo test 2>&1"]
```

Two stages, clear feedback loop. If tests fail, the implementation retries with the error context.

### Bad: overly broad retry group

```yaml
# Don't include design/planning in retry groups
stages:
  - id: "design"
    retry_group: quality    # Wasteful — redesigning from scratch on test failure
  - id: "implement"
    retry_group: quality
  - id: "test"
    retry_group: quality
    commands: ["cargo test 2>&1"]
```

If tests fail, the entire group retries — including the design stage. The design was probably fine; only the implementation needed fixing. Keep retry groups to the implement-and-verify cycle.

### Use verify agents for subjective quality

Command stages catch objective failures (build errors, test failures). Verify agents catch subjective issues (code quality, missing edge cases):

```yaml
retry_groups:
  quality:
    max_retries: 2
    verify:
      model: "claude-haiku-4-5-20251001"
      system_prompt: "Check if the implementation meets all requirements."
      user_prompt: |
        Requirements: {{stages.spec.summary}}
        Verify: PASS or FAIL with explanation.
```

Use Haiku for the verify agent — it's fast and cheap, and verification is a simpler task than implementation.

## Pipeline hygiene checklist

Before committing a pipeline config, run through this:

- [ ] `fujin validate -c pipeline.yaml` passes without errors
- [ ] `fujin run -c pipeline.yaml --dry-run` shows correct rendered prompts
- [ ] Each stage has a clear, single responsibility
- [ ] Models are matched to task complexity (not Opus for everything)
- [ ] Tool access is restricted to what each stage actually needs
- [ ] Command stages have `2>&1` to capture stderr
- [ ] Template variables use consistent naming
- [ ] Intermediate artifacts have a cleanup strategy
- [ ] Retry groups are scoped to implement-and-verify cycles, not the whole pipeline
- [ ] Exported variables have fallback defaults in `variables`

## What to read next

- **[Getting Started](getting-started.md)** — Build your first pipeline from scratch
- **[Multi-Stage Pipelines](multi-stage-pipelines.md)** — Designing stage sequences and passing context
- **[Pipeline Patterns](pipeline-patterns.md)** — Ready-to-use recipes for common workflows
- **[Pipeline Authoring Reference](../pipeline-authoring.md)** — Complete field reference and validation rules
