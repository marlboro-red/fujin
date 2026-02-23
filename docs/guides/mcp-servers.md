# MCP Servers

This guide covers how to attach [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) servers to pipeline stages. MCP servers give agents access to external tools — databases, APIs, documentation indexes, and more — without baking tool logic into your prompts.

## How it works

1. **Define** servers at the pipeline level under `mcp_servers`
2. **Reference** them by name on each stage that needs them
3. At runtime, fujin generates a temporary MCP config file and passes it to the agent via `--mcp-config`
4. The agent runtime starts the servers, uses them during execution, and the config file is cleaned up automatically

```yaml
mcp_servers:
  database:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://localhost/mydb

stages:
  - id: analyze
    name: Analyze Schema
    system_prompt: "You are a database analyst."
    user_prompt: "Examine the schema and suggest improvements."
    mcp_servers: [database]
```

This is the same pattern as `retry_groups` — define once at the top level, reference by name per stage.

---

## Transports

Each MCP server uses exactly one transport. Specifying both `command` and `url` (or neither) is a validation error.

### Stdio

Runs a local command as a subprocess. The agent communicates with the server over stdin/stdout.

```yaml
mcp_servers:
  postgres:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://user:pass@localhost/mydb
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | string | — | The command to run |
| `args` | list | `[]` | Command-line arguments |
| `env` | map | `{}` | Environment variables passed to the process |

Use `env` for connection strings, API keys, and other secrets the server needs.

### HTTP/SSE

Connects to a remote MCP server over HTTP with Server-Sent Events.

```yaml
mcp_servers:
  api-docs:
    url: https://api-docs.example.com/mcp
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | — | The server URL |
| `headers` | map | `{}` | HTTP headers sent with every request |

---

## Authentication

### Stdio servers

Pass credentials through `env`. The server process inherits these environment variables:

```yaml
mcp_servers:
  database:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://user:secret@prod-host/mydb

  github:
    command: npx
    args: [-y, "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: ghp_xxxxxxxxxxxx
```

### HTTP/SSE servers

Pass credentials through `headers`:

```yaml
mcp_servers:
  private-api:
    url: https://internal.example.com/mcp
    headers:
      Authorization: "Bearer sk-proj-xxxxxxxxxxxx"

  custom-service:
    url: https://service.example.com/mcp
    headers:
      X-API-Key: "my-api-key"
      X-Org-Id: "org-123"
```

### Using variables for secrets

