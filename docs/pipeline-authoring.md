# Pipeline Authoring Guide

This document covers everything you need to know to create fujin pipeline configuration files.

## Overview

A fujin pipeline is a YAML file that defines a sequence of **stages**. Each stage invokes a Claude Code agent with a specific prompt, model, and set of tools. Stages execute sequentially — the output of one stage feeds into the next through automatic summarization and file change tracking. Models always run in the directory where `fujin` was invoked.

## Where to put pipeline files

Fujin discovers pipeline configs from two locations:

| Location | Platform path |
|----------|--------------|
| Current directory | `./*.yaml` |
| Global configs dir | macOS: `~/Library/Application Support/fujin/configs/`<br>Linux: `~/.local/share/fujin/configs/`<br>Windows: `%LOCALAPPDATA%/fujin/configs/` |

Pipelines in the global configs directory appear in the TUI browser automatically. Use `fujin init` to scaffold a new config there, or `fujin init --local` to write it to the current directory.

You can also run any pipeline by path:

```
fujin run -c ./my-pipeline.yaml
fujin run -c my-pipeline          # searches CWD then global configs dir
```

---

## Full schema reference

### Top-level fields

```yaml
name: "My Pipeline"               # REQUIRED — human-readable name
version: "1.0"                     # optional, default: "1.0"
variables:                         # optional, default: {}
  key: "value"
summarizer:                        # optional, has defaults
  model: "claude-haiku-4-5-20251001"
  max_tokens: 1024
stages:                            # REQUIRED — at least one stage
  - ...
```

#### `name` (required, string)

A human-readable name for the pipeline. Displayed in the TUI and CLI output.

#### `version` (optional, string)

A version string for your config. Informational only. Default: `"1.0"`.

#### `variables` (optional, map)

Key-value pairs available as `{{variable_name}}` in prompt templates. Variables can be overridden at runtime:

```
fujin run -c pipeline.yaml --var language=python --var project_name=my-app
```

#### `summarizer` (optional, object)

Controls how the output of each stage is summarized before being passed to the next stage via `{{prior_summary}}`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | `claude-haiku-4-5-20251001` | Model used for summarization. Haiku is recommended for speed/cost. |
| `max_tokens` | integer | `1024` | Target max tokens for the summary. |

The summarizer calls Claude to condense the previous stage's full output into a concise summary focusing on what was accomplished, files changed, and key decisions.

#### `stages` (required, list)

An ordered list of stage configurations. Stages execute top-to-bottom. The pipeline must have at least one stage.

---

### Stage fields

```yaml
stages:
  - id: "codegen"                          # REQUIRED — unique identifier
    name: "Generate Code"                  # REQUIRED — display name
    model: "claude-sonnet-4-6"             # optional, default: "claude-sonnet-4-6"
    system_prompt: |                       # REQUIRED — sets agent behavior
      You are an expert developer...
    user_prompt: |                         # REQUIRED — the task (supports templates)
      Build a {{language}} project...
    allowed_tools:                         # optional, default: ["read", "write"]
      - "read"
      - "write"
      - "bash"
```

#### `id` (required, string)

A unique identifier for the stage. Used in checkpoints, logs, and event tracking. Must be unique across all stages in the pipeline.

Good IDs are short, descriptive, and kebab-case: `"design"`, `"codegen"`, `"write-tests"`, `"review"`.

#### `name` (required, string)

A human-readable name displayed in the TUI stage list and CLI output.

#### `model` (optional, string)

The Claude model to use for this stage. Default: `"claude-sonnet-4-6"`.

Available models:

| Model | Best for |
|-------|----------|
| `claude-opus-4-6` | Complex reasoning, architecture design, difficult tasks |
| `claude-sonnet-4-6` | General-purpose coding, good balance of quality and speed |
| `claude-haiku-4-5-20251001` | Fast, cheap tasks — summaries, simple edits, validation |

You can mix models across stages. Use a powerful model for complex design work and a cheaper model for mechanical tasks.

#### `system_prompt` (required, string)

Sets the agent's persona and behavioral constraints. This is sent as the system prompt to Claude Code.

Tips:
- Be specific about the agent's role: `"You are a senior Rust developer"` not `"You are helpful"`
- Include constraints: `"Only modify files in the src/ directory"`
- Mention output expectations: `"Create a file at docs/architecture.md"`
- System prompts support `{{variables}}` too

#### `user_prompt` (required, string)

