# Pipeline Patterns and Recipes

This guide is a collection of ready-to-use pipeline patterns for common workflows. Each pattern is a complete, realistic YAML config you can adapt to your project.

## Pattern: Implement → Test → Fix loop

A pipeline that writes code, runs tests, and fixes failures. The command stage in the middle gives deterministic feedback that the fix stage can act on.

```yaml
name: "Implement with Test Feedback"

variables:
  task: "Add pagination support to the GET /users endpoint with cursor-based pagination"

stages:
  - id: "implement"
    name: "Implement Feature"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior developer. Read the existing codebase to understand
      the patterns before making changes. Follow existing conventions for
      routing, error handling, and response formats.
    user_prompt: |
      Task: {{task}}

      Read the relevant source files, then implement the feature.
      Write tests alongside the implementation.
    allowed_tools: ["read", "write", "edit", "bash", "glob", "grep"]

  - id: "test"
    name: "Run Tests"
    commands:
      - "cargo test 2>&1"
      - "cargo clippy -- -D warnings 2>&1"
  - id: "fix"
    name: "Fix Failures"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a developer fixing build errors and test failures.
      Read the error output carefully and make targeted fixes.
      Do not rewrite large sections — make minimal changes to fix
      the specific issues.
    user_prompt: |
      Test output: {{prior_summary}}

      Fix all compilation errors, test failures, and clippy warnings.
      Run `cargo test` and `cargo clippy` after each fix to verify.
    allowed_tools: ["read", "edit", "bash"]

```

**When to use:** Any time you want deterministic build/test feedback between implementation and refinement. The command stage is free (no API call) and gives the fix stage exact error messages to work with.

## Pattern: Code review pipeline

Generate code and then have it reviewed by a different model/persona. The reviewer edits the code directly.

```yaml
name: "Code with Review"

variables:
  language: "go"
  task: "Implement a concurrent-safe LRU cache with TTL support"

stages:
  - id: "implement"
    name: "Initial Implementation"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a {{language}} developer. Write clean, idiomatic code
      with proper error handling and documentation.
    user_prompt: |
      {{task}}

      Create a well-structured package with:
      - The core implementation
      - Unit tests
      - A usage example in cmd/example/main.go
      - A README.md
    allowed_tools: ["write", "read", "bash"]

  - id: "review"
    name: "Senior Review"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a principal {{language}} engineer reviewing code from a
      mid-level developer. You focus on:
      - Race conditions and concurrency bugs
      - API design and ergonomics
      - Performance (unnecessary allocations, lock contention)
      - Edge cases in error handling
      - Test coverage gaps

      When you find an issue, fix it directly. Don't just describe it.
    user_prompt: |
      Review the implementation of: {{task}}

      Files to review:
      {{artifact_list}}

      Read each file, identify issues, and fix them. Then run the tests.
    allowed_tools: ["read", "edit", "bash"]

  - id: "verify"
    name: "Final Verification"
    commands:
      - "go test -race -count=1 ./... 2>&1"
      - "go vet ./... 2>&1"
      - "golangci-lint run 2>&1"
```

**Why Opus for review:** Code review benefits from deeper reasoning — catching subtle concurrency bugs, questioning API design choices, and spotting edge cases. Opus excels at this.

## Pattern: Spec → Implement → Document

A three-stage pipeline where an architect writes a spec, a developer implements it, and a writer documents it. Each specialist stays in their lane.

```yaml
name: "Spec-Driven Development"

variables:
  feature: "User invitation system — existing users can invite new users by email, with configurable role assignment and expiration"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 2048

stages:
  - id: "spec"
    name: "Write Technical Spec"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior software architect writing a technical spec.
      Read the existing codebase to understand patterns, then design
      the feature to fit naturally into the existing architecture.
      Be specific about data models, API contracts, and edge cases.
    user_prompt: |
      Feature: {{feature}}

      Read the existing codebase. Then create docs/spec.md covering:
      - Data model changes (exact schema)
      - API endpoints (method, path, request/response schemas)
      - Business rules and validation
      - Email delivery approach
      - Security considerations (token generation, expiration, rate limiting)
      - Migration steps
    allowed_tools: ["read", "write", "glob", "grep"]
  - id: "implement"
    name: "Implement from Spec"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a developer implementing a feature from a spec.
      Follow the spec exactly — don't improvise or add features
      not in the spec. If the spec is ambiguous, make the simplest
      choice.
    user_prompt: |
      Read the spec at docs/spec.md and implement everything it describes.
      Follow the existing codebase patterns for:
      - File organization
      - Error handling
      - Testing approach
      - Naming conventions

      Write tests for each API endpoint. Run the full test suite when done.
    allowed_tools: ["read", "write", "edit", "bash", "glob"]

  - id: "document"
    name: "Write User Docs"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a technical writer. You write documentation for
      developers who will use this API. Be clear, concise, and
      include practical examples. Don't repeat the spec — focus
      on how to use the feature.
    user_prompt: |
      Spec: {{stages.spec.summary}}
      Implementation: {{prior_summary}}

      Update the project's API documentation to cover the invitation system:
      - Add invitation endpoints to docs/API.md
      - Add an "Invitations" section to README.md
      - Include curl examples for common workflows
    allowed_tools: ["read", "write", "edit"]
```

