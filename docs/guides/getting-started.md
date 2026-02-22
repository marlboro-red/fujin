# Getting Started with Fujin Pipelines

This guide walks you through creating, validating, and running your first fujin pipeline. By the end, you'll have a working pipeline that scaffolds a project, writes tests, and formats the output.

## Prerequisites

- Fujin installed (`cargo install --path crates/fujin-cli`)
- At least one agent runtime:
  - **Claude Code**: `npm install -g @anthropic-ai/claude-code`
  - **GitHub Copilot CLI**: `npm install -g @github/copilot`

Verify your setup:

```bash
fujin agents --check
```

## Step 1: Understand the mental model

A fujin pipeline is a YAML file that describes a sequence of **stages**. Each stage spawns an AI agent (or runs shell commands) in your current directory. Stages run one at a time, top to bottom. Between stages, fujin:

1. Summarizes what the previous stage produced
2. Tracks which files were created, modified, or deleted
3. Passes that context to the next stage via template variables

The agents do the actual work — reading files, writing code, running commands. Fujin orchestrates the sequencing, context passing, and checkpointing.

## Step 2: Create the pipeline file

Let's build a pipeline that generates a Python CLI tool. Create a file called `pipeline.yaml` in an empty directory:

```yaml
name: "Python CLI Generator"

variables:
  project_name: "logparse"
  description: "A CLI tool that parses nginx access logs and outputs a summary report with top endpoints, status code distribution, and requests per hour"

stages:
  - id: "implement"
    name: "Implement the CLI"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior Python developer. You write clean, well-structured
      code using standard library modules where possible. You use argparse
      for CLI argument parsing and follow PEP 8.
    user_prompt: |
      Create a Python project called "{{project_name}}" that does the following:

      {{description}}

      Requirements:
      - Single-file CLI at {{project_name}}/main.py
      - Accept a log file path as a positional argument
      - Support --top-n flag (default 10) to control how many top endpoints to show
      - Print a clean, formatted report to stdout
      - Include a sample nginx log file at tests/sample.log for testing
      - Include a pyproject.toml with project metadata
    allowed_tools:
      - "write"
      - "read"
      - "bash"
```

### What each field does

- **`name`**: Displayed in the TUI and CLI output. Pick something descriptive.
- **`variables`**: Key-value pairs you can reference in prompts as `{{key}}`. This makes your pipeline reusable — change the variables, get a different project.
- **`stages`**: The ordered list of stages. We start with just one.

### Stage anatomy

- **`id`**: A unique slug for the stage. Used in checkpoints and context references.
- **`name`**: Human-readable label shown in the TUI.
- **`model`**: Which model to use. Sonnet is a good default for coding tasks.
- **`system_prompt`**: Sets the agent's persona. Be specific about language, style, and constraints.
- **`user_prompt`**: The actual task. Uses `{{variables}}` for templating.
- **`allowed_tools`**: What the agent can do. We give it `write` (create files), `read` (inspect files), and `bash` (run commands like `python`).

## Step 3: Validate the config

Before running anything, validate:

```bash
fujin validate -c pipeline.yaml
```

This checks for structural errors (missing fields, duplicate stage IDs, invalid tool names) and prints warnings about potential issues.

## Step 4: Preview with dry-run

See the execution plan without spending API credits:

```bash
fujin run -c pipeline.yaml --dry-run
```

This shows you the stages in order, the models they'll use, and the rendered prompts (with variables substituted).

## Step 5: Run it

```bash
fujin run -c pipeline.yaml
```

Fujin will:
1. Render the templates (replacing `{{project_name}}` and `{{description}}`)
2. Spawn a Claude Code agent with the system prompt and user prompt
3. Stream progress in the terminal — you'll see which tools the agent uses in real time
4. When the stage completes, save a checkpoint and report the results

After the run, you should see a `logparse/` directory with the project files.

## Step 6: Add a second stage

