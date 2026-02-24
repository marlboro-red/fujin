use crate::artifact::ArtifactSet;
use crate::error::{CoreError, CoreResult};
use crate::stage::StageResult;
use crate::util::truncate_chars;
use crate::workspace::Workspace;
use handlebars::Handlebars;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info, warn};
use fujin_config::{PipelineConfig, StageConfig, SummarizerConfig};

use crate::copilot::CopilotCliRuntime;

/// Build a `Command` for invoking copilot, resolving .cmd shims on Windows.
fn copilot_command() -> Command {
    CopilotCliRuntime::new().into_command()
}

/// Context passed to a stage during execution.
#[derive(Debug, Clone)]
pub struct StageContext {
    /// The rendered user prompt (template variables resolved).
    pub rendered_prompt: String,

    /// Summary of the prior stage (if any).
    pub prior_summary: Option<String>,

    /// Files changed in prior stages.
    pub changed_files: Vec<PathBuf>,

    /// Feedback from a failed verification agent (retry groups only).
    /// Present when the stage is being re-executed after a verify FAIL.
    pub verify_feedback: Option<String>,
}

/// Builds context for each stage, including template rendering and summarization.
///
/// The summarizer uses the pipeline's configured runtime to invoke the
/// appropriate CLI tool for text summarization.
pub struct ContextBuilder {
    /// Name of the runtime to use for summarization.
    /// Determines which CLI binary and flags to use.
    pub(crate) runtime_name: String,
}

impl ContextBuilder {
    pub fn new() -> Self {
        Self {
            runtime_name: "claude-code".to_string(),
        }
    }

    pub fn with_runtime(runtime_name: String) -> Self {
        Self { runtime_name }
    }

    /// Build the context for a stage, incorporating prior results.
    ///
    /// `prior_results` contains results from all direct parent stages (DAG
    /// dependencies). For stages with a single parent this is equivalent to
    /// the old `Option<&StageResult>` behavior. For multiple parents,
    /// summaries are concatenated and artifact sets are unioned.
    ///
    /// Per-stage template variables like `{{stages.<id>.summary}}` are
    /// populated for every completed stage in `all_completed`.
    ///
    /// `extra_vars` contains variables exported by earlier stages (via `exports`).
    /// These are merged after config variables, so exports override YAML defaults.
    #[allow(clippy::too_many_arguments)]
    pub async fn build(
        &self,
        config: &PipelineConfig,
        stage_config: &StageConfig,
        prior_results: &[&StageResult],
        workspace: &Workspace,
        verify_feedback: Option<&str>,
        all_completed: &[StageResult],
        extra_vars: &HashMap<String, String>,
    ) -> CoreResult<StageContext> {
        // Build template variables
        let mut vars: HashMap<String, String> = config.variables.clone();

        // Merge exported variables (from prior stages' exports files)
        for (k, v) in extra_vars {
            vars.insert(k.clone(), v.clone());
        }

        // Per-stage template variables: {{stages.<id>.summary}} and {{stages.<id>.response}}
        for result in all_completed {
            vars.insert(
                format!("stages.{}.summary", result.stage_id),
                result.summary.clone().unwrap_or_default(),
            );
            vars.insert(
                format!("stages.{}.response", result.stage_id),
                result.response_text.clone(),
            );
        }

        // Build prior_summary from all parent results
        let prior_summary = if prior_results.is_empty() {
            vars.insert("prior_summary".to_string(), String::new());
            None
        } else {
            let mut summaries = Vec::new();
            for prior in prior_results {
                let summary = if let Some(ref s) = prior.summary {
                    s.clone()
                } else {
                    let mut text = prior.response_text.clone();
                    let artifact_desc = Self::build_artifact_description(&prior.artifacts);
                    if !artifact_desc.is_empty() {
                        text.push_str("\n\nFile changes from this stage:\n");
                        text.push_str(&artifact_desc);
                    }
                    self.summarize(&config.summarizer, &text).await?
                };
                if !summary.is_empty() {
                    summaries.push(summary);
                }
            }
            let combined = summaries.join("\n\n---\n\n");
            vars.insert("prior_summary".to_string(), combined.clone());
            if combined.is_empty() { None } else { Some(combined) }
        };

        // Collect changed files from all parent results (union)
        let mut changed_files: Vec<PathBuf> = Vec::new();
        let mut seen_paths = std::collections::HashSet::new();
        for prior in prior_results {
            for path in prior.artifacts.changed_paths() {
                if seen_paths.insert(path.clone()) {
                    changed_files.push(path.clone());
                }
            }
        }

        // Build artifact_list variable
        let artifact_list = changed_files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        vars.insert("artifact_list".to_string(), artifact_list);

        // Build all_artifacts variable (with content)
        let all_artifacts = self.build_all_artifacts_var(&changed_files, workspace)?;
        vars.insert("all_artifacts".to_string(), all_artifacts);

        // Verify feedback is NOT injected as a template variable to avoid
        // double injection — the agent runtime's build_prompt() prepends it
        // automatically. We still store it on StageContext for the runtime.
        let verify_feedback_owned = verify_feedback.map(|s| s.to_string());
        vars.insert("verify_feedback".to_string(), String::new());

        // Render the user prompt template
        let rendered_prompt = render_template(&stage_config.user_prompt, &vars)?;

        Ok(StageContext {
            rendered_prompt,
            prior_summary,
            changed_files,
            verify_feedback: verify_feedback_owned,
        })
    }

