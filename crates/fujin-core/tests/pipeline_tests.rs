use async_trait::async_trait;
use fujin_config::PipelineConfig;
use fujin_core::*;
use fujin_core::agent::{AgentRuntime, AgentOutput};
use fujin_core::context::StageContext;
use fujin_core::event::PipelineEvent;
use fujin_core::stage::TokenUsage;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Helper: build a minimal PipelineConfig from YAML
// ---------------------------------------------------------------------------

fn single_stage_yaml() -> String {
    r#"
name: test-pipeline
stages:
  - id: stage-1
    name: First Stage
    model: mock-model
    system_prompt: "You are a test assistant."
    user_prompt: "Do something useful."
    max_turns: 5
    timeout_secs: 30
"#
    .to_string()
}

fn two_stage_yaml() -> String {
    r#"
name: test-pipeline
stages:
  - id: stage-1
    name: First Stage
    model: mock-model
    system_prompt: "You are a test assistant."
    user_prompt: "Do the first thing."
    max_turns: 5
    timeout_secs: 30
  - id: stage-2
    name: Second Stage
    model: mock-model
    system_prompt: "You are a test assistant."
    user_prompt: "Do the second thing."
    max_turns: 5
    timeout_secs: 30
"#
    .to_string()
}

fn parse_config(yaml: &str) -> PipelineConfig {
    PipelineConfig::from_yaml(yaml).expect("Failed to parse test pipeline config")
}

/// Collect all events from the receiver until it is closed.
async fn collect_events(mut rx: mpsc::UnboundedReceiver<PipelineEvent>) -> Vec<PipelineEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

// ---------------------------------------------------------------------------
// MockRuntime
// ---------------------------------------------------------------------------

/// A mock `AgentRuntime` for integration testing.
///
/// Configurable behaviour:
///   - `response_text`: text the mock returns from `execute`.
///   - `token_usage`: optional token usage to report.
///   - `files_to_write`: list of (relative_path, content) pairs that the mock
///     writes into the workspace during execution, simulating artifact creation.
///   - `fail_on_stage`: if `Some(id)`, the mock returns an error when
///     the stage's id matches.
///   - `sleep_ms`: optional delay (milliseconds) to simulate long-running work.
struct MockRuntime {
    response_text: String,
    token_usage: Option<TokenUsage>,
    files_to_write: Vec<(String, String)>,
    fail_on_stage: Option<String>,
    sleep_ms: Option<u64>,
}

impl MockRuntime {
    fn new(response_text: &str) -> Self {
        Self {
            response_text: response_text.to_string(),
            token_usage: None,
            files_to_write: Vec::new(),
            fail_on_stage: None,
            sleep_ms: None,
        }
    }

    fn with_token_usage(mut self, input: u64, output: u64) -> Self {
        self.token_usage = Some(TokenUsage {
            input_tokens: input,
            output_tokens: output,
        });
        self
    }

    fn with_files(mut self, files: Vec<(&str, &str)>) -> Self {
        self.files_to_write = files
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        self
    }

    fn with_fail_on_stage(mut self, stage_id: &str) -> Self {
        self.fail_on_stage = Some(stage_id.to_string());
        self
    }

    fn with_sleep_ms(mut self, ms: u64) -> Self {
        self.sleep_ms = Some(ms);
        self
    }
}

#[async_trait]
impl AgentRuntime for MockRuntime {
    fn name(&self) -> &str {
        "mock-runtime"
    }

    async fn execute(
        &self,
        config: &fujin_config::StageConfig,
        _context: &StageContext,
        workspace_root: &Path,
        _progress_tx: Option<mpsc::UnboundedSender<String>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        // Check if we should fail for this stage
        if let Some(ref fail_id) = self.fail_on_stage {
            if config.id == *fail_id {
                return Err(CoreError::AgentError {
                    message: format!("Mock failure on stage '{}'", config.id),
                });
            }
        }

        // Simulate long-running work (for timeout / cancellation tests)
        if let Some(ms) = self.sleep_ms {
            let step = std::time::Duration::from_millis(10);
            let total = std::time::Duration::from_millis(ms);
            let start = std::time::Instant::now();
            while start.elapsed() < total {
                // Cooperative cancellation check
                if let Some(ref flag) = cancel_flag {
                    if flag.load(Ordering::Relaxed) {
                        return Err(CoreError::Cancelled {
                            stage_id: config.id.clone(),
                        });
                    }
                }
                tokio::time::sleep(step).await;
            }
        }

        // Write files into workspace to simulate artifact creation
        for (rel_path, content) in &self.files_to_write {
            let full_path = workspace_root.join(rel_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&full_path, content)
                .map_err(|e| CoreError::AgentError {
                    message: format!("Mock failed to write {}: {e}", rel_path),
                })?;
        }

        Ok(AgentOutput {
            response_text: self.response_text.clone(),
            token_usage: self.token_usage.clone(),
        })
    }