A single stage works, but fujin's real power is multi-stage pipelines where each stage builds on the previous one. Let's add a test-writing stage:

```yaml
name: "Python CLI Generator"

variables:
  project_name: "logparse"
  description: "A CLI tool that parses nginx access logs and outputs a summary report with top endpoints, status code distribution, and requests per hour"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 1024

stages:
  - id: "implement"
    name: "Implement the CLI"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior Python developer. You write clean, well-structured
      code using standard library modules where possible. You use argparse
      for CLI argument parsing and follow PEP 8.
    user_prompt: |
      Create a Python project called "{{project_name}}" that does the following:

      {{description}}

      Requirements:
      - Single-file CLI at {{project_name}}/main.py
      - Accept a log file path as a positional argument
      - Support --top-n flag (default 10) to control how many top endpoints to show
      - Print a clean, formatted report to stdout
      - Include a sample nginx log file at tests/sample.log for testing
      - Include a pyproject.toml with project metadata
    allowed_tools:
      - "write"
      - "read"
      - "bash"

  - id: "test"
    name: "Write Tests"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a test engineer who writes thorough pytest test suites.
      You test edge cases, not just happy paths. You use fixtures and
      parametrize where appropriate.
    user_prompt: |
      Previous stage summary: {{prior_summary}}

      Files created: {{artifact_list}}

      Write a comprehensive pytest test suite for the {{project_name}} project.
      Put tests in tests/test_main.py. Cover:
      - Parsing individual log lines (valid and malformed)
      - The --top-n flag
      - Empty log file handling
      - The summary report output format

      Run the tests with `pytest -v` to make sure they pass.
    allowed_tools:
      - "read"
      - "write"
      - "bash"
```

### What's new

- **`summarizer`**: Controls how stage output is condensed for the next stage. Haiku is fast and cheap — it just needs to summarize what happened.
- **`{{prior_summary}}`**: The second stage gets a summary of what the first stage did, so it understands the context.
- **`{{artifact_list}}`**: A list of files the first stage created or modified. The test stage can see exactly what files exist.

## Step 7: Add a command stage

Not every step needs an AI. Let's add a formatting step using shell commands:

```yaml
  - id: "format"
    name: "Format and Lint"
    commands:
      - "cd {{project_name}} && python -m py_compile main.py"
```

Command stages run shell commands directly — no agent, no API call. They support `{{variables}}` in commands just like prompts do.

## Step 8: Override variables from the CLI

The same pipeline can generate different projects:

```bash
fujin run -c pipeline.yaml \
  --var project_name=sysmon \
  --var description="A CLI tool that monitors system CPU and memory usage, displaying a live-updating dashboard in the terminal"
```

CLI overrides take precedence over YAML values.

## Step 9: Resume after failure

If a stage fails (network error, timeout, agent mistake), don't start over:

```bash
fujin run -c pipeline.yaml --resume
```

Fujin reloads the last checkpoint and skips completed stages. This saves time and API costs.

Manage checkpoints:

```bash
fujin checkpoint list        # See all saved checkpoints
fujin checkpoint show <id>   # Inspect a specific checkpoint
fujin checkpoint clean       # Clear all checkpoints
```

## What to read next

- **[Multi-Stage Pipelines](multi-stage-pipelines.md)** — Designing effective stage sequences, choosing models, and passing context
- **[Branching and Conditions](branching-and-conditions.md)** — Conditional execution with `when` and `branch`/`on_branch`
- **[Exports and Dynamic Variables](exports-and-dynamic-variables.md)** — Letting agents set variables at runtime
- **[Pipeline Patterns](pipeline-patterns.md)** — Ready-to-use patterns for common workflows
- **[Best Practices](best-practices.md)** — Cost optimization, cleanup, prompt writing, and pipeline hygiene
- **[Pipeline Authoring Reference](../pipeline-authoring.md)** — Complete field reference and validation rules