## Pattern: Adaptive analysis with exports

A pipeline that discovers project properties and adapts its behavior accordingly. No hardcoded assumptions about the tech stack.

```yaml
name: "Auto-Adapt Pipeline"

stages:
  - id: "discover"
    name: "Discover Project"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a project analyst. Examine the repository structure,
      configuration files, and code to determine the full technology
      stack and project conventions.
    user_prompt: |
      Analyze this repository thoroughly. Determine:
      - Primary language and version
      - Framework and major dependencies
      - Build command
      - Test command
      - Linting command
      - Source directory
      - Test directory

      Write your findings as a flat JSON object to {{exports_file}}:
      {
        "language": "...",
        "framework": "...",
        "build_cmd": "...",
        "test_cmd": "...",
        "lint_cmd": "...",
        "src_dir": "...",
        "test_dir": "..."
      }
    allowed_tools: ["read", "glob", "grep"]
    exports:
      keys: [language, framework, build_cmd, test_cmd, lint_cmd, src_dir, test_dir]
  - id: "improve"
    name: "Improve Code Quality"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a senior {{language}} developer who knows {{framework}} well.
      Focus on practical improvements: fix real bugs, reduce complexity,
      improve error messages. Don't add unnecessary abstractions.
    user_prompt: |
      Discovery: {{prior_summary}}

      Read the source files in {{src_dir}} and the tests in {{test_dir}}.
      Make targeted improvements:
      - Fix any obvious bugs
      - Improve error handling where it's missing
      - Simplify overly complex functions
      - Add missing input validation on public APIs
    allowed_tools: ["read", "edit", "glob", "grep"]

  - id: "verify"
    name: "Build and Test"
    commands:
      - "{{build_cmd}} 2>&1"
      - "{{test_cmd}} 2>&1"
      - "{{lint_cmd}} 2>&1"
```

## Pattern: Conditional test writer

Only write tests if coverage analysis says they're needed. Uses `when` gating.

```yaml
name: "Conditional Test Improvement"

stages:
  - id: "analyze"
    name: "Analyze Coverage"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a test coverage analyst. Examine the relationship
      between source modules and test files. Identify gaps where
      important logic has no tests.
    user_prompt: |
      Analyze test coverage in this project:
      1. List all source modules and their corresponding test files
      2. Identify modules with no tests
      3. Identify modules where tests exist but miss important paths
      4. Count the ratio of test lines to source lines

      End with exactly one of:
      VERDICT: NEEDS_TESTS — if coverage has significant gaps
      VERDICT: ADEQUATE — if coverage is reasonable
    allowed_tools: ["read", "glob", "grep"]
  - id: "write-tests"
    name: "Write Missing Tests"
    when:
      stage: "analyze"
      output_matches: "NEEDS_TESTS"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a test engineer. Write focused tests for the gaps
      identified in the analysis. Prioritize tests for:
      1. Core business logic
      2. Error handling paths
      3. Edge cases
    user_prompt: |
      Coverage analysis: {{stages.analyze.summary}}

      Write tests to close the gaps. Run them to verify they pass.
    allowed_tools: ["read", "write", "bash"]

  - id: "format"
    name: "Format"
    commands:
      - "cargo fmt 2>&1"
```

## Pattern: Multi-model brainstorm

Use two different models for the same task, then have a third model pick the better approach. Useful for critical design decisions.

