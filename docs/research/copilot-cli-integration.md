# Research: GitHub Copilot CLI Integration in Fujin

## Executive Summary

**Verdict: Yes, Fujin can support GitHub Copilot CLI alongside Claude Code — and the existing architecture makes this feasible with moderate effort.**

Fujin's `AgentRuntime` trait already provides a clean abstraction point. GitHub Copilot CLI's programmatic mode (`copilot -p -s`) is close enough to Claude Code's `claude --print` interface that a `CopilotCliRuntime` implementation is viable. However, there are meaningful gaps in structured output, token tracking, and the summarizer subsystem that require careful handling.

This document covers the full analysis: Copilot CLI capabilities, mapping to Fujin's interface contract, integration paths, gaps, and a recommended implementation plan.

---

## 1. GitHub Copilot CLI Overview

GitHub Copilot CLI (released GA September 2025) is an agentic AI coding assistant that runs in the terminal. It is installed via `npm install -g @github/copilot` and invoked as `copilot`.

### Key Capabilities

| Capability | Claude Code | Copilot CLI | Notes |
|---|---|---|---|
| Non-interactive mode | `claude --print` | `copilot -p "prompt"` | Both support headless execution |
| Stdin piping | Yes (pipe to stdin) | Yes (pipe or `-p` flag) | Both accept piped input |
| Silent/clean output | `--output-format json` | `-s` (silent flag) | Different: Claude gives structured JSON, Copilot gives plain text |
| Streaming JSON | `--output-format stream-json --verbose` | Not available | Major gap — no structured streaming |
| Model selection | `--model <name>` | `--model <name>` | Both support per-invocation model selection |
| Tool permissions | `--tools Read,Write,Bash` | `--allow-tool Read --deny-tool 'shell(cd)'` | Different syntax, similar concept |
| Skip permission prompts | `--dangerously-skip-permissions` | `--allow-all-tools` or `--yolo` | Equivalent intent |
| System prompt | `--append-system-prompt <text>` | Not available as CLI flag | Gap — must embed in user prompt |
| Token usage (structured) | JSON `usage` field in output | Not in programmatic output | Gap — only available via `/usage` in interactive mode |
| Health check | `claude --version` | `copilot --version` | Equivalent |
| Working directory | `cmd.current_dir()` | `cmd.current_dir()` | Both respect cwd |
| Authentication | API key / OAuth | `GH_TOKEN` / `GITHUB_TOKEN` / OAuth | Different auth mechanisms |

### Copilot CLI Models

Available models (as of early 2026):
- `claude-sonnet-4.5` (default)
- `claude-sonnet-4`
- `claude-haiku-4.5`
- `gpt-5`

### Copilot CLI Tool Names

