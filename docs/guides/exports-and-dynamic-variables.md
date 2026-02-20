# Exports and Dynamic Variables

This guide covers the **exports** feature — how to let agents discover information at runtime and pass it to downstream stages as template variables. This makes pipelines adaptive: instead of hardcoding values in YAML, the pipeline figures things out on its own.

## The problem exports solve

Consider a pipeline that generates code. You define the language and framework in `variables`:

```yaml
variables:
  language: "rust"
  framework: "actix-web"
```

This works when you know the project details upfront. But what if:
- The pipeline should auto-detect the existing tech stack?
- A discovery stage determines which database to use based on project requirements?
- The build command varies depending on what the analysis stage finds?

Exports let an agent write values at runtime that downstream stages use as template variables.

## How exports work

1. You add `exports` to a stage config and reference `{{exports_file}}` in the prompt
2. Fujin auto-generates a unique file path and injects it as `{{exports_file}}`
3. The agent writes a flat JSON object to that path during execution
4. After the stage completes, fujin reads the JSON and merges its key-value pairs into the pipeline's template variables
5. All downstream stages can use the exported values as `{{variable_name}}`

The exports file is stored in the platform data directory (not your repository), so it won't pollute your workspace.

## Basic example

```yaml
name: "Auto-Detect and Implement"

stages:
  - id: "detect"
    name: "Detect Tech Stack"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a project analyst. Examine the repository to determine
      the technology stack.
    user_prompt: |
      Analyze this project. Determine:
      - Primary programming language
      - Web framework (if any)
      - Package manager command
      - Test command

      Write your findings as a flat JSON object to {{exports_file}}:
      {"language": "rust", "framework": "actix-web", "pkg_manager": "cargo", "test_cmd": "cargo test"}

      Only include string values. Use your actual findings, not the example values above.
    allowed_tools: ["read", "glob", "grep"]
    exports:
      keys: [language, framework, pkg_manager, test_cmd]
  - id: "implement"
    name: "Add Feature"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an expert {{language}} developer familiar with {{framework}}.
    user_prompt: |
      Add a health check endpoint to this {{framework}} project.
      Follow the existing code patterns.
    allowed_tools: ["read", "write", "edit"]
  - id: "test"
    name: "Run Tests"
    commands:
      - "{{test_cmd}}"
```

After the `detect` stage writes `{"language": "rust", "framework": "actix-web", ...}`, the `implement` stage's system prompt becomes _"You are an expert rust developer familiar with actix-web"_ and the `test` command becomes `cargo test`.

## The `exports` config field

```yaml
exports:
  keys: [language, framework]    # Optional: expected keys
```

| Field | Required | Description |
|-------|----------|-------------|
| `keys` | No | A list of expected key names. If any are missing from the JSON, fujin emits a warning (but continues). |

If you omit `keys`, no validation is performed — any keys in the JSON are accepted.

The mere presence of `exports: {}` (even with empty keys) enables the feature for that stage — fujin generates the `{{exports_file}}` path and reads the file after the stage.

## Writing the exports file

The agent must write a **flat JSON object with string values** to `{{exports_file}}`:

```json
{"language": "python", "framework": "flask", "python_version": "3.12"}
```

### Prompt patterns that work

**Explicit example with clear instructions:**

```yaml
user_prompt: |
  Analyze the project structure.

  After your analysis, write a flat JSON object to {{exports_file}} with these keys:
  - "language": the primary programming language
  - "framework": the web framework, or "none"
  - "test_cmd": the command to run tests

  Example: {"language": "go", "framework": "chi", "test_cmd": "go test ./..."}
```

**Structured task with exports as the final step:**

```yaml
user_prompt: |
  Examine this repository and determine:
  1. What language is it written in?
  2. What build system does it use?
  3. What is the entry point?

  Summarize your findings, then write them as JSON to {{exports_file}}:
  {"language": "...", "build_system": "...", "entry_point": "..."}
```

### What doesn't work