```yaml
name: "Dual Approach Design"

variables:
  problem: "Design an event sourcing system that handles 50k events/second with exactly-once processing guarantees"

stages:
  - id: "approach-a"
    name: "Approach A (Opus)"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a distributed systems architect. Design a solution
      optimizing for correctness and simplicity. Prefer proven
      patterns over novel approaches. Write your design to
      docs/approach-a.md.
    user_prompt: |
      Problem: {{problem}}
      Write a detailed technical design.
    allowed_tools: ["read", "write"]
  - id: "approach-b"
    name: "Approach B (Copilot/GPT)"
    runtime: "copilot-cli"
    model: "gpt-5"
    system_prompt: |
      You are a distributed systems architect. Design a solution
      optimizing for performance and horizontal scalability.
      Write your design to docs/approach-b.md.
    user_prompt: |
      Problem: {{problem}}
      Write a detailed technical design.
    allowed_tools: ["read", "write"]
  - id: "evaluate"
    name: "Evaluate and Choose"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a principal engineer evaluating two competing designs
      for the same system. Analyze trade-offs objectively.
    user_prompt: |
      Read docs/approach-a.md and docs/approach-b.md.

      Compare them across:
      - Correctness guarantees
      - Performance characteristics
      - Operational complexity
      - Implementation effort
      - Failure modes

      Write your evaluation to docs/evaluation.md. Pick the better
      approach (or synthesize the best parts of both) and write the
      final design to docs/final-design.md.
    allowed_tools: ["read", "write"]
```

## Pattern: PR preparation pipeline

A pipeline that prepares a project for a pull request — runs all checks, fixes issues, and generates a summary.

```yaml
name: "PR Preparation"

variables:
  branch_name: "feature/user-profiles"
  base_branch: "main"

stages:
  - id: "check"
    name: "Run All Checks"
    commands:
      - "cargo build 2>&1"
      - "cargo test 2>&1"
      - "cargo clippy -- -D warnings 2>&1"
      - "cargo fmt --check 2>&1"
  - id: "fix"
    name: "Fix Issues"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a developer preparing code for review. Fix all
      build errors, test failures, clippy warnings, and formatting
      issues. Make minimal, targeted fixes.
    user_prompt: |
      Check results: {{prior_summary}}

      If there are failures, fix them. Then re-run the checks.
      If everything passed, skip to confirming all checks pass.
    allowed_tools: ["read", "edit", "bash"]

  - id: "changelog"
    name: "Write Changelog Entry"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a technical writer. Write concise, informative
      changelog entries and PR descriptions.
    user_prompt: |
      Examine the changes on this branch compared to {{base_branch}}.
      Use `git diff {{base_branch}}` and `git log {{base_branch}}..HEAD --oneline`.

      Write:
      1. A changelog entry in CHANGELOG.md (under "Unreleased")
      2. A PR description in .github/pr-description.md with:
         - Summary of changes
         - Testing done
         - Screenshots/examples if applicable
    allowed_tools: ["read", "write", "bash"]
```

## Pattern: Codebase migration

A pipeline that migrates code from one pattern to another across many files. Uses exports to discover the scope, then processes files systematically.

```yaml
name: "Pattern Migration"

variables:
  migration: "Replace all callback-style async functions with async/await syntax"

stages:
  - id: "survey"
    name: "Survey Migration Scope"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a codebase analyst. Find all instances of the pattern
      that needs migration and assess the scope of work.
    user_prompt: |
      Migration goal: {{migration}}

      Search the codebase for all instances of the old pattern.
      For each file, note:
      - File path
      - Number of instances
      - Complexity (simple mechanical replacement vs. needs restructuring)

      Write a migration manifest to {{exports_file}}:
      {
        "total_files": "12",
        "total_instances": "47",
        "simple_count": "38",
        "complex_count": "9"
      }

      Also create docs/migration-plan.md listing every file and instance.
    allowed_tools: ["read", "glob", "grep", "write"]
    exports:
      keys: [total_files, total_instances]
  - id: "migrate"
    name: "Apply Migration"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a migration specialist. Work through the migration
      plan systematically, file by file. For each file:
      1. Read it
      2. Apply the migration
      3. Verify it still compiles/runs

      Be methodical. Don't skip files.
    user_prompt: |
      Migration: {{migration}}
      Scope: {{total_files}} files, {{total_instances}} instances

      Read docs/migration-plan.md for the full list.
      Migrate each file. Run the test suite periodically to catch regressions.
    allowed_tools: ["read", "edit", "bash", "glob"]

  - id: "verify"
    name: "Full Verification"
    commands:
      - "npm test 2>&1"
      - "npm run lint 2>&1"
  - id: "review"
    name: "Review Migration"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are reviewing a codebase migration. Check that:
      - All instances were migrated (none missed)
      - The migrations are correct (behavior preserved)
      - No new patterns were introduced that contradict the migration
    user_prompt: |
      Migration: {{migration}}
      Test results: {{prior_summary}}

      Search for any remaining instances of the old pattern.
      Review the changed files for correctness.
    allowed_tools: ["read", "glob", "grep"]
```

