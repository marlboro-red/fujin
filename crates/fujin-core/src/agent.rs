use crate::context::StageContext;
use crate::error::{CoreError, CoreResult};
use crate::stage::TokenUsage;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use fujin_config::{McpServerConfig, StageConfig};

/// Maximum response size (50 MB) to prevent unbounded memory growth.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// Output from an agent execution.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    /// The agent's text response.
    pub response_text: String,

    /// Token usage if available.
    pub token_usage: Option<TokenUsage>,
}

/// Trait for agent runtimes that can execute pipeline stages.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Human-readable name for this runtime.
    fn name(&self) -> &str;

    /// Whether this runtime supports real-time streaming of tool-use activity.
    ///
    /// When `true`, the runtime emits detailed progress messages (tool names,
    /// file paths) via `progress_tx`. When `false`, consumers should show a
    /// generic "working..." indicator instead.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Execute a stage: send prompt, let agent work, return result.
    ///
    /// `progress_tx` is an optional channel for streaming human-readable
    /// activity messages (e.g., "Using tool: Read", "Thinking...").
    ///
    /// `cancel_flag` is an optional flag that, when set to `true`, signals
    /// the runtime to kill the agent process and return a `Cancelled` error.
    async fn execute(
        &self,
        config: &StageConfig,
        context: &StageContext,
        workspace_root: &Path,
        progress_tx: Option<mpsc::UnboundedSender<String>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput>;

    /// Check if the runtime is available (e.g., `claude` binary exists).
    async fn health_check(&self) -> CoreResult<()>;
}

/// Claude Code CLI agent runtime.
///
/// Spawns `claude` as a subprocess for each stage. Claude Code handles
/// file I/O, bash execution, and tool use natively.
pub struct ClaudeCodeRuntime {
    /// Path to the `claude` binary (defaults to "claude").
    claude_bin: String,
}

impl ClaudeCodeRuntime {
    pub fn new() -> Self {
        Self {
            claude_bin: "claude".to_string(),
        }
    }

    pub fn with_binary(claude_bin: String) -> Self {
        Self { claude_bin }
    }

    /// Build the rendered prompt combining system context and user prompt.
    fn build_prompt(context: &StageContext) -> String {
        let mut prompt = context.rendered_prompt.clone();

        // If there's verify feedback from a prior failed verification, prepend it
        if let Some(ref feedback) = context.verify_feedback {
            if !feedback.is_empty() {
                prompt = format!(
                    "Previous verification feedback (address these issues):\n{}\n\n---\n\n{}",
                    feedback, prompt
                );
            }
        }

        // If there's prior context, prepend it
        if let Some(ref prior) = context.prior_summary {
            if !prior.is_empty() && !prompt.contains(prior) {
                prompt = format!(
                    "Context from previous stage:\n{}\n\n---\n\n{}",
                    prior, prompt
                );
            }
        }

        // If there are changed files listed, append them
        if !context.changed_files.is_empty() {
            let file_list = context
                .changed_files
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            prompt = format!(
                "{}\n\nFiles from previous stages:\n{}",
                prompt, file_list
            );
        }

        prompt
    }

    /// Build the MCP config JSON for `--mcp-config`.
    ///
    /// Generates a JSON object with `mcpServers` key containing stdio or SSE
    /// server configs suitable for Claude Code's `--mcp-config` flag.
    fn build_mcp_config_json(servers: &HashMap<String, McpServerConfig>) -> String {
        let mut mcp_servers = serde_json::Map::new();

        for (name, config) in servers {
            let mut server = serde_json::Map::new();

            if let Some(ref command) = config.command {
                // Stdio transport
                server.insert("command".to_string(), serde_json::Value::String(command.clone()));
                if !config.args.is_empty() {
                    server.insert(
                        "args".to_string(),
                        serde_json::Value::Array(
                            config.args.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
                        ),
                    );
                }
                if !config.env.is_empty() {
                    let env_obj: serde_json::Map<String, serde_json::Value> = config
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    server.insert("env".to_string(), serde_json::Value::Object(env_obj));
                }
            } else if let Some(ref url) = config.url {
                // HTTP/SSE transport
                server.insert("type".to_string(), serde_json::Value::String("sse".to_string()));
                server.insert("url".to_string(), serde_json::Value::String(url.clone()));
                if !config.headers.is_empty() {
                    let headers_obj: serde_json::Map<String, serde_json::Value> = config
                        .headers
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    server.insert("headers".to_string(), serde_json::Value::Object(headers_obj));
                }
            }

            mcp_servers.insert(name.clone(), serde_json::Value::Object(server));
        }

        let root = serde_json::json!({ "mcpServers": mcp_servers });
        serde_json::to_string(&root).unwrap_or_else(|_| "{}".to_string())
    }