    async fn health_check(&self) -> CoreResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_single_stage_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = single_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(
            MockRuntime::new("stage-1 done").with_token_usage(100, 50),
        ));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");

    // Should produce exactly one stage result
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].stage_id, "stage-1");
    assert_eq!(results[0].response_text, "stage-1 done");
    assert!(results[0].token_usage.is_some());
    let usage = results[0].token_usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);

    // Verify events
    drop(runner);
    let events = collect_events(rx).await;

    let has_started = events.iter().any(|e| matches!(e, PipelineEvent::PipelineStarted { .. }));
    let has_stage_started = events.iter().any(|e| matches!(e, PipelineEvent::StageStarted { stage_id, .. } if stage_id == "stage-1"));
    let has_stage_completed = events.iter().any(|e| matches!(e, PipelineEvent::StageCompleted { stage_id, .. } if stage_id == "stage-1"));
    let has_pipeline_completed = events.iter().any(|e| matches!(e, PipelineEvent::PipelineCompleted { stages_completed: 1, .. }));

    assert!(has_started, "Expected PipelineStarted event");
    assert!(has_stage_started, "Expected StageStarted event for stage-1");
    assert!(has_stage_completed, "Expected StageCompleted event for stage-1");
    assert!(has_pipeline_completed, "Expected PipelineCompleted event with 1 stage");
}

#[tokio::test]
async fn test_multi_stage_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = two_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("done")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].stage_id, "stage-1");
    assert_eq!(results[1].stage_id, "stage-2");

    drop(runner);
    let events = collect_events(rx).await;

    // Verify both stages started and completed
    let stage_started_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::StageStarted { .. }))
        .count();
    assert_eq!(stage_started_count, 2, "Expected 2 StageStarted events");

    let stage_completed_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::StageCompleted { .. }))
        .count();
    assert_eq!(stage_completed_count, 2, "Expected 2 StageCompleted events");

    // Verify context building happened for both stages
    let context_building_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::ContextBuilding { .. }))
        .count();
    assert_eq!(
        context_building_count, 2,
        "Expected ContextBuilding events for both stages"
    );

    // Verify PipelineCompleted reports 2 stages
    let has_pipeline_completed = events.iter().any(
        |e| matches!(e, PipelineEvent::PipelineCompleted { stages_completed: 2, .. }),
    );
    assert!(has_pipeline_completed, "Expected PipelineCompleted with 2 stages");
}

#[tokio::test]
async fn test_pipeline_creates_workspace() {
    let dir = tempfile::tempdir().unwrap();
    // Use a sub-directory that does not yet exist
    let workspace = dir.path().join("new_workspace");
    assert!(!workspace.exists(), "Workspace should not exist yet");

    let yaml = single_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, _rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, workspace.clone(), tx)
        .with_runtime(Box::new(MockRuntime::new("created workspace")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    runner.run(&options).await.expect("Pipeline should succeed");

    assert!(workspace.exists(), "Workspace directory should have been created");
    assert!(workspace.is_dir(), "Workspace should be a directory");
}

#[tokio::test]
async fn test_pipeline_checkpoint_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = two_stage_yaml();
    let config = parse_config(&yaml);

    // First run: stage-2 will fail
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let runner = PipelineRunner::new(
            config.clone(),
            yaml.clone(),
            dir.path().to_path_buf(),
            tx,
        )
        .with_runtime(Box::new(
            MockRuntime::new("ok").with_fail_on_stage("stage-2"),
        ));

        let options = RunOptions {
            resume: false,
            dry_run: false,
        };

        let err = runner.run(&options).await;
        assert!(err.is_err(), "Pipeline should fail because stage-2 fails");

        drop(runner);
        let events = collect_events(rx).await;

        // Verify checkpoint was saved
        let checkpoint_saved = events.iter().any(|e| {
            matches!(
                e,
                PipelineEvent::StageFailed {
                    stage_id,
                    checkpoint_saved: true,
                    ..
                } if stage_id == "stage-2"
            )
        });
        assert!(checkpoint_saved, "Checkpoint should be saved when stage-2 fails");
    }

    // Second run: resume from checkpoint; stage-1 should be skipped
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let runner = PipelineRunner::new(
            config.clone(),
            yaml.clone(),
            dir.path().to_path_buf(),
            tx,
        )
        .with_runtime(Box::new(MockRuntime::new("resumed ok")));

        let options = RunOptions {
            resume: true,
            dry_run: false,
        };

        let results = runner.run(&options).await.expect("Resume should succeed");

        // The resumed run should include the previously-completed stage-1 result
        // plus the now-successful stage-2 result. The checkpoint stores completed
        // stages, and the resumed run appends newly completed stages.
        // After resume: checkpoint had stage-1 already completed (next_stage_index=1).
        // The pipeline then executes stage-2 and returns all completed stages.
        assert_eq!(results.len(), 2, "Should have 2 total stage results after resume");
        assert_eq!(results[1].stage_id, "stage-2");

        drop(runner);
        let events = collect_events(rx).await;

        // Verify Resuming event was emitted
        let has_resuming = events.iter().any(|e| {
            matches!(
                e,
                PipelineEvent::Resuming {
                    start_index: 1,
                    total_stages: 2,
                    ..
                }
            )
        });
        assert!(has_resuming, "Expected Resuming event with start_index=1");

        // Verify stage-1 was NOT re-executed (no StageStarted for stage-1)
        let stage1_started = events.iter().any(|e| {
            matches!(
                e,
                PipelineEvent::StageStarted { stage_id, .. } if stage_id == "stage-1"
            )
        });
        assert!(
            !stage1_started,
            "Stage-1 should be skipped on resume"
        );

        // Verify stage-2 WAS executed
        let stage2_started = events.iter().any(|e| {
            matches!(
                e,
                PipelineEvent::StageStarted { stage_id, .. } if stage_id == "stage-2"
            )
        });
        assert!(stage2_started, "Stage-2 should execute on resume");
    }
}

