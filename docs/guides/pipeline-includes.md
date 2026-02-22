# Pipeline Composition with `includes`

This guide covers how to compose pipelines from reusable building blocks. The `includes` directive imports stages from other pipeline files, prefixes their IDs, wires their dependencies, and merges everything into a single flat pipeline that the DAG scheduler runs unchanged.

## The problem includes solve

As pipelines grow, you end up with repeated stage patterns. A backend deploy sequence, a frontend build-and-test chain, a database migration group — these appear in multiple pipelines with minor variations. Copy-pasting stages across files creates maintenance burden and drift.

`includes` lets you extract reusable stage libraries into separate YAML files and compose them into larger pipelines. Each included file is a valid pipeline on its own — you can run it standalone or include it as part of a larger workflow.

## Quick example

**backend.yaml** — a standalone backend pipeline:

```yaml
name: backend-stages
runtime: claude-code
variables:
  db_url: "localhost"

stages:
  - id: build
    name: Build Backend
    depends_on: []
    system_prompt: "You are a backend developer."
    user_prompt: "Build the backend using {{db_url}}."
    allowed_tools: ["read", "write", "bash"]

  - id: deploy
    name: Deploy Backend
    # depends_on absent → implicit dep on "build"
    system_prompt: "You are a deployment engineer."
    user_prompt: "Deploy the backend."
    allowed_tools: ["read", "bash"]
```

**fullstack.yaml** — composes backend into a larger pipeline:

```yaml
name: fullstack
variables:
  db_url: "postgres://localhost/app"

includes:
  - source: backend.yaml
    as: be
    depends_on: [analyze]
    vars:
      db_url: "{{db_url}}"

stages:
  - id: analyze
    name: Analysis
    depends_on: []
    system_prompt: "You are an architect."
    user_prompt: "Analyze the codebase."
    allowed_tools: ["read", "glob"]

  - id: integrate
    name: Integration
    depends_on: [be.deploy]
    system_prompt: "You are a QA engineer."
    user_prompt: "Run integration tests."
    allowed_tools: ["read", "bash"]
```

After resolution, the pipeline has four stages: `analyze`, `be.build`, `be.deploy`, `integrate`. The included stages are prefixed with `be.` and wired into the parent's dependency graph.

```
  analyze
     |
  be.build
     |
  be.deploy
     |
  integrate
```

## How includes work

Includes are resolved at **config load time**, before validation and DAG construction. The resolution process:

1. Parses `includes` entries from the parent config
2. Loads each included YAML file (recursively, if it also has includes)
3. Prefixes all stage IDs with the `as` alias: `build` becomes `be.build`
4. Prefixes all internal references (depends_on, retry_group, when.stage)
5. Wires root stages of the included pipeline to the include-level `depends_on`
6. Substitutes variables from the `vars` block into prompts
7. Propagates the included pipeline's runtime to its stages
8. Merges everything into a flat stage list

After resolution, the rest of the system — validation, DAG scheduling, execution — sees a normal flat pipeline config.

## Include fields

```yaml
includes:
  - source: backend.yaml         # REQUIRED — path to included pipeline file
    as: be                        # REQUIRED — prefix alias for stage IDs
    depends_on: [analyze]         # optional — wires included root stages to parent
    vars:                         # optional — variables passed into the include
      db_url: "prod-db.example.com"
```

### `source` (required, string)

Path to the pipeline YAML file. Resolved relative to the parent file's directory. The included file must be a valid pipeline config (it can be run standalone too).

### `as` (required, string)

Prefix alias applied to all stage IDs from this include. Must contain only alphanumeric characters, underscores, or hyphens. Must be unique across all includes in the same parent.

Stage `build` with `as: be` becomes `be.build`. This prevents ID collisions when including multiple pipelines that happen to use the same stage IDs.

### `depends_on` (optional, list of strings)

Stages in the parent pipeline that the included pipeline's **root stages** depend on. Root stages are those with `depends_on: []` or no dependencies within the include.

```yaml
includes:
  - source: backend.yaml
    as: be
    depends_on: [analyze]     # be.build (root) will depend on analyze
```

If omitted, root stages of the include have no parent dependencies and can start immediately.

### `vars` (optional, map)

Variables passed into the included pipeline. These override the included pipeline's own `variables` defaults. Variable substitution (`{{key}}`) is applied to `system_prompt`, `user_prompt`, and `commands` of included stages.