## Pattern: Branching refactor by module

Analyze a codebase, route to different refactoring strategies based on what's found.

```yaml
name: "Smart Refactor"

variables:
  target: "Reduce complexity in the authentication module"

stages:
  - id: "analyze"
    name: "Analyze Module"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a software architect analyzing code quality.
      Identify the root cause of complexity — is it tangled
      dependencies, god objects, duplicated logic, or missing
      abstractions?
    user_prompt: |
      Analyze: {{target}}

      Read the relevant code and classify the type of complexity.
    allowed_tools: ["read", "glob", "grep"]
    branch:
      prompt: "What refactoring strategy is most appropriate?"
      routes: [extract-module, simplify-logic, dedup, restructure]
      default: simplify-logic
  - id: "extract"
    name: "Extract Module"
    on_branch: "extract-module"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are refactoring by extracting cohesive modules from
      a tangled codebase. Create clear module boundaries.
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Extract well-defined modules with clear interfaces.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "simplify"
    name: "Simplify Logic"
    on_branch: "simplify-logic"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are simplifying complex control flow. Reduce nesting,
      use early returns, extract helper functions.
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Simplify the complex functions identified in the analysis.
    allowed_tools: ["read", "edit", "bash"]

  - id: "dedup"
    name: "Remove Duplication"
    on_branch: "dedup"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are removing code duplication. Extract shared logic
      into well-named functions. Don't over-abstract — only
      deduplicate things that are truly the same operation.
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Deduplicate the repeated code patterns.
    allowed_tools: ["read", "edit", "bash"]

  - id: "restructure"
    name: "Restructure"
    on_branch: "restructure"
    model: "claude-opus-4-6"
    system_prompt: |
      You are restructuring a module's architecture. This is a
      larger change — redesign the internal structure while
      preserving the public API.
    user_prompt: |
      Analysis: {{stages.analyze.summary}}
      Restructure the module. Keep the public API stable.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "test"
    name: "Verify Refactor"
    commands:
      - "cargo test 2>&1"
      - "cargo clippy -- -D warnings 2>&1"
```

## Pattern: Parallel CI checks

Run multiple independent checks concurrently, then summarize the results. Uses `depends_on: []` so all checks start immediately.

```yaml
name: "Parallel CI Pipeline"

stages:
  - id: "lint"
    name: "Lint"
    depends_on: []
    commands:
      - "cargo clippy -- -D warnings 2>&1"

  - id: "test"
    name: "Test"
    depends_on: []
    commands:
      - "cargo test 2>&1"

  - id: "format"
    name: "Format Check"
    depends_on: []
    commands:
      - "cargo fmt --check 2>&1"

  - id: "security"
    name: "Security Audit"
    depends_on: []
    commands:
      - "cargo audit 2>&1"

  - id: "fix"
    name: "Fix Issues"
    depends_on: [lint, test, format, security]
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a developer fixing CI failures. Read the output from
      all checks and fix every issue. Be targeted — make minimal
      changes to resolve each specific failure.
    user_prompt: |
      Lint results: {{stages.lint.response}}
      Test results: {{stages.test.response}}
      Format check: {{stages.format.response}}
      Security audit: {{stages.security.response}}

      Fix all failures. Re-run the checks to verify.
    allowed_tools: ["read", "edit", "bash"]
```

**When to use:** Pipelines with multiple independent validation steps. All four checks run concurrently, then the fix stage addresses any failures. This is faster than running checks sequentially.

## Pattern: Parallel frontend + backend implementation

A diamond pipeline that plans once, implements frontend and backend in parallel, then runs integration tests.