Copilot CLI tools use a different naming convention:
- `Read`, `Glob`, `Grep` — similar to Claude Code
- `shell(command:*)` — granular shell access (vs Claude Code's flat `Bash`)
- `fetch`, `websearch`, `extensions`, `githubRepo` — Copilot-specific tools

### ACP (Agent Client Protocol) Server Mode

Copilot CLI also supports running as an ACP server (`copilot --acp --stdio` or `--acp --port 8080`), which provides structured JSON-RPC communication over NDJSON. This is a richer integration path than the `-p -s` programmatic mode but requires a fundamentally different execution model.

---

## 2. Fujin's Current Architecture

### AgentRuntime Trait (the integration point)

```rust
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn name(&self) -> &str;

    async fn execute(
        &self,
        config: &StageConfig,
        context: &StageContext,
        workspace_root: &Path,
        progress_tx: Option<mpsc::UnboundedSender<String>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput>;

    async fn health_check(&self) -> CoreResult<()>;
}
```

### What `execute()` Must Return

```rust
pub struct AgentOutput {
    pub response_text: String,       // The agent's text response
    pub token_usage: Option<TokenUsage>,  // Optional token tracking
}
```

### Current ClaudeCodeRuntime Flow

1. Build prompt from `StageContext`
2. Spawn `claude --print --output-format json --model X --dangerously-skip-permissions --append-system-prompt "..." --tools T1,T2`
3. Pipe prompt via stdin
4. Parse structured JSON output → `AgentOutput`
5. Stream-json mode extracts real-time tool-use activity for TUI

### Tight Coupling Points

1. **`ContextBuilder::summarize()`** — directly spawns `claude --print --model <summarizer>` (hardcoded, not via `AgentRuntime`)
2. **Tool name mapping** — `ClaudeCodeRuntime::build_tools_flag()` maps `read` → `Read`, `bash` → `Bash`, etc.
3. **Stream-json parsing** — `extract_activity()` parses Claude-specific JSON schema for TUI progress
4. **Token usage** — expects `input_tokens` / `output_tokens` from Claude's JSON output

---

## 3. Integration Approach: `CopilotCliRuntime`

### 3.1 Basic Programmatic Mode (`copilot -p -s`)

The simplest integration path — treat Copilot CLI like a text-in/text-out subprocess.

**Command construction:**
```
copilot -p "<prompt>" -s --model <model> --allow-tool Read --allow-tool Write ...
```

Or with stdin piping (preferred, avoids argument-length issues):
```
echo "<prompt>" | copilot -p /dev/stdin -s --model <model> --allow-all-tools
```

**Pros:**
- Simple, matches the existing subprocess pattern
- Works today with no special SDK dependencies
- Prompt is just text; system prompt can be prepended to user prompt

**Cons:**
- No structured JSON output — `response_text` must be captured as raw stdout
- No token usage information — `AgentOutput.token_usage` will always be `None`
- No real-time streaming progress — `progress_tx` channel will be unused (TUI shows no tool activity)
- No `--append-system-prompt` — system prompt must be inlined into the user prompt

### 3.2 ACP Server Mode (`copilot --acp --stdio`)

A richer path — run Copilot CLI as a persistent JSON-RPC server.

**Protocol flow:**
```
→ session/initialize
← capabilities
→ session/new
← session_id
→ session/prompt { prompt: "..." }
← session/update { type: "agent_message_chunk", content: "..." }
← session/update { type: "tool_use", tool: "Read", ... }
← session/update { type: "agent_message_chunk", content: "..." }
← session/update { type: "done" }
```

**Pros:**
- Structured NDJSON streaming — could extract tool-use activity for TUI
- Session persistence — could carry context across stages without re-summarizing
- Richer metadata in responses

**Cons:**
- Fundamentally different execution model (persistent server vs one-shot subprocess)
- Requires managing a long-running process lifecycle
- ACP is still in public preview — protocol may change
- Significantly more implementation effort
- Would need a Rust ACP client library (none official; community Rust SDK exists)

### 3.3 Copilot SDK (JSON-RPC via SDK)

The Copilot SDK (`@github/copilot-sdk` for TypeScript, `github-copilot-sdk` for Python) provides programmatic access. A community Rust implementation exists.

**Pros:**
- Structured output, proper lifecycle management
- Official SDKs available (TypeScript, Python, Go, .NET)

**Cons:**
- Fujin is Rust — no official Rust SDK (only community)
- Adding a TypeScript/Python dependency to a Rust project is architecturally awkward
- SDK is in Technical Preview — not production-ready
- Adds significant dependency weight

### Recommended Path: Start with 3.1, evolve to 3.2

Begin with the simple programmatic mode. It maps cleanly to the existing `AgentRuntime` subprocess pattern. Once ACP stabilizes and a Rust client is mature, migrate to the ACP server model for richer integration.

---

## 4. Implementation Plan

### Phase 1: Core Runtime (Moderate Effort)

#### 4.1 New `CopilotCliRuntime` struct

```rust
pub struct CopilotCliRuntime {
    copilot_bin: String,  // defaults to "copilot"
}
```

#### 4.2 Tool name mapping

```rust
fn build_tools_flags(allowed_tools: &[String]) -> Vec<String> {
    allowed_tools.iter().flat_map(|t| {
        let tool_name = match t.as_str() {
            "read"     => "Read",
            "write"    => "Write",
            "edit"     => "Edit",
            "bash"     => "shell",
            "glob"     => "Glob",
            "grep"     => "Grep",
            "notebook" => "NotebookEdit",
            other      => other,
        };
        vec!["--allow-tool".to_string(), tool_name.to_string()]
    }).collect()
}
```

Note: Copilot uses `--allow-tool X` per tool (not comma-separated). The `bash` → `shell` mapping is approximate; Copilot's shell tool has granular sub-permissions like `shell(git:*)`.

#### 4.3 System prompt handling

Since Copilot CLI has no `--append-system-prompt` flag, the system prompt must be embedded in the user prompt:

```rust
fn build_prompt(context: &StageContext, system_prompt: &str) -> String {
    format!(
        "<system>\n{}\n</system>\n\n{}",
        system_prompt,
        Self::build_base_prompt(context)  // reuse existing prompt logic
    )
}
```

#### 4.4 Execute implementation

```rust
async fn execute(&self, config, context, workspace_root, progress_tx, cancel_flag) -> CoreResult<AgentOutput> {
    let prompt = Self::build_prompt(context, &config.system_prompt);

    let mut cmd = Command::new(&self.copilot_bin);
    cmd.arg("-p").arg("/dev/stdin")
       .arg("-s")
       .arg("--model").arg(&config.model);

    // If any tools are specified, allow them individually
    if !config.allowed_tools.is_empty() {
        for flag in Self::build_tools_flags(&config.allowed_tools) {
            cmd.arg(flag);
        }
    } else {
        cmd.arg("--allow-all-tools");
    }

    cmd.current_dir(workspace_root)
       .stdin(Stdio::piped())
       .stdout(Stdio::piped())
       .stderr(Stdio::piped());

    // Spawn, pipe prompt, capture output (same pattern as ClaudeCodeRuntime)
    // ...

    Ok(AgentOutput {
        response_text: stdout_text,
        token_usage: None,  // Not available from programmatic mode
    })
}
```

### Phase 2: Config & CLI Changes

#### 4.5 Add `runtime` field to pipeline config

```yaml
name: my-pipeline
runtime: copilot-cli   # new field; defaults to "claude-code"
stages:
  - id: generate
    model: claude-sonnet-4   # Copilot CLI model names
    # ...
```

Or per-stage runtime selection:

```yaml
stages:
  - id: generate
    runtime: copilot-cli
    model: claude-sonnet-4
  - id: review
    runtime: claude-code
    model: claude-sonnet-4-6
```

#### 4.6 Runtime factory

```rust
fn create_runtime(name: &str) -> Box<dyn AgentRuntime> {
    match name {
        "claude-code" => Box::new(ClaudeCodeRuntime::new()),
        "copilot-cli" => Box::new(CopilotCliRuntime::new()),
        _ => panic!("Unknown runtime: {name}"),
    }
}
```

#### 4.7 Adapt `ContextBuilder` summarizer

The summarizer currently hardcodes `claude --print`. Options:
- Accept a `Box<dyn AgentRuntime>` for summarization
- Use the pipeline's configured runtime
- Keep Claude Code as summarizer (cheapest option; Claude Haiku is fast/cheap)

### Phase 3: TUI Adaptation

The TUI progress display relies on Claude's stream-json output. For Copilot CLI in basic mode:
- Show a generic "Agent working..." spinner instead of tool-by-tool activity
- Progress channel emits periodic heartbeat messages
- Future ACP integration could restore granular progress

### Phase 4: Future ACP Integration

Once ACP stabilizes:
- Implement `AcpCopilotRuntime` that manages a persistent `copilot --acp --stdio` process
- Parse NDJSON session updates for tool-use activity
- Extract structured metadata from responses
- Enable cross-stage session continuity

---

## 5. Gap Analysis

| Feature | Claude Code | Copilot CLI (`-p -s`) | Impact on Fujin |
|---|---|---|---|
| Structured JSON output | `--output-format json` | Not available | Token usage tracking lost; response is raw text (acceptable) |
| Streaming tool activity | `--output-format stream-json` | Not available | TUI shows no per-tool progress (degraded UX, not blocking) |
| System prompt flag | `--append-system-prompt` | Not available | Must inline into user prompt (workable) |
| Token usage | `usage.input_tokens/output_tokens` | Not exposed | `AgentOutput.token_usage` always `None` (cosmetic loss) |
| Tool permissions | `--tools T1,T2` (allowlist) | `--allow-tool X` per tool | Different syntax but equivalent semantics |
| Skip permissions | `--dangerously-skip-permissions` | `--allow-all-tools` / `--yolo` | Direct equivalent |
| Model names | `claude-sonnet-4-6`, `claude-haiku-4-5-20251001` | `claude-sonnet-4`, `claude-haiku-4.5`, `gpt-5` | Different naming; config must use runtime-appropriate names |
| Pricing model | Anthropic API usage | GitHub Copilot subscription (premium requests) | Different billing; users must have Copilot subscription |
| Authentication | Anthropic API key | `GH_TOKEN` / GitHub OAuth | Different auth setup |

---

## 6. Open Questions

1. **Model name validation**: Should fujin validate model names against the selected runtime, or pass them through? (Recommend: pass-through with a warning on unknown names.)

2. **Mixed-runtime pipelines**: Should different stages in the same pipeline use different runtimes? (Useful for cost optimization — e.g., GPT-5 for complex stages, Claude Haiku for summarization.)

3. **Summarizer coupling**: Should the summarizer also use the pipeline runtime, or stay on Claude Code? (Recommend: make it configurable but default to Claude Code since Haiku is fast/cheap.)

4. **Copilot CLI availability**: Should `fujin agents --check` verify all configured runtimes? (Yes — both `claude --version` and `copilot --version`.)

5. **ACP timeline**: When should the ACP-based runtime be prioritized? (Wait for ACP to exit public preview and a stable Rust client to emerge.)

---

## 7. Effort Estimate

| Component | Scope |
|---|---|
| `CopilotCliRuntime` struct + execute | ~200 lines of Rust |
| Tool mapping + prompt adaptation | ~50 lines |
| Config schema changes (`runtime` field) | ~30 lines in pipeline_config.rs + validation |
| Runtime factory / CLI integration | ~50 lines |
| ContextBuilder abstraction (optional) | ~100 lines |
| Tests | ~150 lines |
| TUI graceful degradation | ~30 lines |
| **Total Phase 1** | **~600 lines** |

---

## 8. Conclusion

Supporting GitHub Copilot CLI in Fujin is **feasible and architecturally sound**. The existing `AgentRuntime` trait was designed for exactly this kind of extensibility. The programmatic mode (`copilot -p -s`) provides enough functionality to drive Copilot CLI as a pipeline stage backend, with acceptable tradeoffs in token tracking and TUI granularity.

The recommended approach is:
1. **Phase 1**: Implement `CopilotCliRuntime` using `copilot -p -s` (subprocess model)
2. **Phase 2**: Add `runtime` config field and runtime factory
3. **Phase 3**: Gracefully degrade TUI for non-streaming runtimes
4. **Phase 4**: Migrate to ACP server mode when the protocol stabilizes

This gives users the choice of running pipelines with Claude Code, GitHub Copilot CLI, or a mix of both — which is a compelling differentiator for Fujin as a runtime-agnostic pipeline orchestrator.

---

## Sources

- [GitHub Copilot CLI — Official Feature Page](https://github.com/features/copilot/cli)
- [GitHub Copilot CLI — Repository](https://github.com/github/copilot-cli)
- [GitHub Docs — Using Copilot CLI](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli)
- [GitHub Docs — About Copilot CLI](https://docs.github.com/copilot/concepts/agents/about-copilot-cli)
- [GitHub Copilot SDK — Repository](https://github.com/github/copilot-sdk)
- [Copilot CLI ACP Server — GitHub Docs](https://docs.github.com/en/copilot/reference/acp-server)
- [ACP Support Public Preview — Discussion](https://github.com/orgs/community/discussions/185860)
- [Automating Copilot CLI — R-bloggers](https://www.r-bloggers.com/2025/10/automating-the-github-copilot-agent-from-the-command-line-with-copilot-cli/)
- [Copilot CLI Release Overview — SmartScope](https://smartscope.blog/en/generative-ai/github-copilot-cli-release-2025/)
- [Copilot CLI Tutorial — DataCamp](https://www.datacamp.com/tutorial/github-copilot-cli-tutorial)