The task prompt sent to the agent. This is where you describe what the stage should accomplish. Supports Handlebars template syntax with `{{variable_name}}`.

#### `allowed_tools` (optional, list of strings)

Controls which Claude Code tools the agent can use in this stage. Default: `["read", "write"]`.

Available tools:

| Tool | Description |
|------|-------------|
| `read` | Read file contents |
| `write` | Create or overwrite files |
| `edit` | Make targeted edits to existing files |
| `bash` | Run shell commands |
| `glob` | Find files by pattern |
| `grep` | Search file contents |
| `notebook` | Edit Jupyter notebooks |

Restricting tools is useful for safety and focus:
- A "design" stage might only need `read` + `write` (no shell access)
- A "build and test" stage needs `bash` to run compilers and test suites
- A "review" stage might only need `read` to inspect code

---

## Template variables

Prompt templates use [Handlebars](https://handlebarsjs.com/) syntax. Any `{{variable_name}}` in `system_prompt` or `user_prompt` is replaced with the corresponding value from `variables`.

### User-defined variables

Defined in the top-level `variables` map:

```yaml
variables:
  language: "rust"
  project_name: "my-api"
  description: "A REST API for managing todos"

stages:
  - id: "codegen"
    name: "Generate Code"
    system_prompt: "You are an expert {{language}} developer."
    user_prompt: "Build {{project_name}}: {{description}}"
```

### Built-in variables

These are automatically injected by fujin and don't need to be defined in `variables`:

| Variable | Available in | Description |
|----------|-------------|-------------|
| `{{prior_summary}}` | Stage 2+ | Summarized output from the previous stage |
| `{{artifact_list}}` | Stage 2+ | Newline-separated list of file paths changed by the previous stage |
| `{{all_artifacts}}` | Stage 2+ | Full content of files changed by the previous stage, formatted as `=== path ===\n<content>` blocks |

Using `{{prior_summary}}` in the first stage produces a validation warning since there is no prior stage.

### Runtime overrides

Variables can be overridden from the CLI without editing the YAML:

```
fujin run -c pipeline.yaml --var language=python --var description="A CLI tool"
```

Overrides take precedence over values in the YAML file.

---

## Inter-stage context passing

When a pipeline has multiple stages, fujin automatically passes context from one stage to the next through three mechanisms:

### 1. Prior summary (`{{prior_summary}}`)

After each stage completes, its full text output is summarized using the configured `summarizer` model. The summary is available as `{{prior_summary}}` in the next stage's prompt.

### 2. Shared directory

All stages share the same working directory (the directory where `fujin` was invoked). Files written by stage 1 are readable by stage 2. Fujin tracks file changes (created, modified, deleted) per stage and displays them in the TUI.

### 3. Artifact variables

`{{artifact_list}}` gives you a simple list of changed file paths. `{{all_artifacts}}` gives you the full content of those files inlined into the prompt. Use these when you want the next stage to explicitly see what the previous stage produced without needing to read files.

---

## Examples

### Minimal single-stage pipeline

The simplest possible pipeline:

```yaml
name: "Hello World"
stages:
  - id: "hello"
    name: "Say Hello"
    system_prompt: "You are a helpful assistant."
    user_prompt: "Create a file called hello.txt containing 'Hello, World!'"
```

This uses all defaults: Sonnet model, `read`+`write` tools, runs in the current directory.

### Single stage with variables

```yaml
name: "Code Generator"
variables:
  language: "rust"
  project_name: "csv-converter"
  description: "A CLI tool that converts CSV files to JSON"

stages:
  - id: "codegen"
    name: "Generate Code"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert {{language}} developer. Write clean, idiomatic,
      well-structured code. Include error handling and comments.
    user_prompt: |
      Create a {{language}} project called "{{project_name}}" that does:
      {{description}}

      Requirements:
      - Set up proper project structure
      - Include a README.md with usage instructions
      - Write complete, working code
    allowed_tools:
      - "write"
      - "read"
      - "bash"
```

### Multi-stage pipeline with context passing

A three-stage pipeline where each stage builds on the previous:

```yaml
name: "Full Project Generator"
variables:
  project_name: "my-api"
  language: "rust"
  description: "A REST API for managing a todo list"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 1024

stages:
  - id: "architecture"
    name: "Design Architecture"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior software architect. Design the architecture
      for the requested project. Create docs/architecture.md with
      the full design and directory structure.
    user_prompt: |
      Design the architecture for: {{description}}
      Project name: {{project_name}}
      Language: {{language}}
    allowed_tools:
      - "write"
      - "read"

  - id: "codegen"
    name: "Generate Code"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert {{language}} developer. Implement the project
      based on the architecture document. Write complete, working code.
    user_prompt: |
      Previous stage summary: {{prior_summary}}

      The architecture document is at docs/architecture.md.
      Read it and implement the full project.
    allowed_tools:
      - "write"
      - "read"
      - "bash"

  - id: "documentation"
    name: "Write Documentation"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a technical writer. Review the codebase and produce
      comprehensive Markdown documentation.
    user_prompt: |
      Previous stage summary: {{prior_summary}}

      Review all code in the project and create:
      - README.md with project overview, setup, and usage
      - docs/API.md with API endpoint documentation
      - docs/DEVELOPMENT.md with development guide
    allowed_tools:
      - "write"
      - "read"
```

### Review and refactor pipeline

A pipeline that generates code and then reviews/improves it:

```yaml
name: "Code with Review"
variables:
  task: "implement a binary search tree with insert, delete, and search"
  language: "python"

stages:
  - id: "implement"
    name: "Initial Implementation"
    system_prompt: "You are a {{language}} developer. Write working code."
    user_prompt: "{{task}}"
    allowed_tools:
      - "write"

  - id: "review"
    name: "Code Review"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior code reviewer. Read the code in the project
      and provide a detailed review. Fix any bugs, improve performance,
      and refactor for clarity. Edit files in place.
    user_prompt: |
      Previous stage summary: {{prior_summary}}

      Files to review:
      {{artifact_list}}

      Review the code for:
      - Correctness and edge cases
      - Performance
      - Code style and readability
      - Missing error handling

      Fix any issues you find by editing the files directly.
    allowed_tools:
      - "read"
      - "edit"
      - "write"

  - id: "test"
    name: "Write Tests"
    system_prompt: |
      You are a test engineer. Write comprehensive tests for the
      codebase.
    user_prompt: |
      Previous stage summary: {{prior_summary}}

      Write unit tests covering all public functions. Use pytest.
      Run the tests with bash to verify they pass.
    allowed_tools:
      - "read"
      - "write"
      - "bash"
```

---

## Validation rules

Fujin validates your pipeline config before execution. The following rules are enforced:

**Errors** (prevent execution):
- `name` must not be empty
- `stages` must contain at least one stage
- All stage `id` values must be unique
- Each stage must have non-empty `id`, `name`, `system_prompt`, and `user_prompt`

**Warnings** (reported but don't prevent execution):
- Unknown tool names in `allowed_tools` (valid: `read`, `write`, `bash`, `edit`, `glob`, `grep`, `notebook`)
- First stage references `{{prior_summary}}` (will be empty)

Validate without running:

```
fujin validate -c pipeline.yaml
```

---

## Checkpoints and resuming

Fujin saves a checkpoint after each stage completes. If a stage fails (agent error, network issue, etc.), you can resume from where it left off:

```
fujin run -c pipeline.yaml --resume
```

Resume reloads the checkpoint and skips already-completed stages. The config must match the original run (validated by hash).

Manage checkpoints:

```
fujin checkpoint list
fujin checkpoint show <run-id>
fujin checkpoint clean
```

---

## Tips for effective pipelines

**Keep stages focused.** Each stage should have a single, clear responsibility. A "design then implement then test" pipeline works better than one stage that tries to do everything.

**Use the right model for the job.** Opus for complex architecture decisions, Sonnet for general coding, Haiku for simple mechanical tasks like formatting or summarization.

**Restrict tools intentionally.** A design stage doesn't need `bash`. A review stage doesn't need `write` if it should only report findings. Limiting tools keeps agents focused and prevents unintended side effects.

**Write specific system prompts.** "You are a senior Rust developer who follows the project's existing patterns" is much better than "You are helpful." Include constraints and expectations.

**Use `{{prior_summary}}` in later stages.** This gives the agent context about what happened before without flooding the prompt with raw output. For more detail, use `{{artifact_list}}` or `{{all_artifacts}}`.

**Test with `--dry-run` first.** Verify your config parses correctly and the stages look right before spending API credits:

```
fujin run -c pipeline.yaml --dry-run
```

**Use variables for reusability.** Put project-specific values in `variables` so the same pipeline structure can be reused across projects with `--var` overrides.
