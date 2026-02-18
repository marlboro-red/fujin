# fujin

A Rust CLI tool that orchestrates multi-stage AI agent pipelines using [Claude Code](https://docs.anthropic.com/en/docs/claude-code). Define stages in YAML, and `fujin` executes them sequentially — each stage gets a fresh Claude Code context, operates directly on the current directory, and passes summarized results to the next stage.

## How It Works

```
cd /path/to/your/project
fujin run -c pipeline.yaml

For each stage:
  1. Snapshot current directory (SHA-256 hash all files)
  2. Build context: summarize prior stage output + collect file diffs
  3. Spawn `claude` CLI subprocess with stage config
  4. Claude Code executes — reads/writes files in the current directory
  5. Diff directory against snapshot → track artifacts
  6. Save checkpoint (JSON)
  7. Next stage
```

Claude Code handles all file I/O, bash execution, and tool use natively. The pipeline doesn't parse code blocks or manage file writes — it just tracks what changed. Models always run in the directory where `fujin` was invoked.

## Installation

Requires [Rust](https://rustup.rs/) and the [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code).

```bash
# Clone and build
git clone <repo-url> && cd wtg-pipeline
cargo build --release

# The binary is at target/release/fujin
# Optionally add to PATH:
cp target/release/fujin ~/.local/bin/
```

Verify setup:

```bash
fujin agents --check
```

## Quick Start

```bash
# Set up the data directory and install built-in templates
fujin setup

# Scaffold a config from a template
fujin init --template code-and-docs -o pipeline.yaml

# Edit the config to describe your project
$EDITOR pipeline.yaml

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
    max_turns: 15
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
    max_turns: 30
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

Run `fujin init --list` to see all available templates, or `fujin setup` to install them locally where you can customize them.

### Config Reference

**Top-level fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | *required* | Pipeline name |
| `version` | string | `"1.0"` | Config version |
| `variables` | map | `{}` | Template variables for prompts |
| `summarizer` | object | see below | Inter-stage summarizer settings |
| `stages` | list | *required* | Pipeline stages (at least 1) |

**Summarizer fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | `"claude-haiku-4-5-20251001"` | Model for summarization |
| `max_tokens` | integer | `1024` | Max tokens for summaries |

**Stage fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | string | *required* | Unique stage identifier |
| `name` | string | *required* | Human-readable name |
| `model` | string | `"claude-sonnet-4-6"` | Claude model to use |
| `system_prompt` | string | *required* | System prompt for the agent |
| `user_prompt` | string | *required* | User prompt template |
| `max_turns` | integer | `10` | Max agentic turns for Claude Code |
| `allowed_tools` | list | `["read", "write"]` | Tools Claude Code can use |
| `timeout_secs` | integer | `300` | Stage timeout in seconds |

**Allowed tools:** `read`, `write`, `edit`, `bash`, `glob`, `grep`, `notebook`

### Template Variables

Prompts use [Handlebars](https://handlebarsjs.com/) syntax. Available variables:

| Variable | Description |
|----------|-------------|
| `{{<key>}}` | Any key defined in `variables` |
| `{{prior_summary}}` | Summary of the previous stage's output |
| `{{artifact_list}}` | Newline-separated list of files changed by prior stages |
| `{{all_artifacts}}` | Changed file paths with their full content |

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
  --check    Run health checks (tests that `claude` CLI is installed and working)
```

### `fujin init`

Create a new config from a template.

```
fujin init [--template <NAME>] [-o <PATH>] [--list] [--global]

Options:
  --template <NAME>    Template name (default: "simple")
  -o, --output <PATH>  Output file path (default: "pipeline.yaml")
  --list               List all available templates (built-in + user)
  --global             Write output to the global configs directory
```

Templates are loaded from the data directory first (user-customizable), then from built-in defaults. Run `fujin setup` to install built-in templates locally.

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

## Checkpointing and Resume

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

## Project Structure

```
wtg-pipeline/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── fujin-cli/                # Binary (fujin) — CLI entry point
│   │   ├── configs/              # Built-in templates (compiled into binary)
│   │   └── src/
│   │       ├── main.rs           # CLI entry point
│   │       └── paths.rs          # Platform data directory helpers
│   ├── fujin-core/               # Library — pipeline engine
│   │   └── src/
│   │       ├── pipeline.rs       # Pipeline execution loop
│   │       ├── stage.rs          # Stage result types
│   │       ├── agent.rs          # AgentRuntime trait + ClaudeCodeRuntime
│   │       ├── artifact.rs       # File change tracking
│   │       ├── workspace.rs      # Filesystem snapshot/diff
│   │       ├── checkpoint.rs     # Checkpoint save/load/resume
│   │       ├── context.rs        # Context building + summarization
│   │       └── error.rs          # Error types
│   └── fujin-config/             # Library — YAML config parsing & validation
│       └── src/
│           ├── pipeline_config.rs
│           └── validation.rs
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