- **Nested objects**: `{"db": {"host": "localhost", "port": 5432}}` — only flat key-value pairs are supported
- **Non-string values**: `{"port": 5432}` — values must be strings: `{"port": "5432"}`
- **Multiple JSON writes**: Only the final state of the file is read. Write once at the end.
- **Forgetting to mention `{{exports_file}}`**: If you don't tell the agent the path, it can't write the file

## What happens when exports fail

Exports are designed to be resilient. Failure doesn't crash the pipeline:

| Scenario | Behavior |
|----------|----------|
| Agent doesn't write the file | Warning emitted, pipeline continues. Missing variables render as empty strings. |
| JSON is malformed | Warning emitted, no variables imported. |
| Some `keys` are missing | Warning for each missing key, found keys are still imported. |
| Extra keys not in `keys` | Accepted silently. `keys` is a minimum set, not an exhaustive list. |

This means you can use exports opportunistically — if the agent discovers the information, great; if not, the pipeline degrades gracefully.

## Exports override YAML variables

If a YAML variable and an exported variable have the same name, the **export wins**. This is intentional — it lets you define defaults in `variables` that get overridden by runtime discovery:

```yaml
variables:
  language: "python"        # default assumption
  framework: "django"       # default assumption

stages:
  - id: "detect"
    name: "Detect Stack"
    user_prompt: |
      Detect the actual tech stack and write to {{exports_file}}.
    exports:
      keys: [language, framework]
  - id: "implement"
    name: "Implement"
    system_prompt: "You are a {{language}} developer using {{framework}}."
    # If detect succeeds: uses actual detected values
    # If detect fails (no file written): falls back to YAML defaults
```

## Multiple stages can export

Later exports override earlier ones for the same key:

```yaml
stages:
  - id: "detect"
    exports:
      keys: [language, framework]
    # Writes: {"language": "rust", "framework": "actix-web"}
  - id: "analyze"
    exports:
      keys: [framework, db_type]
    # Writes: {"framework": "axum", "db_type": "postgres"}
    # Now framework="axum" (overrides "actix-web"), db_type="postgres"
  - id: "implement"
    # Sees: language="rust", framework="axum", db_type="postgres"
```

## Exports in command stages

Exported variables work in command stages just like prompt templates:

```yaml
stages:
  - id: "detect"
    name: "Detect Stack"
    system_prompt: "You are a project analyst."
    user_prompt: |
      Determine the build command and test command for this project.
      Write to {{exports_file}}:
      {"build_cmd": "...", "test_cmd": "...", "lint_cmd": "..."}
    exports:
      keys: [build_cmd, test_cmd, lint_cmd]
  - id: "build"
    name: "Build and Test"
    commands:
      - "{{build_cmd}}"
      - "{{test_cmd}}"
      - "{{lint_cmd}}"
```

Note: adding `exports` to a command stage itself doesn't make sense (there's no agent to write the file), and fujin will warn you about it.

## Realistic example: Adaptive migration pipeline

A pipeline that analyzes a legacy codebase, determines the migration strategy, and executes it:

```yaml
name: "Legacy Code Migration"

variables:
  target_language: "typescript"
  migration_goal: "Migrate the Flask REST API to Express.js with TypeScript"

stages:
  - id: "survey"
    name: "Survey Existing Codebase"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a software architect doing a codebase assessment.
      Be thorough — count endpoints, identify dependencies, note
      patterns and anti-patterns.
    user_prompt: |
      Survey this codebase for migration planning.
      Migration goal: {{migration_goal}}

      Analyze:
      - Number of API endpoints and their HTTP methods
      - Database access patterns (ORM, raw SQL, etc.)
      - External service integrations
      - Authentication/authorization approach
      - Configuration management

      After your analysis, write a summary to {{exports_file}}:
      {
        "source_language": "python",
        "source_framework": "flask",
        "endpoint_count": "14",
        "db_layer": "sqlalchemy",
        "auth_method": "jwt",
        "complexity": "medium"
      }
    allowed_tools: ["read", "glob", "grep"]
    exports:
      keys: [source_language, source_framework, endpoint_count, db_layer, auth_method, complexity]
  - id: "scaffold"
    name: "Scaffold Target Project"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a {{target_language}} developer setting up a new project.
      Mirror the architecture of the existing {{source_framework}} app
      using {{target_language}} idioms.
    user_prompt: |
      Survey results: {{prior_summary}}

      Set up the {{target_language}} project structure:
      - Initialize with appropriate package manager
      - Set up the equivalent of {{db_layer}} (use an appropriate ORM)
      - Configure {{auth_method}} authentication
      - Create placeholder route files for all {{endpoint_count}} endpoints
      - Set up test infrastructure

      Don't implement business logic yet — just create the skeleton.
    allowed_tools: ["read", "write", "bash"]

  - id: "migrate"
    name: "Migrate Business Logic"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a migration specialist. Translate {{source_language}}/{{source_framework}}
      code to {{target_language}} while preserving all business logic.
      Pay special attention to error handling and edge cases.
    user_prompt: |
      Survey: {{stages.survey.summary}}
      Scaffold: {{prior_summary}}

      Now migrate each endpoint's business logic from the
      {{source_framework}} app to the new {{target_language}} project.
      Read the original implementation, understand the logic,
      and rewrite it in {{target_language}}.

      Work through endpoints methodically. Run tests as you go.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "verify"
    name: "Verify Migration"
    commands:
      - "npm test 2>&1"
      - "npm run lint 2>&1"
```

The `survey` stage discovers `source_language`, `source_framework`, `db_layer`, `auth_method`, etc. All downstream stages use these to tailor their work to the actual codebase instead of relying on hardcoded assumptions.

## Realistic example: Dynamic CI pipeline

A pipeline that adapts its build and test steps based on what it discovers:

```yaml
name: "Adaptive CI"

stages:
  - id: "detect"
    name: "Detect Project Config"
    model: "claude-sonnet-4-6"
    system_prompt: "You are a build system analyst."
    user_prompt: |
      Examine this repository and determine the build configuration.
      Look at package files, makefiles, CI configs, and project structure.

      Write your findings to {{exports_file}}:
      {
        "build_cmd": "the command to build the project",
        "test_cmd": "the command to run tests",
        "lint_cmd": "the command to run linting",
        "has_docker": "yes or no",
        "node_version": "version number or n/a"
      }
    allowed_tools: ["read", "glob"]
    exports:
      keys: [build_cmd, test_cmd, lint_cmd, has_docker, node_version]
  - id: "build-and-test"
    name: "Build and Test"
    commands:
      - "{{build_cmd}} 2>&1"
      - "{{test_cmd}} 2>&1"
      - "{{lint_cmd}} 2>&1"
  - id: "fix-issues"
    name: "Fix Any Failures"
    system_prompt: |
      You are a developer. Fix build errors, test failures,
      and lint warnings.
    user_prompt: |
      Build/test output: {{prior_summary}}

      Fix all failures and verify with:
      {{build_cmd}}
      {{test_cmd}}
    allowed_tools: ["read", "edit", "bash"]
```

## Best practices

**Be explicit about the JSON format.** Show the agent an example of what to write. Agents follow examples reliably.

**Use `keys` for documentation and validation.** Even though they're optional, listing expected keys makes the pipeline self-documenting and gives you warnings when things go wrong.

**Provide YAML defaults as fallbacks.** Define reasonable defaults in `variables` so the pipeline works even if the export fails.

**Keep exports flat and simple.** String key-value pairs only. If you need structured data, have the agent write a separate file and reference it in prompts.

**One export stage per concern.** Don't overload a single stage with discovering everything. A `detect-stack` stage and a separate `detect-config` stage are clearer than one stage trying to find 20 different values.

## What to read next

- **[Pipeline Patterns](pipeline-patterns.md)** — Full pipeline recipes using exports and other features
- **[Pipeline Authoring Reference](../pipeline-authoring.md)** — Complete field reference