Combine `headers`/`env` with pipeline [variables](../pipeline-authoring.md#template-variables) so secrets aren't hardcoded:

```yaml
variables:
  db_url: postgresql://localhost/dev
  api_token: ""

mcp_servers:
  database:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: "{{db_url}}"
  api:
    url: https://api.example.com/mcp
    headers:
      Authorization: "Bearer {{api_token}}"
```

Override at runtime:

```
fujin run -c pipeline.yaml \
  --var db_url=postgresql://user:pass@prod/mydb \
  --var api_token=sk-prod-xxxx
```

> **Note:** Variable substitution in `env` and `headers` values happens during template rendering, before the MCP config file is generated.

---

## Per-stage server selection

Not every stage needs every server. Reference only the servers each stage actually uses:

```yaml
mcp_servers:
  database:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://localhost/mydb
  api-docs:
    url: https://api-docs.example.com/mcp

stages:
  - id: analyze
    name: Analyze
    system_prompt: "You are a database analyst with access to API documentation."
    user_prompt: "Review the schema and check it against the API spec."
    mcp_servers: [database, api-docs]

  - id: implement
    name: Implement
    system_prompt: "You are a backend developer."
    user_prompt: "Write the data access layer based on the analysis."
    mcp_servers: [database]

  - id: review
    name: Review
    system_prompt: "You are a code reviewer."
    user_prompt: "Review the implementation for correctness."
    # No mcp_servers — this stage doesn't need external tools
```

---

## Interaction with includes

MCP servers follow the same prefixing rules as retry groups and stage IDs. When you include a pipeline:

- Child pipeline MCP server definitions are merged into the parent with the alias prefix
- Stage-level `mcp_servers` references to child-defined servers are prefixed automatically
- References to parent-defined servers are left unchanged

**Child pipeline** (`backend.yaml`):
```yaml
name: backend
mcp_servers:
  database:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: "{{db_url}}"

stages:
  - id: migrate
    name: Migrate
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
    mcp_servers: [database]
```

**Parent pipeline**:
```yaml
name: fullstack
mcp_servers:
  docs:
    url: https://docs.example.com/mcp

includes:
  - source: backend.yaml
    as: be
    depends_on: [analyze]
    vars:
      db_url: postgresql://localhost/prod

stages:
  - id: analyze
    name: Analyze
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
    mcp_servers: [docs]
```

After include resolution:

- `docs` — parent server, unchanged
- `be.database` — child server, prefixed with alias
- Stage `be.migrate` references `be.database` (auto-prefixed)
- Stage `analyze` references `docs` (unchanged)

Nested includes stack prefixes: if `backend.yaml` itself includes `database.yaml` as `db`, the server becomes `be.db.database`.

---

## Runtime support

| Runtime | MCP support |
|---------|-------------|
| `claude-code` | Full support via `--mcp-config` |
| `copilot-cli` | Not supported (validation warning; servers are ignored) |

If a stage uses `copilot-cli` and has `mcp_servers`, fujin emits a validation warning and a runtime warning. The stage still executes — it just won't have access to the MCP servers.

---

## Validation rules

**Errors:**
- Each MCP server must have exactly one of `command` or `url`
- `command` and `url` must not be empty strings
- Stage `mcp_servers` entries must reference servers defined at the pipeline level

**Warnings:**
- Defined but unreferenced MCP server
- `mcp_servers` on a command stage
- `mcp_servers` with `copilot-cli` runtime
- `args`/`env` on an HTTP/SSE server (ignored)
- `headers` on a stdio server (ignored)

---

## Examples

### Database query pipeline

```yaml
name: db-analysis
mcp_servers:
  postgres:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://readonly:pass@prod/myapp

stages:
  - id: analyze
    name: Analyze Schema
    system_prompt: |
      You are a database analyst. Use the postgres MCP server
      to inspect the schema and identify performance issues.
    user_prompt: |
      Analyze the database schema. Focus on:
      - Missing indexes on frequently queried columns
      - N+1 query patterns in the ORM models
      - Tables that could benefit from partitioning
    mcp_servers: [postgres]
    allowed_tools: [read, write]

  - id: report
    name: Generate Report
    system_prompt: "You are a technical writer."
    user_prompt: |
      Based on the analysis in {{prior_summary}}, write a
      database optimization report at docs/db-optimization.md.
    allowed_tools: [read, write]
```

### Multi-source research pipeline

```yaml
name: research
variables:
  api_token: ""

mcp_servers:
  github:
    command: npx
    args: [-y, "@modelcontextprotocol/server-github"]
    env:
      GITHUB_TOKEN: "{{api_token}}"
  docs:
    url: https://internal-docs.example.com/mcp
    headers:
      Authorization: "Bearer {{api_token}}"

stages:
  - id: research
    name: Research
    system_prompt: |
      You are a technical researcher. Use the GitHub MCP server
      to search repositories and the docs server to search
      internal documentation.
    user_prompt: |
      Research how authentication is implemented across our
      services. Check both the codebase and internal docs.
    mcp_servers: [github, docs]
    allowed_tools: [read, write]

  - id: synthesize
    name: Synthesize Findings
    system_prompt: "You are a technical architect."
    user_prompt: |
      Based on the research: {{prior_summary}}
      Write a unified authentication architecture proposal.
    allowed_tools: [read, write]
```

### Selective server access with parallel stages

```yaml
name: fullstack-audit
mcp_servers:
  postgres:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://localhost/myapp
  redis:
    command: npx
    args: [-y, "@modelcontextprotocol/server-redis"]
    env:
      REDIS_URL: redis://localhost:6379

stages:
  - id: db-audit
    name: Database Audit
    depends_on: []
    system_prompt: "You are a database specialist."
    user_prompt: "Audit the PostgreSQL schema for issues."
    mcp_servers: [postgres]

  - id: cache-audit
    name: Cache Audit
    depends_on: []
    system_prompt: "You are a caching specialist."
    user_prompt: "Audit the Redis usage patterns."
    mcp_servers: [redis]

  - id: report
    name: Combined Report
    depends_on: [db-audit, cache-audit]
    system_prompt: "You are a senior architect."
    user_prompt: |
      Combine the database and cache audit findings into
      a single infrastructure report.
    allowed_tools: [read, write]
```