#[tokio::test]
async fn test_pipeline_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = single_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("should not finish").with_sleep_ms(5000)))
        .with_cancel_flag(cancel_flag.clone());

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    // Set cancel flag after a short delay
    let flag = cancel_flag.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });

    let result = runner.run(&options).await;
    assert!(result.is_err(), "Pipeline should return an error on cancellation");

    let err = result.unwrap_err();
    assert!(
        matches!(err, CoreError::Cancelled { .. }),
        "Error should be Cancelled, got: {err:?}"
    );

    drop(runner);
    let events = collect_events(rx).await;

    let has_cancelled = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::PipelineCancelled { .. }));
    assert!(has_cancelled, "Expected PipelineCancelled event");
}

#[tokio::test]
async fn test_pipeline_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = two_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("should not execute")));

    let options = RunOptions {
        resume: false,
        dry_run: true,
    };

    let results = runner.run(&options).await.expect("Dry run should succeed");

    // Dry run should not execute any stages
    assert!(results.is_empty(), "Dry run should return no stage results");

    drop(runner);
    let events = collect_events(rx).await;

    // PipelineStarted should be emitted even for dry runs
    let has_started = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::PipelineStarted { .. }));
    assert!(has_started, "Expected PipelineStarted event in dry run");

    // No stages should have started
    let stage_started_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::StageStarted { .. }))
        .count();
    assert_eq!(stage_started_count, 0, "No stages should execute during dry run");

    // No PipelineCompleted should be emitted (dry_run returns early)
    let has_completed = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::PipelineCompleted { .. }));
    assert!(!has_completed, "PipelineCompleted should not be emitted in dry run");
}

#[tokio::test]
async fn test_pipeline_stage_timeout() {
    let dir = tempfile::tempdir().unwrap();

    // Create a config with a very short timeout
    let yaml = r#"
name: timeout-pipeline
stages:
  - id: slow-stage
    name: Slow Stage
    model: mock-model
    system_prompt: "You are a test assistant."
    user_prompt: "Be slow."
    max_turns: 5
    timeout_secs: 1
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Mock sleeps 10 seconds, but timeout is 1 second
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("too slow").with_sleep_ms(10_000)));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let result = runner.run(&options).await;
    assert!(result.is_err(), "Pipeline should fail due to timeout");

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("timed out"),
        "Error should mention timeout, got: {err_msg}"
    );

    drop(runner);
    let events = collect_events(rx).await;

    // Verify StageFailed event was emitted
    let has_stage_failed = events.iter().any(|e| {
        matches!(
            e,
            PipelineEvent::StageFailed { stage_id, .. } if stage_id == "slow-stage"
        )
    });
    assert!(has_stage_failed, "Expected StageFailed event for slow-stage");
}

#[tokio::test]
async fn test_workspace_artifact_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = single_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(
            MockRuntime::new("wrote files").with_files(vec![
                ("output.txt", "Hello from mock"),
                ("src/main.rs", "fn main() {}"),
            ]),
        ));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");

    assert_eq!(results.len(), 1);
    let artifacts = &results[0].artifacts;
    assert_eq!(
        artifacts.created_count(),
        2,
        "Should detect 2 created files"
    );

    // Verify the files actually exist on disk
    assert!(dir.path().join("output.txt").exists());
    assert!(dir.path().join("src/main.rs").exists());

    drop(runner);
    let events = collect_events(rx).await;

    // Find the StageCompleted event and check its artifacts
    let stage_completed = events.iter().find(|e| {
        matches!(
            e,
            PipelineEvent::StageCompleted { stage_id, .. } if stage_id == "stage-1"
        )
    });
    assert!(stage_completed.is_some(), "Expected StageCompleted event");

    if let Some(PipelineEvent::StageCompleted { artifacts, .. }) = stage_completed {
        assert_eq!(
            artifacts.created_count(),
            2,
            "StageCompleted event should report 2 created artifacts"
        );
        let paths: Vec<String> = artifacts
            .changed_paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert!(
            paths.iter().any(|p| p.contains("output.txt")),
            "Artifacts should contain output.txt, got: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("main.rs")),
            "Artifacts should contain main.rs, got: {paths:?}"
        );
    }
}
