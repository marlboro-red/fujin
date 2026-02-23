# fujin

A Rust CLI tool that orchestrates multi-stage AI agent pipelines. Define stages in YAML, and `fujin` executes them — passing context between stages, tracking file changes via git, and running independent stages in parallel when dependencies allow.

Fujin supports multiple agent runtimes:
- **[Claude Code](https://docs.anthropic.com/en/docs/claude-code)** — the default runtime, with structured JSON output and streaming progress
- **[GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli)** — an alternative runtime using Copilot's programmatic mode

You can choose a runtime per-pipeline or per-stage, and mix runtimes in the same pipeline.

## How It Works

```
cd /path/to/your/project
fujin run -c pipeline.yaml

For each stage (respecting dependency order, parallel when possible):
  1. Build context: summarize prior stage output + collect file diffs
  2. Spawn agent subprocess with stage config
  3. Agent executes — reads/writes files in the current directory
  4. Detect changes via git status → track artifacts
  5. Save checkpoint (JSON)
```

Agents handle all file I/O, bash execution, and tool use natively. The pipeline doesn't parse code blocks or manage file writes — it uses `git status` to track what changed. Models always run in the directory where `fujin` was invoked.

## Installation

Requires [Rust](https://rustup.rs/) and at least one agent runtime:

- **Claude Code CLI** — `npm install -g @anthropic-ai/claude-code` ([docs](https://docs.anthropic.com/en/docs/claude-code))
- **GitHub Copilot CLI** — `npm install -g @github/copilot` ([docs](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli))

```bash
# Clone and install
git clone <repo-url> && cd fujin
cargo install --path crates/fujin-cli
```

Verify setup:

```bash
fujin agents --check    # checks all installed runtimes
```

## Quick Start

```bash
# Set up the data directory and install built-in templates
fujin setup

# Scaffold a config from a template
fujin init --template code-and-docs -o pipeline.yaml

# Edit the config to describe your project
fujin edit pipeline.yaml

# Validate it
fujin validate -c pipeline.yaml

# Preview the execution plan
fujin run -c pipeline.yaml --dry-run

# Run it (models operate in the current directory)
fujin run -c pipeline.yaml
```

## Pipeline Config

Pipelines are defined in YAML. Here's a minimal example:

```yaml
name: "My Pipeline"

variables:
  language: "rust"
  description: "A CLI tool that converts CSV to JSON"

stages:
  - id: "codegen"
    name: "Generate Code"
    system_prompt: |
      You are an expert {{language}} developer.
    user_prompt: |
      Create a project: {{description}}
    allowed_tools:
      - "write"
      - "read"
      - "bash"
```

A multi-stage pipeline passes context between stages automatically:

```yaml
name: "Code and Docs"

variables:
  language: "rust"
  description: "A REST API for todo management"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 1024

stages:
  - id: "architecture"
    name: "Design Architecture"
    system_prompt: |
      You are a senior software architect.
    user_prompt: |
      Design the architecture for: {{description}}

  - id: "codegen"
    name: "Generate Code"
    system_prompt: |
      You are an expert {{language}} developer.
    user_prompt: |
      Previous stage summary: {{prior_summary}}
      Implement the project based on the architecture doc.
    allowed_tools: ["write", "read", "bash"]

  - id: "docs"
    name: "Write Documentation"
    system_prompt: |
      You are a technical writer.
    user_prompt: |
      Previous stage summary: {{prior_summary}}
      Files from previous stages: {{artifact_list}}
      Review all code and write comprehensive docs.
```

## Features

### Agent Runtimes

By default, pipelines use Claude Code. Set the `runtime` field to choose a different agent runtime:

```yaml
name: "My Pipeline"
runtime: "copilot-cli"    # use GitHub Copilot CLI for all stages

stages:
  - id: "codegen"
    name: "Generate Code"
    model: "claude-sonnet-4"    # use Copilot CLI model names
    system_prompt: |
      You are an expert developer.
    user_prompt: |
      Create a project.
    allowed_tools:
      - "write"
      - "read"
      - "bash"
```

You can also override the runtime per-stage to mix runtimes in the same pipeline:

```yaml
name: "Mixed Runtime Pipeline"
runtime: "claude-code"    # default runtime

stages:
  - id: "codegen"
    name: "Generate Code"
    model: "claude-sonnet-4-6"
    system_prompt: "You are an expert developer."
    user_prompt: "Implement the feature."
    allowed_tools: ["write", "read", "bash"]

  - id: "review"
    name: "Review Code"
    runtime: "copilot-cli"    # override: use Copilot CLI for this stage
    model: "gpt-5"
    system_prompt: "You are a senior code reviewer."
    user_prompt: "Review the code for correctness and style."
    allowed_tools: ["read"]
```

Available runtimes: `claude-code` (default), `copilot-cli`.

> **Note:** Model names differ between runtimes. Claude Code uses names like `claude-sonnet-4-6` while Copilot CLI uses `claude-sonnet-4`, `claude-haiku-4.5`, or `gpt-5`. Use the model names appropriate for the stage's runtime.

### Command Stages

Not every stage needs an AI agent. **Command stages** run shell commands directly, without spawning Claude. Use them for build steps, tests, linting, or any CLI task in your pipeline.

A command stage uses the `commands` field instead of `system_prompt` / `user_prompt`:

```yaml
stages:
  - id: "build"
    name: "Build Project"
    commands:
      - "cargo build --release"
      - "cargo test"
```

Commands run sequentially. If any command exits with a non-zero status, the stage fails and the pipeline stops (with a checkpoint saved so you can `--resume`).

Commands support template variables, just like prompts:

```yaml
variables:
  target: "x86_64-unknown-linux-gnu"

stages:
  - id: "cross-build"
    name: "Cross Compile"
    commands:
      - "cargo build --release --target {{target}}"
```

The built-in variables `{{stage_id}}` and `{{stage_name}}` are also available in commands.

You can mix command stages and agent stages in the same pipeline:

```yaml
stages:
  - id: "codegen"
    name: "Generate Code"
    system_prompt: |
      You are an expert developer.
    user_prompt: |
      Implement the feature described in SPEC.md.
    allowed_tools: ["read", "write", "bash"]

  - id: "test"
    name: "Run Tests"
    commands:
      - "cargo test"
      - "cargo clippy -- -D warnings"
```

When a stage has `commands`, the agent-specific fields (`system_prompt`, `user_prompt`, `model`, `allowed_tools`) are ignored. Only `id`, `name`, `timeout_secs`, and `commands` apply.

### Parallel Stages

By default, stages execute sequentially — each stage implicitly depends on the previous one. The `dependencies` field lets you declare explicit dependencies so that independent stages can run in parallel:

```yaml
stages:
  - id: analyze
    name: Analyze
    system_prompt: "You are a senior architect."
    user_prompt: "Read SPEC.md and create a plan."
    allowed_tools: ["read", "write"]

  - id: frontend
    name: Build Frontend
    dependencies: [analyze]
    system_prompt: "You are a frontend developer."
    user_prompt: "Plan: {{stages.analyze.summary}}. Build the UI."
    allowed_tools: ["read", "write", "bash"]

  - id: backend
    name: Build Backend
    dependencies: [analyze]
    system_prompt: "You are a backend developer."
    user_prompt: "Plan: {{stages.analyze.summary}}. Build the API."
    allowed_tools: ["read", "write", "bash"]

  - id: integrate
    name: Integration Tests
    dependencies: [frontend, backend]
    commands:
      - "npm test 2>&1"
```

This creates a diamond dependency graph — `frontend` and `backend` run in parallel after `analyze` completes, and `integrate` waits for both.

| Config | Behavior |
|--------|----------|
| `dependencies` absent | Implicitly depends on the previous stage (sequential) |
| `dependencies: [a, b]` | Waits for all listed stages to complete |
| `dependencies: []` | No dependencies — starts immediately |

Existing pipelines without `dependencies` work exactly as before. See the **[Parallel Stages Guide](docs/guides/parallel-stages.md)** for fan-out/fan-in topologies, context passing in DAG pipelines, and interaction with retry groups and branching.

### Conditional Execution

Stages can be skipped based on prior results — enabling branching workflows.

#### `when` — regex gating

Skip a stage unless a prior stage's output matches a regex pattern:

```yaml
stages:
  - id: analyze
    name: Analyze Task
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze this code. End with VERDICT: NEEDS_TESTS or SKIP_TESTS"

  - id: write-tests
    name: Write Tests
    when:
      stage: analyze
      output_matches: "NEEDS_TESTS"
    system_prompt: "You are a test engineer."
    user_prompt: "Write tests for the project."
```

#### `branch`/`on_branch` — AI-driven routing

A `branch` classifier picks a named route after a stage completes. Downstream stages with `on_branch` only run if their route was selected:

```yaml
stages:
  - id: analyze
    name: Analyze Task
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze the requirements"
    branch:
      prompt: "Classify the work needed"
      routes: [frontend, backend, fullstack]
      default: fullstack

  - id: frontend-impl
    name: Frontend Work
    on_branch: frontend
    system_prompt: "You are a frontend developer."
    user_prompt: "Build the frontend using {{stages.analyze.summary}}"

  - id: backend-impl
    name: Backend Work
    on_branch: backend
    system_prompt: "You are a backend developer."
    user_prompt: "Build the backend using {{stages.analyze.summary}}"

  - id: review
    name: Code Review
    # no on_branch = always runs (convergence point)
    system_prompt: "You are a code reviewer."
    user_prompt: "Review all changes"
```

`on_branch` accepts a single string or a list: `on_branch: [frontend, fullstack]` runs if either route was selected.

### Exports

Stages can export variables at runtime for downstream stages to use. The pipeline runner auto-generates a unique file path for each stage with `exports` and injects it as `{{exports_file}}`. The agent writes a flat JSON object to that path, and the runner merges its values into the template variables:

```yaml
stages:
  - id: analyze
    name: Analyze
    system_prompt: "You are a project analyzer."
    user_prompt: |
      Analyze the project. Write findings as a flat JSON object to {{exports_file}}:
      {"language": "...", "framework": "..."}
    exports:
      keys: [language, framework]    # optional: warns if keys are missing

  - id: implement
    name: Implement
    system_prompt: "You are a {{language}} developer."
    user_prompt: "Build a REST API using {{framework}}."
```

After the `analyze` stage completes, the runner reads the exports file and makes `{{language}}` and `{{framework}}` available to all subsequent stages — including command stages. If the file is missing or malformed, a warning is emitted but the pipeline continues.

Export files are stored in the platform data directory (not the workspace), so they don't pollute the repository.

### Checkpointing and Resume

After each stage completes, `fujin` saves a checkpoint to `.fujin/checkpoints/` in the current directory. If a stage fails or is interrupted:

```bash
# Resume from where it left off
fujin run -c pipeline.yaml --resume
```

Resume validates that the pipeline config hasn't changed since the checkpoint was created. If you've modified the config, clean the checkpoints and start fresh:

```bash
fujin checkpoint clean
fujin run -c pipeline.yaml
```

## Config Reference

See the [Pipeline Authoring Guide](docs/pipeline-authoring.md) for the full field reference.

**Top-level fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | *required* | Pipeline name |
| `version` | string | `"1.0"` | Config version |
| `runtime` | string | `"claude-code"` | Default agent runtime (`claude-code` or `copilot-cli`) |
| `variables` | map | `{}` | Template variables for prompts |
| `summarizer` | object | see below | Inter-stage summarizer settings |
| `retry_groups` | map | `{}` | Retry group definitions (see [Retry Groups](docs/guides/retry-groups.md)) |
| `stages` | list | *required* | Pipeline stages (at least 1) |

**Summarizer fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | `"claude-haiku-4-5-20251001"` | Model for summarization |
| `max_tokens` | integer | `1024` | Max tokens for summaries |

**Stage fields (common):**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | string | *required* | Unique stage identifier |
| `name` | string | *required* | Human-readable name |
| `timeout_secs` | integer | — | Stage timeout in seconds (no limit if unset) |
| `commands` | list | — | Shell commands to run (makes this a command stage) |
| `dependencies` | list | — | Explicit dependencies (see [Parallel Stages](#parallel-stages)). Absent = depends on previous stage; `[]` = no dependencies |

**Agent stage fields** (ignored when `commands` is set):

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `runtime` | string | pipeline default | Override runtime for this stage |
| `model` | string | `"claude-sonnet-4-6"` | Model to use (names vary by runtime) |
| `system_prompt` | string | *required* | System prompt for the agent |
| `user_prompt` | string | *required* | User prompt template |
| `allowed_tools` | list | `["read", "write"]` | Tools the agent can use |

**Conditional execution fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `when` | object | — | Gate execution on a prior stage's output (see [Conditional Execution](#conditional-execution)) |
| `branch` | object | — | AI-driven route classifier (see [Conditional Execution](#conditional-execution)) |
| `on_branch` | string or list | — | Only run if the named branch route was selected |
| `exports` | object | — | Export agent-set variables for downstream stages (see [Exports](#exports)) |

**Allowed tools:** `read`, `write`, `edit`, `bash`, `glob`, `grep`, `notebook`

Tool names are the same across runtimes — fujin maps them to runtime-specific names automatically (e.g., `bash` maps to `Bash` in Claude Code and `shell` in Copilot CLI).

### Template Variables

Prompts and commands use [Handlebars](https://handlebarsjs.com/) syntax. Available variables:

| Variable | Description |
|----------|-------------|
| `{{<key>}}` | Any key defined in `variables` |
| `{{prior_summary}}` | Summary of the previous stage's output (or combined parent summaries in DAG pipelines) |
| `{{artifact_list}}` | Newline-separated list of files changed by prior stages |
| `{{all_artifacts}}` | Changed file paths with their full content |
| `{{stages.<id>.summary}}` | Summary of a specific completed stage (by stage ID) |
| `{{stages.<id>.response}}` | Full response text of a specific completed stage |
| `{{exports_file}}` | Path where the agent should write exports JSON (only for stages with `exports`) |

Variables can be overridden from the CLI:

```bash
fujin run -c pipeline.yaml --var language=python --var description="A web scraper"
```

## CLI Reference

### `fujin run`

Execute a pipeline. Models run in the current directory.

```
fujin run -c <config> [OPTIONS]

Options:
  -c, --config <NAME|PATH>  Pipeline config name or path (required)
      --resume              Resume from last checkpoint
      --dry-run             Show execution plan without running
      --var <KEY=VALUE>     Override template variables (repeatable)
```

The `-c` flag accepts either an explicit file path (`-c ./my-pipeline.yaml`) or a config name (`-c my-pipeline`). When given a name (no path separator or extension), it searches the current directory and then the global configs directory.

### `fujin validate`

Validate a config file without executing.

```
fujin validate -c <config>
```

### `fujin agents`

Check agent runtime availability.

```
fujin agents [--check]

Options:
  --check    Run health checks on all installed runtimes (claude and copilot)
```

### `fujin init`

Create a new config from a template.

```
fujin init [--template <NAME>] [-o <PATH>] [--list] [--local]

Options:
  --template <NAME>    Template name (default: "simple")
  -o, --output <PATH>  Output file path (default: "pipeline.yaml")
  --list               List all available templates (built-in + user)
  --local              Write output to the current directory instead of the global configs directory
```

Templates are loaded from the data directory first (user-customizable), then from built-in defaults. Run `fujin setup` to install built-in templates locally.

### `fujin edit`

Open a pipeline config in an editor (defaults to VS Code).

```
fujin edit [OPTIONS] <NAME|PATH>

Options:
  --editor <COMMAND>  Editor command to use (default: "code")
```

Accepts a config name (`fujin edit simple.yaml`) or a path. When given a filename, it searches the current directory first, then the global configs directory.

### `fujin setup`

Set up the data directory structure and install built-in templates.

```
fujin setup
```

Creates the platform-specific data directory:
- **macOS**: `~/Library/Application Support/fujin/`
- **Linux**: `~/.local/share/fujin/`
- **Windows**: `%LOCALAPPDATA%/fujin/`

### `fujin checkpoint`

Manage pipeline checkpoints.

```
fujin checkpoint list                # List all checkpoints
fujin checkpoint show <run-id>       # Show checkpoint details
fujin checkpoint clean               # Remove all checkpoints
```

Checkpoint commands operate on `.fujin/checkpoints/` in the current directory.

## Guides

- **[Getting Started](docs/guides/getting-started.md)** — Create your first pipeline from scratch
- **[Multi-Stage Pipelines](docs/guides/multi-stage-pipelines.md)** — Context passing, model selection, and stage composition
- **[Parallel Stages](docs/guides/parallel-stages.md)** — Run independent stages concurrently with `dependencies`
- **[Branching and Conditions](docs/guides/branching-and-conditions.md)** — Conditional execution with `when` and `branch`/`on_branch`
- **[Exports and Dynamic Variables](docs/guides/exports-and-dynamic-variables.md)** — Let agents set variables at runtime
- **[Retry Groups](docs/guides/retry-groups.md)** — Automatic retry-on-failure with verification agents
- **[Pipeline Patterns](docs/guides/pipeline-patterns.md)** — Ready-to-use recipes for common workflows

Run `fujin init --list` to see all available templates, or `fujin setup` to install them locally where you can customize them.

## Project Structure

```
fujin/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── fujin-cli/                # Binary (fujin) — CLI entry point
│   │   ├── configs/              # Built-in templates (compiled into binary)
│   │   └── src/
│   │       └── main.rs           # CLI entry point
│   ├── fujin-core/               # Library — pipeline engine
│   │   └── src/
│   │       ├── pipeline.rs       # Pipeline execution loop (sequential + DAG)
│   │       ├── dag.rs            # DAG construction and parallel scheduling
│   │       ├── stage.rs          # Stage result types
│   │       ├── agent.rs          # AgentRuntime trait + ClaudeCodeRuntime
│   │       ├── copilot.rs        # CopilotCliRuntime (GitHub Copilot CLI)
│   │       ├── artifact.rs       # File change tracking
│   │       ├── workspace.rs      # Filesystem snapshot/diff
│   │       ├── checkpoint.rs     # Checkpoint save/load/resume
│   │       ├── context.rs        # Context building + summarization
│   │       ├── event.rs          # Pipeline event types
│   │       ├── paths.rs          # Platform data directory helpers
│   │       ├── util.rs           # Shared utilities
│   │       └── error.rs          # Error types
│   ├── fujin-config/             # Library — YAML config parsing & validation
│   │   └── src/
│   │       ├── pipeline_config.rs
│   │       └── validation.rs
│   └── fujin-tui/                # Library — terminal UI for interactive mode
│       └── src/
│           ├── app.rs            # Application state + event loop
│           ├── discovery.rs      # Pipeline file discovery
│           ├── screens/          # Browser, variables, execution screens
│           ├── widgets/          # Reusable UI components
│           └── theme.rs          # Colors and styling
```

## Development

```bash
cargo build              # Build all crates
cargo test               # Run all tests
cargo clippy             # Lint
cargo run --bin fujin -- --help
```

## License

MIT