```yaml
includes:
  - source: backend.yaml
    as: be
    vars:
      db_url: "prod-db.example.com"   # overrides child's default "localhost"
      api_port: "8080"                 # adds a new variable
```

Variables are **scoped** — included pipeline variables are not merged into the parent's variables. The substitution happens only within the included stages.

## Cross-referencing included stages

Any stage can reference an included stage by its prefixed ID:

```yaml
stages:
  - id: integrate
    depends_on: [be.deploy, fe.build]    # cross-reference via prefix
    ...
```

This works for `depends_on`, `when.stage`, and any other field that references stage IDs:

```yaml
stages:
  - id: check
    when:
      stage: be.build
      output_matches: "SUCCESS"
    depends_on: [be.build]
    ...
```

## Multiple includes

Include multiple pipelines into a single parent. Each gets its own alias:

```yaml
name: fullstack
includes:
  - source: backend.yaml
    as: be
    depends_on: [analyze]
    vars:
      db_url: "{{db_url}}"
      api_port: "8080"

  - source: frontend.yaml
    as: fe
    depends_on: [analyze]
    vars:
      api_base: "http://localhost:8080"

stages:
  - id: analyze
    name: Analysis
    depends_on: []
    system_prompt: "Analyze the project."
    user_prompt: "Go."

  - id: integrate
    name: Integration
    depends_on: [be.deploy, fe.build]
    system_prompt: "Run integration tests."
    user_prompt: "Go."
```

```
         analyze
        /       \
   be.build    fe.build
      |            |
   be.deploy   fe.test
        \       /
        integrate
```

Aliases must be unique — you can't have two includes with `as: be`.

## Nested includes

Included pipelines can themselves include other pipelines. Prefixes stack:

**database.yaml:**
```yaml
name: database
stages:
  - id: migrate
    name: Migrate DB
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
```

**backend.yaml:**
```yaml
name: backend
includes:
  - source: database.yaml
    as: db
    depends_on: [build]
stages:
  - id: build
    name: Build
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
```

**fullstack.yaml:**
```yaml
name: fullstack
includes:
  - source: backend.yaml
    as: be
stages:
  - id: start
    ...
```

After resolution, `fullstack.yaml` contains stages: `start`, `be.build`, `be.db.migrate`. The database stage gets the double prefix `be.db.` because it was included by backend (as `db`) which was included by fullstack (as `be`).

## Implicit dependencies

When an included pipeline uses implicit sequential dependencies (stages without `depends_on`), they are converted to explicit dependencies during resolution:

**child.yaml:**
```yaml
stages:
  - id: first
    depends_on: []      # root stage
    ...
  - id: second          # implicit: depends on "first"
    ...
  - id: third           # implicit: depends on "second"
    ...
```

**parent.yaml:**
```yaml
includes:
  - source: child.yaml
    as: ch
    depends_on: [setup]
```

After resolution:
- `ch.first` depends on `[setup]` (root stage gets include-level depends_on)
- `ch.second` depends on `[ch.first]` (implicit converted to explicit)
- `ch.third` depends on `[ch.second]` (implicit converted to explicit)

## Runtime propagation

If the included pipeline specifies a different `runtime` than the parent, its stages inherit that runtime automatically:

```yaml
# parent: runtime = claude-code (default)
includes:
  - source: copilot-stages.yaml    # runtime: copilot-cli
    as: cop
```

After resolution, stages from `copilot-stages.yaml` have `runtime: copilot-cli` set explicitly, so they use the correct runtime even though the parent defaults to `claude-code`. Stages that already have a per-stage runtime override keep their original value.

## Retry group prefixing

Retry groups from included pipelines are prefixed with the alias, just like stage IDs:

**child.yaml:**
```yaml
retry_groups:
  fix:
    max_retries: 3
stages:
  - id: code
    retry_group: fix
    ...
  - id: test
    retry_group: fix
    ...
```

After including with `as: ch`:
- Retry group `fix` becomes `ch.fix`
- Stage `retry_group` fields become `ch.fix`
- The group functions identically — stages are still consecutive and retry as a unit

## Circular include detection

Fujin detects circular includes and reports an error:

```
# a.yaml includes b.yaml, b.yaml includes a.yaml
Error: Include resolution failed for 'b.yaml': Circular include detected
```

The detection uses canonical file paths, so different relative paths to the same file are correctly identified as duplicates.

## Validation

Includes are validated in two phases:

### Before resolution (structural checks)

