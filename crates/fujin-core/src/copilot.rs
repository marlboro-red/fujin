use crate::agent::{AgentOutput, AgentRuntime};
use crate::context::StageContext;
use crate::error::{CoreError, CoreResult};
use crate::stage::TokenUsage;
use async_trait::async_trait;
use fujin_config::StageConfig;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Maximum response size (50 MB) to prevent unbounded memory growth.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// GitHub Copilot CLI agent runtime.
///
/// Spawns `copilot` as a subprocess for each stage using programmatic mode
/// (`-p -s`). Copilot CLI handles file I/O and tool execution natively.
///
/// Note: Copilot CLI in programmatic mode does not provide structured JSON
/// output or token usage data. The TUI will show generic progress instead
/// of per-tool activity.
pub struct CopilotCliRuntime {
    /// Path to the `copilot` binary (defaults to "copilot").
    copilot_bin: String,
}

/// Resolved node + script path from an npm `.cmd` shim (Windows only).
#[cfg(windows)]
struct ResolvedShim {
    node: String,
    script: String,
}

/// Parse an npm `.cmd` shim to extract the node binary and JS entry point.
///
/// npm shims follow a predictable pattern: the last line invokes
/// `"%_prog%" "%dp0%\node_modules\...\entry.js" %*`. We locate the
/// `.cmd` file on PATH, read it, and extract the JS path.
#[cfg(windows)]
fn resolve_cmd_shim(bin_name: &str) -> Option<ResolvedShim> {
    use std::fs;

    let cmd_name = if bin_name.ends_with(".cmd") {
        bin_name.to_string()
    } else {
        format!("{bin_name}.cmd")
    };

    let cmd_path = std::env::var("PATH").ok().and_then(|path| {
        path.split(';').find_map(|dir| {
            let candidate = std::path::PathBuf::from(dir).join(&cmd_name);
            candidate.exists().then_some(candidate)
        })
    })?;

    let contents = fs::read_to_string(&cmd_path).ok()?;
    let dir = cmd_path.parent()?;

    // Extract the JS entry point: "%dp0%\node_modules\...\something.js"
    let js_path = contents
        .lines()
        .rev()
        .find_map(|line| {
            let lower = line.to_lowercase();
            if !lower.contains("%dp0%") || !lower.contains(".js") {
                return None;
            }
            let start = line.find("%dp0%")?;
            let after = &line[start + 5..]; // skip %dp0%
            let after = after.strip_prefix('\\')?;
            let end = after.find('"').or_else(|| after.find(' ')).unwrap_or(after.len());
            Some(after[..end].to_string())
        })?;

    let script = dir.join(&js_path);
    if !script.exists() {
        return None;
    }

    let node_in_dir = dir.join("node.exe");
    let node = if node_in_dir.exists() {
        node_in_dir.to_string_lossy().to_string()
    } else {
        "node".to_string()
    };

    Some(ResolvedShim {
        node,
        script: script.to_string_lossy().to_string(),
    })
}

impl CopilotCliRuntime {
    pub fn new() -> Self {
        Self {
            copilot_bin: "copilot".to_string(),
        }
    }

    pub fn with_binary(copilot_bin: String) -> Self {
        Self { copilot_bin }
    }

    /// Consume self and return a `Command` for the copilot binary.
    /// Used by the summarizer in `context.rs`.
    pub fn into_command(self) -> Command {
        self.command()
    }

    /// Build a `Command` that invokes the copilot binary.
    ///
    /// On Windows, npm-installed binaries are `.cmd` shims that cause
    /// argument quoting issues when invoked via Rust's `Command` API.
    /// We resolve the shim to invoke node directly with the JS entry point.
    #[cfg(windows)]
    fn command(&self) -> Command {
        if let Some(resolved) = resolve_cmd_shim(&self.copilot_bin) {
            let mut cmd = Command::new(&resolved.node);
            cmd.arg(&resolved.script);
            return cmd;
        }
        Command::new(&self.copilot_bin)
    }