```yaml
name: "Parallel Full-Stack"

variables:
  feature: "User profile pages with avatar upload and activity feed"

summarizer:
  max_tokens: 2048

stages:
  - id: "plan"
    name: "Design Architecture"
    model: "claude-opus-4-6"
    system_prompt: |
      You are a senior architect. Design the feature for both
      frontend and backend. Write docs/plan.md with API contracts,
      data models, and component hierarchy.
    user_prompt: |
      Feature: {{feature}}
      Read the existing codebase, then design the architecture.
    allowed_tools: ["read", "write", "glob", "grep"]

  - id: "backend"
    name: "Implement Backend"
    depends_on: [plan]
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a backend developer. Implement the server-side
      components described in docs/plan.md. Follow existing patterns.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Implement the API endpoints, data models, and migrations.
      Run backend tests when done.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "frontend"
    name: "Implement Frontend"
    depends_on: [plan]
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a frontend developer. Implement the client-side
      components described in docs/plan.md. Follow existing patterns.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Implement the React components, API client calls, and routes.
      Run frontend tests when done.
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: "integrate"
    name: "Integration Tests"
    depends_on: [frontend, backend]
    commands:
      - "npm run test:integration 2>&1"

  - id: "review"
    name: "Code Review"
    depends_on: [integrate]
    model: "claude-opus-4-6"
    system_prompt: |
      You are a principal engineer reviewing a full-stack feature.
      Check for API contract mismatches between frontend and backend,
      security issues in file uploads, and missing error handling.
    user_prompt: |
      Plan: {{stages.plan.summary}}
      Backend: {{stages.backend.summary}}
      Frontend: {{stages.frontend.summary}}
      Integration results: {{stages.integrate.response}}

      Review all changes. Fix any issues you find.
    allowed_tools: ["read", "edit", "bash"]
```

**When to use:** Any full-stack feature where frontend and backend work is independent after the initial design phase. The parallel execution can cut total pipeline time significantly.

## Pattern: Documentation generator

Read an existing codebase and generate comprehensive documentation from scratch.

```yaml
name: "Documentation Generator"

variables:
  project_name: "myproject"

summarizer:
  model: "claude-haiku-4-5-20251001"
  max_tokens: 2048

stages:
  - id: "survey"
    name: "Survey Codebase"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a codebase analyst preparing to write documentation.
      Map out the entire project structure, identify public APIs,
      configuration options, and usage patterns.
    user_prompt: |
      Examine the {{project_name}} codebase thoroughly:
      - Project structure and module organization
      - All public APIs and their signatures
      - Configuration files and options
      - CLI commands and flags (if applicable)
      - Dependencies and their purposes

      Create docs/codebase-map.md with your findings.
    allowed_tools: ["read", "glob", "grep", "write"]
  - id: "readme"
    name: "Write README"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a technical writer who writes READMEs that developers
      actually want to read. Be concise. Lead with what the project
      does, then how to use it. Skip marketing fluff.
    user_prompt: |
      Codebase survey: {{prior_summary}}
      Read docs/codebase-map.md for full details.

      Write a README.md covering:
      - What this project does (one paragraph)
      - Installation
      - Quick start (get something working in 60 seconds)
      - Configuration
      - CLI reference (if applicable)
      - Development setup
    allowed_tools: ["read", "write"]
  - id: "api-docs"
    name: "Write API Documentation"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are an API documentation writer. Document every public
      endpoint or function with its parameters, return values,
      error cases, and a usage example.
    user_prompt: |
      Codebase survey: {{stages.survey.summary}}
      Read the source code to document all public APIs.

      Create docs/API.md with comprehensive API documentation.
      Include:
      - Every public function/endpoint
      - Parameters with types and descriptions
      - Return values
      - Error cases
      - Code examples
    allowed_tools: ["read", "write", "glob"]
  - id: "examples"
    name: "Write Examples"
    model: "claude-sonnet-4-6"
    system_prompt: |
      You are a developer advocate writing practical code examples.
      Each example should be self-contained and runnable.
    user_prompt: |
      Survey: {{stages.survey.summary}}
      API docs: {{prior_summary}}

      Create examples/ directory with practical, runnable examples:
      - examples/basic-usage — the simplest possible use case
      - examples/advanced — a realistic, production-like example
      - Each example should have its own README.md explaining what it does
    allowed_tools: ["read", "write"]
```

## Tips for writing effective pipelines

**Start simple, add stages.** Begin with a single stage that does the core task. Run it, evaluate the output, then decide what additional stages would improve the result.

**Use command stages for deterministic steps.** Build, test, lint, format — anything with a known command belongs in a command stage. It's free, fast, and gives exact feedback.

**Match model to task complexity.** Opus for design and review. Sonnet for implementation. Haiku for classification and summarization. Don't use Opus for everything — it's slower and more expensive without proportional benefit for mechanical tasks.

**Be specific in system prompts.** "You are a senior Rust developer who follows this project's error handling patterns" is much better than "You are helpful." Include constraints, preferences, and what not to do.

**Reference specific stages in branching pipelines.** Use `{{stages.analyze.summary}}` instead of `{{prior_summary}}` when the previous stage might have been skipped.

**Use `--dry-run` liberally.** Validate your templates are rendering correctly before spending API credits.

**Keep variable names consistent.** If `detect` exports `language`, don't have another stage export `lang`. Use the same name everywhere.