- `source` must not be empty
- `as` alias must not be empty
- `as` alias must be a valid identifier (alphanumeric, underscore, hyphen)
- No duplicate aliases within the same parent
- `depends_on` entries must reference stages defined in the parent's `stages`

### After resolution (standard validation)

Once includes are resolved into a flat config, all standard validation rules apply:
- Unique stage IDs (including prefixed IDs)
- Valid `depends_on` references
- No circular dependencies
- Retry group constraints
- All other existing validation checks

## Interaction with other features

### Parallel stages

Includes work naturally with `depends_on`. Included stages form subgraphs within the larger DAG. The scheduler runs independent stages concurrently regardless of whether they came from includes or the parent.

### Branching

`when` and `branch`/`on_branch` work across include boundaries. An included stage can reference a parent stage in its `when.stage`, and parent stages can reference included stages in their `depends_on`. Branch route names are not prefixed — they are just strings.

### Checkpoints

After resolution, the pipeline is flat. Checkpoints see the resolved stage IDs (e.g., `be.build`) and resume correctly.

### Variables and overrides

CLI `--var` overrides apply to the parent pipeline's variables. To pass them into includes, reference them in the include's `vars` block:

```yaml
variables:
  db_url: "localhost"

includes:
  - source: backend.yaml
    as: be
    vars:
      db_url: "{{db_url}}"     # passes the parent variable (or CLI override) through
```

## Realistic example: Microservice deployment pipeline

A pipeline that composes backend, frontend, and infrastructure stages from separate files:

**infra.yaml:**
```yaml
name: infrastructure
variables:
  env: "dev"
  region: "us-east-1"

stages:
  - id: provision
    name: Provision Infrastructure
    depends_on: []
    system_prompt: "You are a DevOps engineer."
    user_prompt: |
      Provision {{env}} infrastructure in {{region}}.
      Write Terraform configs.
    allowed_tools: ["read", "write"]

  - id: validate
    name: Validate Terraform
    commands:
      - "terraform validate 2>&1"
      - "terraform plan -out=tfplan 2>&1"
```

**backend.yaml:**
```yaml
name: backend-service
runtime: claude-code
variables:
  db_url: "localhost:5432"
  service_name: "api"

retry_groups:
  build:
    max_retries: 3

stages:
  - id: implement
    name: Implement Service
    depends_on: []
    retry_group: build
    system_prompt: "You are a backend developer."
    user_prompt: |
      Build the {{service_name}} service.
      Database: {{db_url}}
    allowed_tools: ["read", "write", "edit", "bash"]

  - id: test
    name: Test Service
    retry_group: build
    commands:
      - "cargo build 2>&1"
      - "cargo test 2>&1"
```

**deploy.yaml (the composed pipeline):**
```yaml
name: full-deploy
variables:
  env: "production"
  region: "us-west-2"
  db_url: "prod-db.internal:5432"

includes:
  - source: infra.yaml
    as: infra
    vars:
      env: "{{env}}"
      region: "{{region}}"

  - source: backend.yaml
    as: api
    depends_on: [infra.validate]
    vars:
      db_url: "{{db_url}}"
      service_name: "user-api"

  - source: backend.yaml
    as: auth
    depends_on: [infra.validate]
    vars:
      db_url: "{{db_url}}"
      service_name: "auth-service"

stages:
  - id: smoke-test
    name: Smoke Tests
    depends_on: [api.test, auth.test]
    commands:
      - "curl -f http://localhost:8080/health 2>&1"
      - "curl -f http://localhost:8081/health 2>&1"
```

This reuses `backend.yaml` twice with different aliases and variables, producing services `api.implement`, `api.test`, `auth.implement`, `auth.test` — each with their own retry group (`api.build`, `auth.build`).

```
  infra.provision
       |
  infra.validate
     /        \
api.implement  auth.implement
     |              |
  api.test      auth.test
     \          /
    smoke-test
```

Run it:
```
fujin run -c deploy.yaml
fujin run -c deploy.yaml --var env=staging --var region=eu-west-1
```

## What to read next

- **[Parallel Stages](parallel-stages.md)** — `depends_on` and DAG scheduling
- **[Retry Groups](retry-groups.md)** — Automatic retry-on-failure with verification
- **[Pipeline Patterns](pipeline-patterns.md)** — Copy-pasteable recipes for common workflows
- **[Pipeline Authoring Reference](../pipeline-authoring.md)** — Complete field reference