    #[cfg(not(windows))]
    fn command(&self) -> Command {
        Command::new(&self.copilot_bin)
    }

    /// Build the rendered prompt combining system context and user prompt.
    ///
    /// Since Copilot CLI has no `--append-system-prompt` flag, the system
    /// prompt is embedded directly into the user prompt.
    fn build_prompt(config: &StageConfig, context: &StageContext) -> String {
        let mut prompt = String::new();

        // Embed system prompt as a preamble (Copilot CLI has no separate system prompt flag)
        if !config.system_prompt.is_empty() {
            prompt.push_str("<system>\n");
            prompt.push_str(&config.system_prompt);
            prompt.push_str("\n</system>\n\n");
        }

        // Add prior context then verify feedback (same order as ClaudeCodeRuntime)
        if let Some(ref prior) = context.prior_summary {
            if !prior.is_empty() && !context.rendered_prompt.contains(prior) {
                prompt.push_str("Context from previous stage:\n");
                prompt.push_str(prior);
                prompt.push_str("\n\n---\n\n");
            }
        }

        if let Some(ref feedback) = context.verify_feedback {
            if !feedback.is_empty() {
                prompt.push_str("Previous verification feedback (address these issues):\n");
                prompt.push_str(feedback);
                prompt.push_str("\n\n---\n\n");
            }
        }

        prompt.push_str(&context.rendered_prompt);

        // Append changed files list
        if !context.changed_files.is_empty() {
            let file_list = context
                .changed_files
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            prompt.push_str("\n\nFiles from previous stages:\n");
            prompt.push_str(&file_list);
        }

        prompt
    }

    /// Build allowed tools flags for Copilot CLI.
    ///
    /// Copilot CLI uses `--allow-tool <name>` per tool (not comma-separated).
    /// Tool name mapping differs from Claude Code.
    fn build_tools_flags(allowed_tools: &[String]) -> Vec<String> {
        if allowed_tools.is_empty() {
            return vec!["--allow-all-tools".to_string()];
        }

        allowed_tools
            .iter()
            .flat_map(|t| {
                let tool_name = match t.as_str() {
                    "read" => "Read",
                    "write" => "Write",
                    "edit" => "Edit",
                    "bash" => "shell",
                    "glob" => "Glob",
                    "grep" => "Grep",
                    "notebook" => "NotebookEdit",
                    other => other,
                };
                vec!["--allow-tool".to_string(), tool_name.to_string()]
            })
            .collect()
    }
}

impl Default for CopilotCliRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Poll a cancel flag until it becomes true. Used with `tokio::select!`.
async fn cancel_poll(flag: &Arc<AtomicBool>) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Parse copilot CLI stderr for usage statistics.
///
/// Stderr format:
/// ```text
/// Total usage est:        2 Premium requests
/// Breakdown by AI model:
///  gpt-5.3-codex           15.0k in, 392 out, 0 cached (Est. 2 Premium requests)
/// ```
fn parse_copilot_usage(stderr: &str) -> Option<TokenUsage> {
    if stderr.is_empty() {
        return None;
    }

    let mut premium_requests: Option<u32> = None;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;

    for line in stderr.lines() {
        let trimmed = line.trim();

        // "Total usage est:        2 Premium requests"
        if trimmed.starts_with("Total usage est:") {
            if let Some(rest) = trimmed.strip_prefix("Total usage est:") {
                let rest = rest.trim();
                if let Some(num_str) = rest.split_whitespace().next() {
                    premium_requests = num_str.parse().ok();
                }
            }
        }

        // " gpt-5.3-codex           15.0k in, 392 out, 0 cached (Est. 2 Premium requests)"
        if trimmed.contains(" in,") && trimmed.contains(" out,") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            for (i, part) in parts.iter().enumerate() {
                if *part == "in," && i > 0 {
                    input_tokens += parse_token_count(parts[i - 1]);
                }
                if *part == "out," && i > 0 {
                    output_tokens += parse_token_count(parts[i - 1]);
                }
            }
        }
    }

    if premium_requests.is_some() || input_tokens > 0 || output_tokens > 0 {
        Some(TokenUsage {
            input_tokens,
            output_tokens,
            premium_requests,
        })
    } else {
        None
    }
}

