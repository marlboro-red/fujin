# Pipeline Authoring Guide

This document covers everything you need to know to create fujin pipeline configuration files.

## Overview

A fujin pipeline is a YAML file that defines a sequence of **stages**. Each stage invokes an agent with a specific prompt, model, and set of tools. Stages execute sequentially — the output of one stage feeds into the next through automatic summarization and file change tracking. Agents always run in the directory where `fujin` was invoked.

Fujin supports multiple agent runtimes — Claude Code (default) and GitHub Copilot CLI — which can be configured per-pipeline or per-stage.

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
runtime: "claude-code"             # optional, default: "claude-code"
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

#### `runtime` (optional, string)

The default agent runtime for all stages. Default: `"claude-code"`.

Available runtimes:

| Runtime | Description |
|---------|-------------|
| `claude-code` | Claude Code CLI — structured JSON output, streaming progress, token tracking |
| `copilot-cli` | GitHub Copilot CLI — programmatic mode (`copilot -p -s`), plain text output |

Individual stages can override this with their own `runtime` field (see [Per-stage runtime selection](#per-stage-runtime-selection)).

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
    runtime: "copilot-cli"                 # optional — overrides pipeline default
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

#### `runtime` (optional, string)

Override the pipeline-level runtime for this specific stage. When not set, the stage uses the pipeline's `runtime` value. See [Per-stage runtime selection](#per-stage-runtime-selection) for details.

#### `model` (optional, string)

The model to use for this stage. Default: `"claude-sonnet-4-6"`.

Model names differ between runtimes. Use the names appropriate for the stage's runtime:

**Claude Code models:**

| Model | Best for |
|-------|----------|
| `claude-opus-4-6` | Complex reasoning, architecture design, difficult tasks |
| `claude-sonnet-4-6` | General-purpose coding, good balance of quality and speed |
| `claude-haiku-4-5-20251001` | Fast, cheap tasks — summaries, simple edits, validation |

**Copilot CLI models:**

| Model | Best for |
|-------|----------|
| `claude-sonnet-4` | General-purpose coding (Copilot CLI default) |
| `claude-haiku-4.5` | Fast, cheap tasks |
| `gpt-5` | OpenAI model available through Copilot |

You can mix models across stages. Use a powerful model for complex design work and a cheaper model for mechanical tasks.

#### `system_prompt` (required, string)

Sets the agent's persona and behavioral constraints. With Claude Code, this is sent as the system prompt via `--append-system-prompt`. With Copilot CLI (which has no system prompt flag), it is automatically embedded into the user prompt.

Tips:
- Be specific about the agent's role: `"You are a senior Rust developer"` not `"You are helpful"`
- Include constraints: `"Only modify files in the src/ directory"`
- Mention output expectations: `"Create a file at docs/architecture.md"`
- System prompts support `{{variables}}` too

#### `user_prompt` (required, string)

The task prompt sent to the agent. This is where you describe what the stage should accomplish. Supports Handlebars template syntax with `{{variable_name}}`.

#### `allowed_tools` (optional, list of strings)

Controls which tools the agent can use in this stage. Default: `["read", "write"]`.

Use the same tool names regardless of runtime — fujin maps them to the correct runtime-specific names automatically:

| Tool | Claude Code | Copilot CLI | Description |
|------|-------------|-------------|-------------|
| `read` | `Read` | `Read` | Read file contents |
| `write` | `Write` | `Write` | Create or overwrite files |
| `edit` | `Edit` | `Edit` | Make targeted edits to existing files |
| `bash` | `Bash` | `shell` | Run shell commands |
| `glob` | `Glob` | `Glob` | Find files by pattern |
| `grep` | `Grep` | `Grep` | Search file contents |
| `notebook` | `NotebookEdit` | `NotebookEdit` | Edit Jupyter notebooks |

Unknown tool names are passed through to the runtime unchanged.

Restricting tools is useful for safety and focus:
- A "design" stage might only need `read` + `write` (no shell access)
- A "build and test" stage needs `bash` to run compilers and test suites
- A "review" stage might only need `read` to inspect code

#### `when` (optional, object)

Gates stage execution on a prior stage's output. If the condition is not met, the stage is skipped entirely.

```yaml
when:
  stage: "analyze"              # ID of a previously completed stage
  output_matches: "NEEDS_TESTS" # regex pattern (case-insensitive)
```

| Field | Type | Description |
|-------|------|-------------|
| `stage` | string | Stage ID whose response text to check. Must reference an earlier stage. |
| `output_matches` | string | Regex pattern matched case-insensitively against the referenced stage's full response text. |

#### `branch` (optional, object)

Adds an AI-driven branch classifier that runs after the stage completes. The classifier reads the stage's output and selects one of the named routes. Downstream stages with matching `on_branch` values will execute; others are skipped.

```yaml
branch:
  model: "claude-haiku-4-5-20251001"  # optional, defaults to summarizer model
  prompt: "Classify the work needed"
  routes: [frontend, backend, fullstack]
  default: fullstack                   # optional fallback
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | string | summarizer model | Model for the classifier call. |
| `prompt` | string | *required* | Prompt sent to the classifier along with the stage's output. |
| `routes` | list | *required* | Valid route names the classifier can select. Must be non-empty. |
| `default` | string | — | Fallback route if the classifier output doesn't match any route name. |

A stage cannot have both `branch` and `on_branch`.

#### `on_branch` (optional, string or list)

Restricts the stage to only execute when a matching branch route was selected by an upstream `branch` classifier. Accepts a single string or a list (OR semantics):

```yaml
on_branch: frontend                    # runs if "frontend" was selected
on_branch: [frontend, fullstack]       # runs if either was selected
```

Stages without `on_branch` always run, making them natural convergence points after branched sections.

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
| `{{stages.<id>.summary}}` | After stage `<id>` completes | Summary of a specific completed stage, referenced by its `id` |
| `{{stages.<id>.response}}` | After stage `<id>` completes | Full response text of a specific completed stage |

Using `{{prior_summary}}` in the first stage produces a validation warning since there is no prior stage.

The `{{stages.<id>.*}}` variables are especially useful in branching pipelines where `{{prior_summary}}` might refer to a skipped stage. You can explicitly reference the stage you care about:

```yaml
user_prompt: |
  Based on the analysis: {{stages.analyze.summary}}
  Build the frontend components.
```

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

## Conditional execution

By default, every stage in a pipeline executes sequentially. **Conditional execution** lets you skip stages based on prior results, enabling branching workflows while preserving the linear execution model — stages are still iterated top-to-bottom, but some may be skipped.

There are two complementary mechanisms:

### `when` — regex gating

The simplest form of conditional execution. A `when` condition checks a prior stage's response text against a regex pattern. If it doesn't match, the stage is skipped.

```yaml
stages:
  - id: analyze
    name: Analyze Codebase
    system_prompt: "You are a code analyzer."
    user_prompt: |
      Analyze the codebase and determine if it has adequate test coverage.
      End your response with exactly one of:
      VERDICT: NEEDS_TESTS
      VERDICT: TESTS_OK

  - id: write-tests
    name: Write Missing Tests
    when:
      stage: analyze
      output_matches: "NEEDS_TESTS"
    system_prompt: "You are a test engineer."
    user_prompt: |
      Based on the analysis: {{stages.analyze.summary}}
      Write tests for the uncovered code paths.
    allowed_tools: ["read", "write", "bash"]

  - id: lint
    name: Lint and Format
    # No `when` — always runs regardless of the analyze verdict
    system_prompt: "You are a code quality engineer."
    user_prompt: "Run linting and formatting on the project."
    commands:
      - "cargo fmt"
      - "cargo clippy -- -D warnings"
```

Key points:
- `when.stage` must reference a stage that appears *earlier* in the stages list
- `when.output_matches` is a regex matched case-insensitively against the stage's full response text
- Skipped stages don't produce output — `{{prior_summary}}` in the next stage refers to the last *executed* stage
- Use `{{stages.<id>.summary}}` to explicitly reference a specific stage's output

### `branch`/`on_branch` — AI-driven routing

For more sophisticated routing, `branch` uses an AI classifier to pick a named route after a stage completes. Downstream stages tagged with `on_branch` only run if their route was selected.

```yaml
stages:
  - id: analyze
    name: Analyze Requirements
    system_prompt: "You are a senior engineer."
    user_prompt: "Analyze the requirements in SPEC.md"
    branch:
      prompt: "Based on the analysis, classify the primary work needed"
      routes: [frontend, backend, fullstack]
      default: fullstack

  - id: frontend-impl
    name: Frontend Development
    on_branch: frontend
    system_prompt: "You are a frontend developer."
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Build the frontend components.
    allowed_tools: ["read", "write", "bash"]

  - id: backend-impl
    name: Backend Development
    on_branch: backend
    system_prompt: "You are a backend developer."
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Build the backend services.
    allowed_tools: ["read", "write", "bash"]

  - id: fullstack-impl
    name: Full Stack Development
    on_branch: fullstack
    system_prompt: "You are a full-stack developer."
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Build both frontend and backend.
    allowed_tools: ["read", "write", "bash"]

  - id: review
    name: Code Review
    # No on_branch = always runs (convergence point)
    system_prompt: "You are a code reviewer."
    user_prompt: |
      Review all changes made in this pipeline.
    allowed_tools: ["read"]
```

Key points:
- The `branch` classifier runs as a separate AI call after the stage completes — it reads the stage's output and selects a route
- `on_branch` accepts a single string (`on_branch: frontend`) or a list (`on_branch: [frontend, fullstack]`) with OR semantics
- Stages without `on_branch` always execute — use them as convergence points after branched sections
- A stage cannot have both `branch` and `on_branch`
- `branch.default` provides a fallback if the classifier output is ambiguous
- Branch selections are saved in checkpoints, so `--resume` correctly replays the same routing decisions

### Combining `when` and `branch`

You can use both mechanisms in the same pipeline. `when` is best for simple binary decisions based on structured output (like a PASS/FAIL verdict), while `branch`/`on_branch` is better for multi-way routing where you want an AI to make the classification decision.

---

## Agent runtimes

Fujin supports pluggable agent runtimes through its `AgentRuntime` trait. Each runtime wraps a different CLI tool as a subprocess backend for executing pipeline stages.

### Claude Code (default)

The default runtime. Uses `claude --print` with structured JSON output.

- Full token usage tracking
- Streaming tool-use activity in the TUI (shows which tools the agent is using in real time)
- System prompt sent via `--append-system-prompt`
- Requires an Anthropic API key or Claude Code authentication

### GitHub Copilot CLI

An alternative runtime using `copilot -p -s` (programmatic + silent mode).

- No token usage tracking (always reports `None`)
- No streaming progress — the TUI shows a generic "Agent working..." message instead of per-tool activity
- System prompt is embedded into the user prompt (Copilot CLI has no system prompt flag)
- Uses `--yolo` flag to skip interactive permission prompts
- Requires GitHub authentication (`GH_TOKEN` / `GITHUB_TOKEN` / GitHub OAuth) with a Copilot subscription

To use Copilot CLI, set `runtime: "copilot-cli"` at the pipeline level or on individual stages.

### Per-stage runtime selection

You can override the runtime on a per-stage basis. This lets you use different runtimes (and different models) for different stages in the same pipeline:

```yaml
name: "Mixed Runtime Pipeline"
runtime: "claude-code"              # default for all stages

stages:
  - id: "design"
    name: "Design Architecture"
    model: "claude-opus-4-6"        # Claude Code model name
    system_prompt: "You are a senior architect."
    user_prompt: "Design the system."
    allowed_tools: ["read", "write"]

  - id: "implement"
    name: "Implement Code"
    runtime: "copilot-cli"          # override: use Copilot CLI
    model: "gpt-5"                  # Copilot CLI model name
    system_prompt: "You are an expert developer."
    user_prompt: |
      Previous stage summary: {{prior_summary}}
      Implement the design.
    allowed_tools: ["read", "write", "bash"]

  - id: "review"
    name: "Code Review"
    model: "claude-sonnet-4-6"      # back to Claude Code (pipeline default)
    system_prompt: "You are a code reviewer."
    user_prompt: |
      Previous stage summary: {{prior_summary}}
      Review the implementation.
    allowed_tools: ["read"]
```

### Runtime feature comparison

| Feature | Claude Code | Copilot CLI |
|---------|-------------|-------------|
| Token usage tracking | Yes | No |
| Streaming tool activity | Yes | No (generic spinner) |
| System prompt | Dedicated flag | Inlined into prompt |
| Structured JSON output | Yes | Plain text |
| Authentication | Anthropic API key | GitHub OAuth / `GH_TOKEN` |
| Billing | Anthropic API usage | GitHub Copilot subscription |

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

### Copilot CLI pipeline

A pipeline using GitHub Copilot CLI as the runtime:

```yaml
name: "Copilot Code Generator"
runtime: "copilot-cli"

variables:
  language: "typescript"
  description: "A REST API with Express"

stages:
  - id: "codegen"
    name: "Generate Code"
    model: "claude-sonnet-4"
    system_prompt: |
      You are an expert {{language}} developer.
    user_prompt: |
      Create a {{language}} project: {{description}}
    allowed_tools:
      - "write"
      - "read"
      - "bash"

  - id: "test"
    name: "Write Tests"
    model: "claude-sonnet-4"
    system_prompt: |
      You are a test engineer.
    user_prompt: |
      Previous stage summary: {{prior_summary}}
      Write comprehensive tests for the project.
    allowed_tools:
      - "read"
      - "write"
      - "bash"
```

### Branching pipeline with AI routing

A pipeline that analyzes requirements and routes to different implementation paths:

```yaml
name: "Smart Project Builder"
variables:
  description: "Build a dashboard for monitoring API performance"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 1024

stages:
  - id: analyze
    name: Analyze Requirements
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior software architect. Analyze requirements
      and determine the best implementation approach.
    user_prompt: |
      Analyze these requirements: {{description}}
      Determine the primary work needed.
    allowed_tools: ["read"]
    branch:
      prompt: "Based on the analysis, what type of work is primarily needed?"
      routes: [frontend, backend, fullstack]
      default: fullstack

  - id: frontend-impl
    name: Frontend Development
    on_branch: frontend
    system_prompt: "You are a frontend developer specializing in React and TypeScript."
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Build the frontend components for: {{description}}
    allowed_tools: ["read", "write", "bash"]

  - id: backend-impl
    name: Backend Development
    on_branch: backend
    system_prompt: "You are a backend developer specializing in APIs and databases."
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Build the backend services for: {{description}}
    allowed_tools: ["read", "write", "bash"]

  - id: fullstack-impl
    name: Full Stack Development
    on_branch: fullstack
    model: "claude-opus-4-6"
    system_prompt: "You are a full-stack developer."
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Build both frontend and backend for: {{description}}
    allowed_tools: ["read", "write", "bash"]
    max_turns: 30

  - id: test
    name: Run Tests
    # No on_branch — always runs after the implementation stage
    commands:
      - "npm test"
```

### Conditional stage with `when`

A pipeline that conditionally writes tests based on analysis:

```yaml
name: "Conditional Test Writer"
stages:
  - id: analyze
    name: Analyze Coverage
    system_prompt: "You are a test coverage analyzer."
    user_prompt: |
      Analyze the test coverage of this project.
      End your response with exactly:
      VERDICT: NEEDS_TESTS or VERDICT: ADEQUATE
    allowed_tools: ["read", "bash"]

  - id: write-tests
    name: Write Tests
    when:
      stage: analyze
      output_matches: "NEEDS_TESTS"
    system_prompt: "You are a test engineer."
    user_prompt: |
      Coverage analysis: {{stages.analyze.summary}}
      Write tests to improve coverage.
    allowed_tools: ["read", "write", "bash"]

  - id: format
    name: Format Code
    commands:
      - "cargo fmt"
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
- `when.stage` must reference a stage ID that appears earlier in the stages list
- `when.output_matches` must be a valid regex
- `branch.routes` must be non-empty
- `branch.default` (if set) must be one of the defined routes
- `on_branch` values must match a route defined in an earlier stage's `branch`
- A stage cannot have both `branch` and `on_branch`

**Warnings** (reported but don't prevent execution):
- Unknown tool names in `allowed_tools` (valid: `read`, `write`, `bash`, `edit`, `glob`, `grep`, `notebook`)
- Unknown runtime name (valid: `claude-code`, `copilot-cli`)
- First stage references `{{prior_summary}}` (will be empty)
- A `branch` route has no downstream `on_branch` stage that references it

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

**Choose the right runtime.** Claude Code provides richer integration (streaming progress, token tracking), while Copilot CLI gives access to models like GPT-5 through a GitHub Copilot subscription. You can mix runtimes in the same pipeline — use per-stage `runtime` overrides to leverage the strengths of each.

**Use runtime-appropriate model names.** Claude Code and Copilot CLI use different model name formats. Double-check that the `model` field matches the runtime that will execute that stage.

**Use `when` for simple binary gates.** If a stage should only run when the previous output contains a specific keyword or verdict, `when` with a regex is simpler and cheaper than `branch` (no extra AI call). Reserve `branch`/`on_branch` for multi-way routing where you want the AI to make a nuanced classification.

**Design convergence points after branches.** Stages without `on_branch` always run, making them natural points where branched paths converge. Place review, testing, or deployment stages after your branch section without `on_branch` so they run regardless of which route was taken.

**Use `{{stages.<id>.summary}}` in branching pipelines.** When stages can be skipped, `{{prior_summary}}` might not refer to the stage you expect. Use the explicit `{{stages.<id>.summary}}` or `{{stages.<id>.response}}` variables to reference specific stages by ID.
