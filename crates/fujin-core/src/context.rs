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
    runtime_name: String,
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
    pub async fn build(
        &self,
        config: &PipelineConfig,
        stage_config: &StageConfig,
        prior_result: Option<&StageResult>,
        workspace: &Workspace,
        verify_feedback: Option<&str>,
    ) -> CoreResult<StageContext> {
        // Build template variables
        let mut vars: HashMap<String, String> = config.variables.clone();

        // Add prior_summary
        let prior_summary = if let Some(prior) = prior_result {
            let summary = if let Some(ref s) = prior.summary {
                s.clone()
            } else {
                // Summarize the prior stage output
                self.summarize(&config.summarizer, &prior.response_text)
                    .await?
            };
            vars.insert("prior_summary".to_string(), summary.clone());
            Some(summary)
        } else {
            vars.insert("prior_summary".to_string(), String::new());
            None
        };

        // Collect changed files from prior result
        let changed_files: Vec<PathBuf> = prior_result
            .map(|r| r.artifacts.changed_paths().into_iter().cloned().collect())
            .unwrap_or_default();

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

        // Add verify feedback if present
        let verify_feedback_owned = verify_feedback.map(|s| s.to_string());
        vars.insert(
            "verify_feedback".to_string(),
            verify_feedback_owned.clone().unwrap_or_default(),
        );

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
    /// - `copilot-cli`: uses `copilot -p - -s --model <model>`
    async fn summarize(&self, config: &SummarizerConfig, text: &str) -> CoreResult<String> {
        if text.trim().is_empty() {
            return Ok(String::new());
        }

        info!(
            model = %config.model,
            runtime = %self.runtime_name,
            "Summarizing prior stage output"
        );

        let summarize_prompt = format!(
            "Summarize the following AI agent output concisely. Focus on what was accomplished, \
             what files were created/modified, and any important decisions or results. \
             Keep it under {} tokens.\n\n---\n\n{}",
            config.max_tokens, text
        );

        let mut child = match self.runtime_name.as_str() {
            "copilot-cli" => {
                Command::new("copilot")
                    .arg("-p").arg("-")
                    .arg("-s")
                    .arg("--model").arg(&config.model)
                    .arg("--allow-all-tools")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| CoreError::ContextError {
                        message: format!("Failed to spawn copilot summarizer: {e}"),
                    })?
            }
            _ => {
                // Default: claude-code
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

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(summarize_prompt.as_bytes()).await.map_err(|e| {
                CoreError::ContextError {
                    message: format!("Failed to write prompt to summarizer stdin: {e}"),
                }
            })?;
            // Drop stdin to close it, signaling EOF
        }

        let output = child.wait_with_output().await.map_err(|e| CoreError::ContextError {
            message: format!("Failed to wait for summarizer: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(stderr = %stderr, "Summarizer failed, using truncated raw text");
            // Fallback: truncate the raw text
            let truncated = truncate_chars(text, 2000);
            return Ok(truncated);
        }

        let summary = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!(summary_len = summary.len(), "Generated summary");
        Ok(summary)
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
}