/// Parse token count strings like "15.0k", "1.2M", "392".
fn parse_token_count(s: &str) -> u64 {
    let s = s.replace(',', "");
    if let Some(num) = s.strip_suffix('k').or_else(|| s.strip_suffix('K')) {
        num.parse::<f64>().map(|n| (n * 1_000.0) as u64).unwrap_or(0)
    } else if let Some(num) = s.strip_suffix('M') {
        num.parse::<f64>().map(|n| (n * 1_000_000.0) as u64).unwrap_or(0)
    } else {
        s.parse().unwrap_or(0)
    }
}

#[async_trait]
impl AgentRuntime for CopilotCliRuntime {
    fn name(&self) -> &str {
        "copilot-cli"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        config: &StageConfig,
        context: &StageContext,
        workspace_root: &Path,
        progress_tx: Option<mpsc::UnboundedSender<String>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        if !config.resolved_mcp_servers.is_empty() {
            warn!(
                stage_id = %config.id,
                mcp_servers = ?config.resolved_mcp_servers.keys().collect::<Vec<_>>(),
                "MCP servers are not supported by copilot-cli runtime; ignoring"
            );
        }

        let prompt = Self::build_prompt(config, context);

        info!(
            stage_id = %config.id,
            model = %config.model,
            "Executing stage with Copilot CLI"
        );
        debug!(prompt_length = prompt.len(), "Built prompt");

        let mut cmd = self.command();

        // Programmatic mode: -p takes the prompt as a string argument.
        // --stream on enables streaming so we can capture live tool activity.
        // --no-ask-user prevents the agent from prompting interactively.
        cmd.arg("-p").arg(&prompt);
        cmd.arg("--stream").arg("on");
        cmd.arg("--no-ask-user");

        cmd.arg("--model").arg(&config.model);

        // Skip interactive permission prompts (equivalent to Claude's
        // --dangerously-skip-permissions).
        cmd.arg("--yolo");

        // Add tool permissions
        let tools_flags = Self::build_tools_flags(&config.allowed_tools);
        for flag in &tools_flags {
            cmd.arg(flag);
        }

        cmd.current_dir(workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(command = ?cmd, "Spawning Copilot CLI process");

        let mut child = cmd.spawn().map_err(|e| CoreError::AgentError {
            message: format!("Failed to spawn copilot process: {e}"),
        })?;

        // Stream stdout line-by-line to extract tool activity for the TUI.
        // Copilot CLI with --stream on emits lines like:
        //   ✅ Read file src/main.rs
        //   ⠋ Searching files...
        // We forward these as progress messages and collect the full output.
        let stdout_handle = {
            let stdout = child.stdout.take();
            let tx = progress_tx.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut full_output = String::new();
                if let Some(stream) = stdout {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line).await {
                            Ok(0) => break, // EOF
                            Ok(_) => {
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    // Forward tool activity lines to the TUI
                                    if let Some(ref tx) = tx {
                                        let _ = tx.send(trimmed.to_string());
                                    }
                                }
                                full_output.push_str(&line);
                                if full_output.len() > MAX_RESPONSE_BYTES {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                full_output
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

        // Wait for process exit, with cancellation support
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
                message: format!("Failed to wait for copilot process: {e}"),
            })?;

        let stdout = stdout_handle.await.unwrap_or_default();
        let stderr = stderr_handle.await.unwrap_or_default();

        if !status.success() {
            warn!(
                exit_code = ?status.code(),
                stderr = %stderr,
                "Copilot CLI process exited with non-zero status"
            );
            return Err(CoreError::AgentError {
                message: format!(
                    "Copilot CLI exited with status {}: {}",
                    status,
                    if stderr.is_empty() { &stdout } else { &stderr }
                ),
            });
        }

        debug!(stdout_len = stdout.len(), "Copilot CLI process completed");

        let mut response_text = stdout.trim().to_string();

        if response_text.len() > MAX_RESPONSE_BYTES {
            warn!(
                "Agent response exceeded {}MB limit, truncating",
                MAX_RESPONSE_BYTES / 1_048_576
            );
            response_text.truncate(MAX_RESPONSE_BYTES);
        }

        // Parse usage stats from stderr (premium requests + tokens)
        let token_usage = parse_copilot_usage(&stderr);

        Ok(AgentOutput {
            response_text,
            token_usage,
        })
    }

    async fn health_check(&self) -> CoreResult<()> {
        let output = self.command()
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CoreError::RuntimeUnavailable {
                runtime: self.name().to_string(),
                reason: format!("Failed to run '{} --version': {e}", self.copilot_bin),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::RuntimeUnavailable {
                runtime: self.name().to_string(),
                reason: format!("copilot --version failed: {stderr}"),
            });
        }

        let version = String::from_utf8_lossy(&output.stdout);
        info!(version = %version.trim(), "Copilot CLI is available");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tools_flags_empty() {
        let flags = CopilotCliRuntime::build_tools_flags(&[]);
        assert_eq!(flags, vec!["--allow-all-tools"]);
    }

    #[test]
    fn test_build_tools_flags_mapped() {
        let tools = vec!["read".to_string(), "write".to_string(), "bash".to_string()];
        let flags = CopilotCliRuntime::build_tools_flags(&tools);
        assert_eq!(
            flags,
            vec![
                "--allow-tool", "Read",
                "--allow-tool", "Write",
                "--allow-tool", "shell",
            ]
        );
    }

    #[test]
    fn test_build_tools_flags_passthrough() {
        let tools = vec!["fetch".to_string(), "websearch".to_string()];
        let flags = CopilotCliRuntime::build_tools_flags(&tools);
        assert_eq!(
            flags,
            vec![
                "--allow-tool", "fetch",
                "--allow-tool", "websearch",
            ]
        );
    }

    #[test]
    fn test_build_prompt_with_system() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: "You are a coding assistant.".to_string(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        let context = StageContext {
            rendered_prompt: "Write hello world".to_string(),
            prior_summary: None,
            changed_files: vec![],
            verify_feedback: None,
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        assert!(prompt.contains("<system>"));
        assert!(prompt.contains("You are a coding assistant."));
        assert!(prompt.contains("</system>"));
        assert!(prompt.contains("Write hello world"));
    }

    #[test]
    fn test_build_prompt_no_system() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: String::new(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        let context = StageContext {
            rendered_prompt: "Write hello world".to_string(),
            prior_summary: None,
            changed_files: vec![],
            verify_feedback: None,
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        assert!(!prompt.contains("<system>"));
        assert_eq!(prompt, "Write hello world");
    }

    #[test]
    fn test_build_prompt_with_prior_summary() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: String::new(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        let context = StageContext {
            rendered_prompt: "Continue the work".to_string(),
            prior_summary: Some("Previous stage created main.rs".to_string()),
            changed_files: vec![],
            verify_feedback: None,
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        assert!(prompt.contains("Context from previous stage:"));
        assert!(prompt.contains("Previous stage created main.rs"));
        assert!(prompt.contains("Continue the work"));
    }

    #[test]
    fn test_build_prompt_prior_summary_already_in_prompt() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: String::new(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        // If prior_summary is already embedded in the rendered prompt, it should not be duplicated
        let context = StageContext {
            rendered_prompt: "Do work. Previous stage created main.rs".to_string(),
            prior_summary: Some("Previous stage created main.rs".to_string()),
            changed_files: vec![],
            verify_feedback: None,
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        assert!(!prompt.contains("Context from previous stage:"));
    }

    #[test]
    fn test_build_prompt_with_verify_feedback() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: String::new(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        let context = StageContext {
            rendered_prompt: "Fix the issues".to_string(),
            prior_summary: None,
            changed_files: vec![],
            verify_feedback: Some("Missing null checks in foo()".to_string()),
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        assert!(prompt.contains("Previous verification feedback"));
        assert!(prompt.contains("Missing null checks in foo()"));
        assert!(prompt.contains("Fix the issues"));
    }

    #[test]
    fn test_build_prompt_with_changed_files() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: String::new(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        let context = StageContext {
            rendered_prompt: "Review the files".to_string(),
            prior_summary: None,
            changed_files: vec![
                std::path::PathBuf::from("src/main.rs"),
                std::path::PathBuf::from("Cargo.toml"),
            ],
            verify_feedback: None,
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        assert!(prompt.contains("Files from previous stages:"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Cargo.toml"));
    }

    #[test]
    fn test_build_prompt_all_sections() {
        let config = StageConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            runtime: None,
            model: "claude-sonnet-4".to_string(),
            system_prompt: "Be careful.".to_string(),
            user_prompt: "".to_string(),
            timeout_secs: None,
            allowed_tools: vec![],
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
            mcp_servers: Vec::new(),
            resolved_mcp_servers: std::collections::HashMap::new(),
        };
        let context = StageContext {
            rendered_prompt: "Do the work".to_string(),
            prior_summary: Some("Created files".to_string()),
            changed_files: vec![std::path::PathBuf::from("output.txt")],
            verify_feedback: Some("Tests failing".to_string()),
        };

        let prompt = CopilotCliRuntime::build_prompt(&config, &context);
        // All sections should be present in order
        let system_pos = prompt.find("<system>").unwrap();
        let prior_pos = prompt.find("Context from previous stage:").unwrap();
        let feedback_pos = prompt.find("Previous verification feedback").unwrap();
        let user_pos = prompt.find("Do the work").unwrap();
        let files_pos = prompt.find("Files from previous stages:").unwrap();

        assert!(system_pos < prior_pos);
        assert!(prior_pos < feedback_pos);
        assert!(feedback_pos < user_pos);
        assert!(user_pos < files_pos);
    }

    #[test]
    fn test_copilot_runtime_name() {
        let runtime = CopilotCliRuntime::new();
        assert_eq!(runtime.name(), "copilot-cli");
    }

    #[test]
    fn test_copilot_supports_streaming() {
        let runtime = CopilotCliRuntime::new();
        assert!(runtime.supports_streaming());
    }

    #[test]
    fn test_copilot_with_binary() {
        let runtime = CopilotCliRuntime::with_binary("/usr/local/bin/copilot".to_string());
        assert_eq!(runtime.copilot_bin, "/usr/local/bin/copilot");
    }

    #[test]
    fn test_copilot_default() {
        let runtime = CopilotCliRuntime::default();
        assert_eq!(runtime.copilot_bin, "copilot");
    }

    #[test]
    fn test_parse_copilot_usage() {
        let stderr = "\
Total usage est:        2 Premium requests
API time spent:         5s
Total session time:     11s
Total code changes:     +0 -0
Breakdown by AI model:
 gpt-5.3-codex           15.0k in, 392 out, 0 cached (Est. 2 Premium requests)";

        let usage = parse_copilot_usage(stderr).unwrap();
        assert_eq!(usage.premium_requests, Some(2));
        assert_eq!(usage.input_tokens, 15000);
        assert_eq!(usage.output_tokens, 392);
    }

    #[test]
    fn test_parse_copilot_usage_empty() {
        assert!(parse_copilot_usage("").is_none());
    }

    #[test]
    fn test_parse_token_count() {
        assert_eq!(parse_token_count("392"), 392);
        assert_eq!(parse_token_count("15.0k"), 15000);
        assert_eq!(parse_token_count("1.2M"), 1200000);
    }
}