    /// Summarize text using the configured summarizer model.
    ///
    /// Dispatches to the appropriate CLI tool based on the runtime name:
    /// - `claude-code`: uses `claude --print --model <model>`
    /// - `copilot-cli`: uses `copilot -p /dev/stdin --model <model>`
    async fn summarize(&self, config: &SummarizerConfig, text: &str) -> CoreResult<String> {
        if text.trim().is_empty() || config.model.is_empty() {
            return Ok(text.to_string());
        }

        info!(
            model = %config.model,
            runtime = %self.runtime_name,
            "Summarizing prior stage output"
        );

        let summarize_prompt = format!(
            "Summarize the following AI agent output concisely. Focus on what was accomplished \
             and what files were created/modified. If the agent's text response is generic or \
             unhelpful, focus on the file changes listed at the end. Do NOT ask for more \
             information — just summarize whatever is available. \
             Keep it under {} tokens.\n\n---\n\n{}",
            config.max_tokens, text
        );

        let prompt_tempfile: Option<std::path::PathBuf>;
        let mut child = match self.runtime_name.as_str() {
            "copilot-cli" => {
                // Copilot CLI: -p takes prompt as a string arg, -s for clean output.
                // On Windows, resolve the npm .cmd shim to invoke node directly
                // to avoid argument quoting issues with cmd.exe.
                //
                // When the prompt is very long (>30 000 chars), passing it as a
                // command-line argument can exceed the Windows CreateProcess
                // limit (32 767 chars total). Write to a temp file and instruct
                // copilot to read it instead.
                let mut cmd = copilot_command();

                const ARG_LIMIT: usize = 30_000;
                if summarize_prompt.len() > ARG_LIMIT {
                    let pf = std::env::temp_dir()
                        .join(format!("fujin-summarize-{}.md", std::process::id()));
                    std::fs::write(&pf, &summarize_prompt)
                        .map_err(|e| CoreError::ContextError {
                            message: format!("Failed to write summarizer prompt file: {e}"),
                        })?;
                    cmd.arg("-p")
                        .arg(format!(
                            "Read the file {} and follow the instructions inside it exactly.",
                            pf.display()
                        ));
                    prompt_tempfile = Some(pf);
                } else {
                    cmd.arg("-p").arg(&summarize_prompt);
                    prompt_tempfile = None;
                }

                cmd
                    .arg("-s")
                    .arg("--no-ask-user")
                    .arg("--model").arg(&config.model)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| CoreError::ContextError {
                        message: format!("Failed to spawn copilot summarizer: {e}"),
                    })?
            }
            _ => {
                // Claude Code: --print reads prompt from stdin
                prompt_tempfile = None;
                Command::new("claude")
                    .arg("--print")
                    .arg("--model").arg(&config.model)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| CoreError::ContextError {
                        message: format!("Failed to spawn claude summarizer: {e}"),
                    })?
            }
        };

        // Claude Code reads from stdin; Copilot CLI takes prompt as -p arg
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(summarize_prompt.as_bytes()).await.map_err(|e| {
                CoreError::ContextError {
                    message: format!("Failed to write prompt to summarizer stdin: {e}"),
                }
            })?;
        }

        let output = child.wait_with_output().await.map_err(|e| CoreError::ContextError {
            message: format!("Failed to wait for summarizer: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(stderr = %stderr, "Summarizer failed, using truncated raw text");
            if let Some(ref path) = prompt_tempfile {
                let _ = std::fs::remove_file(path);
            }
            // Fallback: truncate the raw text
            let truncated = truncate_chars(text, 2000);
            return Ok(truncated);
        }

        let summary = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!(summary_len = summary.len(), "Generated summary");

        // Clean up temp prompt file if used
        if let Some(ref path) = prompt_tempfile {
            let _ = std::fs::remove_file(path);
        }

        Ok(summary)
    }

    /// Build a text description of artifacts for summarization fallback.
    ///
    /// Used when the agent's response text is too short for the summarizer
    /// to produce a meaningful summary (e.g. Copilot CLI silent mode).
    fn build_artifact_description(artifacts: &ArtifactSet) -> String {
        let mut parts = Vec::new();
        let summary = artifacts.summary();
        if !summary.is_empty() {
            parts.push(format!("Files changed: {summary}"));
        }
        for change in &artifacts.changes {
            parts.push(format!("  {} ({})", change.path.display(), change.kind));
        }
        parts.join("\n")
    }

    /// Build the {{all_artifacts}} variable: file paths with their content.
    fn build_all_artifacts_var(
        &self,
        changed_files: &[PathBuf],
        workspace: &Workspace,
    ) -> CoreResult<String> {
        let mut parts = Vec::new();

        for path in changed_files {
            match workspace.read_file(path) {
                Ok(content) => {
                    parts.push(format!(
                        "=== {} ===\n{}",
                        path.display(),
                        content
                    ));
                }
                Err(_) => {
                    parts.push(format!("=== {} === (deleted)", path.display()));
                }
            }
        }

        Ok(parts.join("\n\n"))
    }
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a Handlebars template with the given variables.
fn render_template(template: &str, vars: &HashMap<String, String>) -> CoreResult<String> {
    let mut hbs = Handlebars::new();
    // Don't escape HTML entities in prompts
    hbs.register_escape_fn(handlebars::no_escape);

    hbs.register_template_string("prompt", template)
        .map_err(|e| CoreError::TemplateError(format!("Invalid template: {e}")))?;

    hbs.render("prompt", vars)
        .map_err(|e| CoreError::TemplateError(format!("Template rendering failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_template_basic() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "world".to_string());
        let result = render_template("Hello, {{name}}!", &vars).unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_render_template_missing_var() {
        let vars = HashMap::new();
        let result = render_template("Hello, {{name}}!", &vars).unwrap();
        assert_eq!(result, "Hello, !");
    }

    #[test]
    fn test_render_template_multiple_vars() {
        let mut vars = HashMap::new();
        vars.insert("project".to_string(), "my-api".to_string());
        vars.insert("language".to_string(), "rust".to_string());
        let result =
            render_template("Build {{project}} in {{language}}", &vars).unwrap();
        assert_eq!(result, "Build my-api in rust");
    }

    #[test]
    fn test_render_template_html_not_escaped() {
        let mut vars = HashMap::new();
        vars.insert("code".to_string(), "<div>hello</div>".to_string());
        let result = render_template("Output: {{code}}", &vars).unwrap();
        assert_eq!(result, "Output: <div>hello</div>");
    }

    #[test]
    fn test_render_template_invalid_syntax() {
        let vars = HashMap::new();
        let result = render_template("Hello {{#if}}", &vars);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_artifact_description_empty() {
        let artifacts = ArtifactSet::new();
        let desc = ContextBuilder::build_artifact_description(&artifacts);
        // Empty artifact set with "no changes" summary should produce a description
        // containing that summary but no per-file lines
        assert!(desc.contains("no changes"));
    }

    #[test]
    fn test_build_artifact_description_with_changes() {
        use crate::artifact::{FileChange, FileChangeKind};

        let artifacts = ArtifactSet {
            changes: vec![
                FileChange {
                    path: PathBuf::from("src/main.rs"),
                    kind: FileChangeKind::Created,
                    hash: None,
                    size: Some(100),
                },
                FileChange {
                    path: PathBuf::from("old.txt"),
                    kind: FileChangeKind::Deleted,
                    hash: None,
                    size: None,
                },
            ],
        };

        let desc = ContextBuilder::build_artifact_description(&artifacts);
        assert!(desc.contains("1 created"));
        assert!(desc.contains("1 deleted"));
        assert!(desc.contains("src/main.rs"));
        assert!(desc.contains("old.txt"));
        assert!(desc.contains("(created)"));
        assert!(desc.contains("(deleted)"));
    }

    #[test]
    fn test_context_builder_default_runtime() {
        let cb = ContextBuilder::new();
        assert_eq!(cb.runtime_name, "claude-code");
    }

    #[test]
    fn test_context_builder_custom_runtime() {
        let cb = ContextBuilder::with_runtime("copilot-cli".to_string());
        assert_eq!(cb.runtime_name, "copilot-cli");
    }
}