    /// Build allowed tools flag for Claude CLI.
    fn build_tools_flag(allowed_tools: &[String]) -> Vec<String> {
        if allowed_tools.is_empty() {
            return vec![];
        }

        let tool_names: Vec<String> = allowed_tools
            .iter()
            .map(|t| match t.as_str() {
                "read" => "Read".to_string(),
                "write" => "Write".to_string(),
                "edit" => "Edit".to_string(),
                "bash" => "Bash".to_string(),
                "glob" => "Glob".to_string(),
                "grep" => "Grep".to_string(),
                "notebook" => "NotebookEdit".to_string(),
                other => other.to_string(),
            })
            .collect();

        vec![
            "--tools".to_string(),
            tool_names.join(","),
        ]
    }
}

impl Default for ClaudeCodeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Deserialize helper for stream-json lines. Uses serde_json::Value
/// for flexibility since the schema varies by message type.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct StreamLine {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
    // Tool use info (when type = "tool_use")
    #[serde(default)]
    tool: Option<serde_json::Value>,
    // Content block (for content_block_start)
    #[serde(default)]
    content_block: Option<serde_json::Value>,
    // Message wrapper (for assistant messages)
    #[serde(default)]
    message: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

use crate::util::truncate_chars;

/// Extract a human-readable activity description from a stream-json line.
fn extract_activity(line: &StreamLine) -> Option<String> {
    let msg_type = line.msg_type.as_deref()?;

    match msg_type {
        "assistant" => {
            // The actual content is in message.content[] — look for tool_use or text blocks
            if let Some(message) = &line.message {
                if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                    // Find the last content block (most recent action)
                    for block in content.iter().rev() {
                        let block_type = block.get("type").and_then(|t| t.as_str());
                        match block_type {
                            Some("tool_use") => {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown");
                                let summary = block
                                    .get("input")
                                    .and_then(|input| {
                                        input
                                            .get("file_path")
                                            .or_else(|| input.get("path"))
                                            .or_else(|| input.get("pattern"))
                                            .or_else(|| input.get("command"))
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .unwrap_or_default();
                                if summary.is_empty() {
                                    return Some(format!("Tool: {name}"));
                                }
                                return Some(format!("Tool: {name} — {summary}"));
                            }
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    return Some(truncate_chars(text, 80));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("Thinking...".to_string())
        }

        "user" => {
            // Tool results come back as type "user"
            if let Some(message) = &line.message {
                if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                    if content
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                    {
                        return Some("Processing tool result...".to_string());
                    }
                }
            }
            None
        }

        "result" => {
            let sub = line.subtype.as_deref().unwrap_or("done");
            Some(format!("Finished ({sub})"))
        }

        _ => None,
    }
}

/// Poll a cancel flag until it becomes true. Used with `tokio::select!`.
///
/// When no flag is provided, this future never resolves (equivalent to
/// `std::future::pending()`), which is the correct behavior for a
/// `select!` branch that should never win.
async fn cancel_poll(flag: &Arc<AtomicBool>) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[async_trait]
impl AgentRuntime for ClaudeCodeRuntime {
    fn name(&self) -> &str {
        "claude-code"
    }

    async fn execute(
        &self,
        config: &StageConfig,
        context: &StageContext,
        workspace_root: &Path,
        progress_tx: Option<mpsc::UnboundedSender<String>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        let prompt = Self::build_prompt(context);

        info!(
            stage_id = %config.id,
            model = %config.model,
            "Executing stage with Claude Code"
        );
        debug!(prompt_length = prompt.len(), "Built prompt");

        let use_streaming = progress_tx.is_some();

        let mut cmd = Command::new(&self.claude_bin);

        cmd.arg("--print")
            .arg("--output-format")
            .arg(if use_streaming { "stream-json" } else { "json" });

        // stream-json requires --verbose
        if use_streaming {
            cmd.arg("--verbose");
        }

        cmd.arg("--model")
            .arg(&config.model)
            .arg("--dangerously-skip-permissions")
            .arg("--append-system-prompt")
            .arg(&config.system_prompt);

        // Add allowed tools
        let tools_args = Self::build_tools_flag(&config.allowed_tools);
        for arg in &tools_args {
            cmd.arg(arg);
        }

        // Add MCP server config if any servers are attached to this stage
        let _mcp_tempfile = if !config.resolved_mcp_servers.is_empty() {
            let json = Self::build_mcp_config_json(&config.resolved_mcp_servers);
            let mut tmpfile = tempfile::NamedTempFile::new().map_err(|e| CoreError::AgentError {
                message: format!("Failed to create MCP config tempfile: {e}"),
            })?;
            tmpfile.write_all(json.as_bytes()).map_err(|e| CoreError::AgentError {
                message: format!("Failed to write MCP config: {e}"),
            })?;
            cmd.arg("--mcp-config").arg(tmpfile.path());
            debug!(
                mcp_servers = ?config.resolved_mcp_servers.keys().collect::<Vec<_>>(),
                path = %tmpfile.path().display(),
                "Attached MCP config"
            );
            Some(tmpfile)
        } else {
            None
        };

        cmd.current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(command = ?cmd, "Spawning Claude Code process");

        // Spawn the process and pipe the prompt via stdin to avoid
        // argument length limits and shell escaping issues.
        let mut child = cmd.spawn().map_err(|e| CoreError::AgentError {
            message: format!("Failed to spawn claude process: {e}"),
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await.map_err(|e| {
                CoreError::AgentError {
                    message: format!("Failed to write prompt to stdin: {e}"),
                }
            })?;
            // Drop stdin to close it, signaling EOF
        }

        if use_streaming {
            // Stream mode: read stdout line by line, parse, emit progress
            let stdout = child.stdout.take().ok_or_else(|| CoreError::AgentError {
                message: "Failed to capture stdout".to_string(),
            })?;

            let mut reader = BufReader::new(stdout).lines();
            let mut response_text = String::new();
            let mut token_usage = None;

            loop {
                // Race between reading next line and cancel polling
                let line_result = if let Some(ref flag) = cancel_flag {
                    tokio::select! {
                        result = reader.next_line() => result,
                        _ = cancel_poll(flag) => {
                            info!(stage_id = %config.id, "Cancellation requested, killing agent process");
                            let _ = child.kill().await;
                            return Err(CoreError::Cancelled {
                                stage_id: config.id.clone(),
                            });
                        }
                    }
                } else {
                    reader.next_line().await
                };

                let line = line_result.map_err(|e| CoreError::AgentError {
                    message: format!("Failed to read stdout: {e}"),
                })?;

                let Some(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Ok(parsed) = serde_json::from_str::<StreamLine>(trimmed) {
                    // Emit activity to TUI
                    if let Some(ref tx) = progress_tx {
                        if let Some(activity) = extract_activity(&parsed) {
                            let _ = tx.send(activity);
                        }
                    }

                    // Collect final result
                    if let Some(text) = parsed.result.or(parsed.content) {
                        if !text.is_empty() {
                            if response_text.len() + text.len() + 1 > MAX_RESPONSE_BYTES {
                                warn!(
                                    "Agent response exceeded {}MB limit, truncating",
                                    MAX_RESPONSE_BYTES / 1_048_576
                                );
                                let remaining =
                                    MAX_RESPONSE_BYTES.saturating_sub(response_text.len());
                                if remaining > 0 {
                                    if !response_text.is_empty() {
                                        response_text.push('\n');
                                    }
                                    response_text
                                        .push_str(&text[..remaining.min(text.len())]);
                                }
                                // Continue reading to drain pipe but don't accumulate
                            } else {
                                if !response_text.is_empty() {
                                    response_text.push('\n');
                                }
                                response_text.push_str(&text);
                            }
                        }
                    }

                    // Capture usage from the final "result" line
                    if let Some(u) = &parsed.usage {
                        token_usage = Some(TokenUsage {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                        });
                    }
                }
            }

            // Wait for the process to finish
            let status = child.wait().await.map_err(|e| CoreError::AgentError {
                message: format!("Failed to wait for claude process: {e}"),
            })?;

            if !status.success() {
                let mut stderr_buf = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = stderr.read_to_string(&mut stderr_buf).await;
                }
                return Err(CoreError::AgentError {
                    message: format!(
                        "Claude exited with status {}: {}",
                        status,
                        if stderr_buf.is_empty() { &response_text } else { &stderr_buf }
                    ),
                });
            }

            Ok(AgentOutput {
                response_text,
                token_usage,
            })
        } else {
            // Batch mode: drain stdout/stderr via spawned tasks BEFORE waiting,
            // to avoid pipe buffer deadlock. If the child produces more output
            // than the OS pipe buffer (~64KB), it blocks writing. Calling wait()
            // before draining the pipes would deadlock.
            let stdout_handle = {
                let mut out = child.stdout.take();
                tokio::spawn(async move {
                    let mut buf = String::new();
                    if let Some(ref mut stream) = out {
                        use tokio::io::AsyncReadExt;
                        let _ = stream.read_to_string(&mut buf).await;
                    }
                    buf
                })
            };
            let stderr_handle = {
                let mut err = child.stderr.take();
                tokio::spawn(async move {
                    let mut buf = String::new();
                    if let Some(ref mut stream) = err {
                        use tokio::io::AsyncReadExt;
                        let _ = stream.read_to_string(&mut buf).await;
                    }
                    buf
                })
            };

            let wait_result = if let Some(ref flag) = cancel_flag {
                tokio::select! {
                    result = child.wait() => Some(result),
                    _ = cancel_poll(flag) => {
                        let _ = child.kill().await;
                        return Err(CoreError::Cancelled {
                            stage_id: config.id.clone(),
                        });
                    }
                }
            } else {
                Some(child.wait().await)
            };

            let status = wait_result
                .unwrap()
                .map_err(|e| CoreError::AgentError {
                    message: format!("Failed to wait for claude process: {e}"),
                })?;

            let stdout = stdout_handle.await.unwrap_or_default();
            let stderr = stderr_handle.await.unwrap_or_default();

            if !status.success() {
                warn!(
                    exit_code = ?status.code(),
                    stderr = %stderr,
                    "Claude Code process exited with non-zero status"
                );
                return Err(CoreError::AgentError {
                    message: format!(
                        "Claude exited with status {}: {}",
                        status,
                        if stderr.is_empty() { &stdout } else { &stderr }
                    ),
                });
            }

            debug!(stdout_len = stdout.len(), "Claude Code process completed");

            let (mut response_text, token_usage) = parse_claude_output(&stdout);

            if response_text.len() > MAX_RESPONSE_BYTES {
                warn!(
                    "Agent response exceeded {}MB limit, truncating",
                    MAX_RESPONSE_BYTES / 1_048_576
                );
                response_text.truncate(MAX_RESPONSE_BYTES);
            }

            Ok(AgentOutput {
                response_text,
                token_usage,
            })
        }
    }

    async fn health_check(&self) -> CoreResult<()> {
        let output = Command::new(&self.claude_bin)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CoreError::RuntimeUnavailable {
                runtime: self.name().to_string(),
                reason: format!("Failed to run '{} --version': {e}", self.claude_bin),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::RuntimeUnavailable {
                runtime: self.name().to_string(),
                reason: format!("claude --version failed: {stderr}"),
            });
        }

        let version = String::from_utf8_lossy(&output.stdout);
        info!(version = %version.trim(), "Claude Code CLI is available");
        Ok(())
    }
}

/// Parse Claude Code JSON output into response text and optional token usage.
/// Used in batch (non-streaming) mode.
fn parse_claude_output(stdout: &str) -> (String, Option<TokenUsage>) {
    let trimmed = stdout.trim();

    // Try parsing as a single JSON object first
    if let Ok(result) = serde_json::from_str::<StreamLine>(trimmed) {
        let text = result
            .result
            .or(result.content)
            .unwrap_or_default();
        let usage = result.usage.map(|u| TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        });
        return (text, usage);
    }

    // Try parsing as newline-delimited JSON (streaming format)
    let mut response_text = String::new();
    let mut token_usage = None;

    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<StreamLine>(line) {
            // Collect result/content text
            if let Some(text) = msg.result.or(msg.content) {
                if !text.is_empty() {
                    if !response_text.is_empty() {
                        response_text.push('\n');
                    }
                    response_text.push_str(&text);
                }
            }
            // Capture last usage
            if let Some(u) = msg.usage {
                token_usage = Some(TokenUsage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                });
            }
        }
    }

    // Fallback: return raw stdout if nothing parsed
    if response_text.is_empty() {
        response_text = trimmed.to_string();
    }

    (response_text, token_usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_claude_json_result() {
        let json = r#"{"type":"result","result":"Hello, world!","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let (text, usage) = parse_claude_output(json);
        assert_eq!(text, "Hello, world!");
        assert!(usage.is_some());
        let u = usage.unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
    }

    #[test]
    fn test_parse_plain_text_fallback() {
        let output = "Just some plain text response";
        let (text, usage) = parse_claude_output(output);
        assert_eq!(text, "Just some plain text response");
        assert!(usage.is_none());
    }

    #[test]
    fn test_build_tools_flag() {
        let tools = vec!["read".to_string(), "write".to_string(), "bash".to_string()];
        let flags = ClaudeCodeRuntime::build_tools_flag(&tools);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0], "--tools");
        assert_eq!(flags[1], "Read,Write,Bash");
    }

    #[test]
    fn test_build_tools_flag_empty() {
        let flags = ClaudeCodeRuntime::build_tools_flag(&[]);
        assert!(flags.is_empty());
    }

    #[test]
    fn test_extract_activity_tool_use() {
        // Tool use comes as type "assistant" with message.content[].type = "tool_use"
        let line = StreamLine {
            msg_type: Some("assistant".to_string()),
            message: Some(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "name": "Read",
                    "input": {"file_path": "src/main.rs"}
                }]
            })),
            ..default_stream_line()
        };
        let activity = extract_activity(&line);
        assert_eq!(activity, Some("Tool: Read — src/main.rs".to_string()));
    }

    #[test]
    fn test_extract_activity_assistant_text() {
        // Text content in assistant message
        let line = StreamLine {
            msg_type: Some("assistant".to_string()),
            message: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": "I'll create that file for you."
                }]
            })),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("I'll create that file for you.".to_string())
        );
    }

    #[test]
    fn test_extract_activity_assistant_no_message() {
        // Fallback when no message content
        let line = StreamLine {
            msg_type: Some("assistant".to_string()),
            ..default_stream_line()
        };
        assert_eq!(extract_activity(&line), Some("Thinking...".to_string()));
    }

    #[test]
    fn test_extract_activity_result() {
        let line = StreamLine {
            msg_type: Some("result".to_string()),
            subtype: Some("success".to_string()),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("Finished (success)".to_string())
        );
    }

    fn default_stream_line() -> StreamLine {
        StreamLine {
            msg_type: None,
            subtype: None,
            result: None,
            content: None,
            usage: None,
            tool: None,
            content_block: None,
            message: None,
        }
    }

    // --- parse_claude_output: streaming format ---

    #[test]
    fn test_parse_claude_output_stream_format() {
        // Newline-delimited JSON with multiple lines
        let output = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Working..."}]}}
{"type":"result","result":"Done!","usage":{"input_tokens":200,"output_tokens":100}}"#;
        let (text, usage) = parse_claude_output(output);
        assert_eq!(text, "Done!");
        let u = usage.unwrap();
        assert_eq!(u.input_tokens, 200);
        assert_eq!(u.output_tokens, 100);
    }

    #[test]
    fn test_parse_claude_output_content_field() {
        let json = r#"{"type":"result","content":"Hello via content!"}"#;
        let (text, usage) = parse_claude_output(json);
        assert_eq!(text, "Hello via content!");
        assert!(usage.is_none());
    }

    #[test]
    fn test_parse_claude_output_multiple_results() {
        let output = r#"{"type":"result","result":"Part 1"}
{"type":"result","result":"Part 2"}"#;
        let (text, _) = parse_claude_output(output);
        assert_eq!(text, "Part 1\nPart 2");
    }

    #[test]
    fn test_parse_claude_output_empty_lines_skipped() {
        let output = r#"{"type":"result","result":"Hello"}

"#;
        let (text, _) = parse_claude_output(output);
        assert_eq!(text, "Hello");
    }

    // --- extract_activity gaps ---

    #[test]
    fn test_extract_activity_user_tool_result() {
        let line = StreamLine {
            msg_type: Some("user".to_string()),
            message: Some(serde_json::json!({
                "content": [{"type": "tool_result", "content": "done"}]
            })),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("Processing tool result...".to_string())
        );
    }

    #[test]
    fn test_extract_activity_user_no_tool_result() {
        let line = StreamLine {
            msg_type: Some("user".to_string()),
            message: Some(serde_json::json!({
                "content": [{"type": "text", "text": "hello"}]
            })),
            ..default_stream_line()
        };
        assert_eq!(extract_activity(&line), None);
    }

    #[test]
    fn test_extract_activity_result_no_subtype() {
        let line = StreamLine {
            msg_type: Some("result".to_string()),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("Finished (done)".to_string())
        );
    }

    #[test]
    fn test_extract_activity_unknown_type() {
        let line = StreamLine {
            msg_type: Some("unknown_type".to_string()),
            ..default_stream_line()
        };
        assert_eq!(extract_activity(&line), None);
    }

    #[test]
    fn test_extract_activity_tool_use_with_command() {
        let line = StreamLine {
            msg_type: Some("assistant".to_string()),
            message: Some(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "name": "Bash",
                    "input": {"command": "cargo test"}
                }]
            })),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("Tool: Bash \u{2014} cargo test".to_string())
        );
    }

    #[test]
    fn test_extract_activity_tool_use_with_pattern() {
        let line = StreamLine {
            msg_type: Some("assistant".to_string()),
            message: Some(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "name": "Glob",
                    "input": {"pattern": "**/*.rs"}
                }]
            })),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("Tool: Glob \u{2014} **/*.rs".to_string())
        );
    }

    #[test]
    fn test_extract_activity_tool_use_no_recognized_input() {
        let line = StreamLine {
            msg_type: Some("assistant".to_string()),
            message: Some(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "name": "CustomTool",
                    "input": {"data": "something"}
                }]
            })),
            ..default_stream_line()
        };
        assert_eq!(
            extract_activity(&line),
            Some("Tool: CustomTool".to_string())
        );
    }

    // --- build_tools_flag: more mappings ---

    #[test]
    fn test_build_tools_flag_all_mappings() {
        let tools = vec![
            "edit".to_string(),
            "glob".to_string(),
            "grep".to_string(),
            "notebook".to_string(),
        ];
        let flags = ClaudeCodeRuntime::build_tools_flag(&tools);
        assert_eq!(flags[1], "Edit,Glob,Grep,NotebookEdit");
    }

    #[test]
    fn test_build_tools_flag_passthrough() {
        let tools = vec!["CustomTool".to_string()];
        let flags = ClaudeCodeRuntime::build_tools_flag(&tools);
        assert_eq!(flags[1], "CustomTool");
    }

    // --- build_prompt tests ---

    #[test]
    fn test_build_prompt_with_prior_summary() {
        let context = StageContext {
            rendered_prompt: "Do the task".into(),
            prior_summary: Some("Previously created files".into()),
            changed_files: vec![],
            verify_feedback: None,
        };
        let prompt = ClaudeCodeRuntime::build_prompt(&context);
        assert!(prompt.contains("Context from previous stage:"));
        assert!(prompt.contains("Previously created files"));
        assert!(prompt.contains("Do the task"));
    }

    #[test]
    fn test_build_prompt_with_verify_feedback() {
        let context = StageContext {
            rendered_prompt: "Fix the bug".into(),
            prior_summary: None,
            changed_files: vec![],
            verify_feedback: Some("Tests are still failing".into()),
        };
        let prompt = ClaudeCodeRuntime::build_prompt(&context);
        assert!(prompt.contains("Previous verification feedback"));
        assert!(prompt.contains("Tests are still failing"));
        assert!(prompt.contains("Fix the bug"));
    }

    #[test]
    fn test_build_prompt_with_changed_files() {
        let context = StageContext {
            rendered_prompt: "Review changes".into(),
            prior_summary: None,
            changed_files: vec!["src/main.rs".into(), "README.md".into()],
            verify_feedback: None,
        };
        let prompt = ClaudeCodeRuntime::build_prompt(&context);
        assert!(prompt.contains("Files from previous stages:"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("README.md"));
    }

    #[test]
    fn test_build_prompt_prior_summary_already_in_prompt() {
        let context = StageContext {
            rendered_prompt: "Summary: Previously created files".into(),
            prior_summary: Some("Previously created files".into()),
            changed_files: vec![],
            verify_feedback: None,
        };
        let prompt = ClaudeCodeRuntime::build_prompt(&context);
        // Should NOT prepend since it's already in the prompt
        assert!(!prompt.contains("Context from previous stage:"));
    }

    // --- MCP config JSON tests ---

    #[test]
    fn test_build_mcp_config_json_stdio() {
        let mut servers = HashMap::new();
        servers.insert(
            "database".to_string(),
            McpServerConfig {
                command: Some("npx".to_string()),
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-postgres".to_string()],
                env: {
                    let mut m = HashMap::new();
                    m.insert("DATABASE_URL".to_string(), "postgresql://localhost/mydb".to_string());
                    m
                },
                url: None,
                headers: HashMap::new(),
            },
        );

        let json = ClaudeCodeRuntime::build_mcp_config_json(&servers);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let db = &parsed["mcpServers"]["database"];
        assert_eq!(db["command"], "npx");
        assert_eq!(db["args"][0], "-y");
        assert_eq!(db["args"][1], "@modelcontextprotocol/server-postgres");
        assert_eq!(db["env"]["DATABASE_URL"], "postgresql://localhost/mydb");
        assert!(db.get("type").is_none());
        assert!(db.get("url").is_none());
    }

    #[test]
    fn test_build_mcp_config_json_sse() {
        let mut servers = HashMap::new();
        servers.insert(
            "api-docs".to_string(),
            McpServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                url: Some("https://api-docs.example.com/mcp".to_string()),
                headers: HashMap::new(),
            },
        );

        let json = ClaudeCodeRuntime::build_mcp_config_json(&servers);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let api = &parsed["mcpServers"]["api-docs"];
        assert_eq!(api["type"], "sse");
        assert_eq!(api["url"], "https://api-docs.example.com/mcp");
        assert!(api.get("command").is_none());
        assert!(api.get("headers").is_none());
    }

    #[test]
    fn test_build_mcp_config_json_sse_with_headers() {
        let mut servers = HashMap::new();
        servers.insert(
            "authed-api".to_string(),
            McpServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                url: Some("https://api.example.com/mcp".to_string()),
                headers: {
                    let mut h = HashMap::new();
                    h.insert("Authorization".to_string(), "Bearer sk-test-123".to_string());
                    h.insert("X-Custom".to_string(), "value".to_string());
                    h
                },
            },
        );

        let json = ClaudeCodeRuntime::build_mcp_config_json(&servers);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let api = &parsed["mcpServers"]["authed-api"];
        assert_eq!(api["type"], "sse");
        assert_eq!(api["url"], "https://api.example.com/mcp");
        assert_eq!(api["headers"]["Authorization"], "Bearer sk-test-123");
        assert_eq!(api["headers"]["X-Custom"], "value");
    }

    #[test]
    fn test_build_mcp_config_json_empty() {
        let servers = HashMap::new();
        let json = ClaudeCodeRuntime::build_mcp_config_json(&servers);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["mcpServers"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_build_mcp_config_json_stdio_minimal() {
        let mut servers = HashMap::new();
        servers.insert(
            "simple".to_string(),
            McpServerConfig {
                command: Some("my-server".to_string()),
                args: vec![],
                env: HashMap::new(),
                url: None,
                headers: HashMap::new(),
            },
        );

        let json = ClaudeCodeRuntime::build_mcp_config_json(&servers);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let s = &parsed["mcpServers"]["simple"];
        assert_eq!(s["command"], "my-server");
        // No args or env keys should be present
        assert!(s.get("args").is_none());
        assert!(s.get("env").is_none());
    }
}
