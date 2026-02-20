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
  - id: stage-2
    name: Second Stage
    model: mock-model
    system_prompt: "You are a test assistant."
    user_prompt: "Do the second thing."
    max_turns: 5
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

    // Initialize a git repo so git-based diff can detect changes
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .expect("git init failed");
    // Create an initial commit so new files show as untracked/created
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .expect("git commit failed");

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

// ---------------------------------------------------------------------------
// Retry group verification tests
// ---------------------------------------------------------------------------

fn retry_verify_pass_yaml() -> String {
    r#"
name: verify-pass-pipeline
retry_groups:
  fix:
    max_retries: 3
    verify:
      system_prompt: "You are a verifier."
      user_prompt: "Check the code."
stages:
  - id: code
    name: Write Code
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Write code."
    retry_group: fix
  - id: test
    name: Run Tests
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Test code."
    retry_group: fix
"#
    .to_string()
}

fn retry_verify_fail_yaml() -> String {
    r#"
name: verify-fail-pipeline
retry_groups:
  fix:
    max_retries: 2
    verify:
      system_prompt: "You are a verifier."
      user_prompt: "Check the code."
stages:
  - id: code
    name: Write Code
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Write code."
    retry_group: fix
  - id: test
    name: Run Tests
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Test code."
    retry_group: fix
"#
    .to_string()
}

#[tokio::test]
async fn test_retry_group_verify_pass() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = retry_verify_pass_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Mock returns "PASS" — the verify agent will parse this as a passing verdict
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("Everything looks good. PASS")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed when verify passes");
    assert_eq!(results.len(), 2);

    drop(runner);
    let events = collect_events(rx).await;

    let has_verify_running = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::VerifyRunning { .. }));
    assert!(has_verify_running, "Expected VerifyRunning event");

    let has_verify_passed = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::VerifyPassed { .. }));
    assert!(has_verify_passed, "Expected VerifyPassed event");

    let has_verify_failed = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::VerifyFailed { .. }));
    assert!(!has_verify_failed, "Should not have VerifyFailed event");
}

#[tokio::test]
async fn test_retry_group_verify_fail_loops() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = retry_verify_fail_yaml();
    let config = parse_config(&yaml);

    let (tx, mut rx) = mpsc::unbounded_channel();

    // Mock returns "FAIL" — the verify agent will parse this as a failing verdict
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("The code is wrong. FAIL")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    // Drain events in a background task, declining retry when limit is reached
    let event_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            if let PipelineEvent::RetryLimitReached { response_tx, .. } = event {
                let _ = response_tx.send(false);
                continue;
            }
            events.push(event);
        }
        events
    });

    let result = runner.run(&options).await;
    assert!(result.is_err(), "Pipeline should fail when verify always fails");

    drop(runner);
    let events = event_handle.await.unwrap();

    // With max_retries: 2 and the off-by-one fix (count > max_retries):
    // Attempt 1: verify FAIL → count=1, 1 not > 2, retry
    // Attempt 2: verify FAIL → count=2, 2 not > 2, retry
    // Attempt 3: verify FAIL → count=3, 3 > 2, prompt user → user declines
    let verify_failed_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::VerifyFailed { .. }))
        .count();
    assert_eq!(verify_failed_count, 3, "Expected 3 VerifyFailed events (max_retries=2 means 3 total attempts), got {verify_failed_count}");

    let retry_attempt_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::RetryGroupAttempt { .. }))
        .count();
    assert_eq!(retry_attempt_count, 2, "Expected 2 RetryGroupAttempt events (first 2 failures trigger retry), got {retry_attempt_count}");
}

// ---------------------------------------------------------------------------
// Verify feedback injection test
// ---------------------------------------------------------------------------

use std::sync::Mutex;

/// Captured context from a single agent invocation.
#[derive(Debug, Clone)]
struct CapturedCall {
    rendered_prompt: String,
    verify_feedback: Option<String>,
}

/// A stateful mock runtime that tracks call count and captures contexts.
/// - Returns "FAIL" on the first verify call, "PASS" on the second.
/// - Captures the rendered prompt and verify_feedback from each call.
struct FeedbackCaptureMock {
    call_count: Mutex<u32>,
    captured_calls: Arc<Mutex<Vec<CapturedCall>>>,
}

impl FeedbackCaptureMock {
    fn new(captured_calls: Arc<Mutex<Vec<CapturedCall>>>) -> Self {
        Self {
            call_count: Mutex::new(0),
            captured_calls,
        }
    }
}

#[async_trait]
impl AgentRuntime for FeedbackCaptureMock {
    fn name(&self) -> &str {
        "feedback-capture-mock"
    }

    async fn execute(
        &self,
        config: &fujin_config::StageConfig,
        context: &StageContext,
        workspace_root: &Path,
        _progress_tx: Option<mpsc::UnboundedSender<String>>,
        _cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;
        let call_num = *count;
        drop(count);

        // Capture the rendered prompt and verify feedback
        self.captured_calls.lock().unwrap().push(CapturedCall {
            rendered_prompt: context.rendered_prompt.clone(),
            verify_feedback: context.verify_feedback.clone(),
        });

        let _ = workspace_root;

        // Verify stages have id starting with "__verify_"
        let is_verify = config.id.starts_with("__verify_");

        let response = if is_verify {
            // First verify call: FAIL with specific feedback
            // Second verify call: PASS
            if call_num <= 4 {
                // calls 1-2 are work stages, call 3 is first verify
                "The function is missing null checks. FAIL".to_string()
            } else {
                "All issues resolved. PASS".to_string()
            }
        } else {
            "Stage work done.".to_string()
        };

        Ok(AgentOutput {
            response_text: response,
            token_usage: None,
        })
    }

    async fn health_check(&self) -> CoreResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_verify_feedback_injected_on_retry() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: feedback-test-pipeline
retry_groups:
  fix:
    max_retries: 5
    verify:
      system_prompt: "You are a verifier."
      user_prompt: "Check the code."
stages:
  - id: code
    name: Write Code
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Write code. {{verify_feedback}}"
    retry_group: fix
  - id: test
    name: Run Tests
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Test code."
    retry_group: fix
"#
    .to_string();
    let config = parse_config(&yaml);

    let captured_calls = Arc::new(Mutex::new(Vec::new()));
    let (tx, _rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(FeedbackCaptureMock::new(captured_calls.clone())));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    // The mock will FAIL the first verify, then PASS the second.
    // The pipeline should loop once and succeed.
    let results = runner.run(&options).await.expect("Pipeline should eventually succeed");
    assert_eq!(results.len(), 2, "Should have 2 completed stages");

    let calls = captured_calls.lock().unwrap();

    // Calls: code(1), test(2), verify->FAIL(3), code(4), test(5), verify->PASS(6)
    // calls[0] = first code run — no feedback
    assert!(
        calls[0].verify_feedback.is_none(),
        "First code run should NOT have verify feedback, got: {:?}",
        calls[0].verify_feedback
    );

    // calls[3] = second code run (after FAIL) — should have feedback via StageContext.verify_feedback
    let feedback = calls[3].verify_feedback.as_deref().unwrap_or("");
    assert!(
        feedback.contains("missing null checks"),
        "Second code run should contain verify feedback in context, got: {}",
        feedback
    );

    // Verify that {{verify_feedback}} in the template is NOT populated (no double injection)
    assert!(
        !calls[3].rendered_prompt.contains("missing null checks"),
        "Template variable {{{{verify_feedback}}}} should be empty (feedback delivered via build_prompt), got: {}",
        calls[3].rendered_prompt
    );
}

// ---------------------------------------------------------------------------
// Stage failure feedback injection test
// ---------------------------------------------------------------------------

/// A mock runtime that fails the first time a specific stage runs, then succeeds.
/// Captures verify_feedback on each call to check that failure feedback is injected.
struct FailOnceMock {
    fail_stage_id: String,
    fail_count: Mutex<u32>,
    max_failures: u32,
    captured_calls: Arc<Mutex<Vec<CapturedCall>>>,
}

impl FailOnceMock {
    fn new(fail_stage_id: &str, max_failures: u32, captured_calls: Arc<Mutex<Vec<CapturedCall>>>) -> Self {
        Self {
            fail_stage_id: fail_stage_id.to_string(),
            fail_count: Mutex::new(0),
            max_failures,
            captured_calls,
        }
    }
}

#[async_trait]
impl AgentRuntime for FailOnceMock {
    fn name(&self) -> &str {
        "fail-once-mock"
    }

    async fn execute(
        &self,
        config: &fujin_config::StageConfig,
        context: &StageContext,
        _workspace_root: &Path,
        _progress_tx: Option<mpsc::UnboundedSender<String>>,
        _cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        self.captured_calls.lock().unwrap().push(CapturedCall {
            rendered_prompt: context.rendered_prompt.clone(),
            verify_feedback: context.verify_feedback.clone(),
        });

        if config.id == self.fail_stage_id {
            let mut count = self.fail_count.lock().unwrap();
            if *count < self.max_failures {
                *count += 1;
                return Err(CoreError::AgentError {
                    message: "cargo test failed: test_foo FAILED assertion error".to_string(),
                });
            }
        }

        Ok(AgentOutput {
            response_text: "Done.".to_string(),
            token_usage: None,
        })
    }

    async fn health_check(&self) -> CoreResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_stage_failure_feedback_injected_on_retry() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Two stages in a retry group, no verify agent. Stage "test" will fail once.
    let yaml = r#"
name: stage-failure-feedback
retry_groups:
  fix:
    max_retries: 3
stages:
  - id: code
    name: Write Code
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Write code."
    retry_group: fix
  - id: test
    name: Run Tests
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Run tests."
    retry_group: fix
"#
    .to_string();
    let config = parse_config(&yaml);

    let captured_calls = Arc::new(Mutex::new(Vec::new()));
    let (tx, _rx) = mpsc::unbounded_channel();

    // "test" stage fails once, then succeeds
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(FailOnceMock::new("test", 1, captured_calls.clone())));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed after retry");
    assert_eq!(results.len(), 2, "Should have 2 completed stages");

    let calls = captured_calls.lock().unwrap();

    // Calls: code(0), test(1)->FAIL, code(2), test(3)->OK
    // calls[0] = first code run — no feedback
    assert!(
        calls[0].verify_feedback.is_none(),
        "First code run should not have feedback"
    );

    // calls[2] = second code run (after test failure) — should have the failure error
    let feedback = calls[2].verify_feedback.as_deref().unwrap_or("");
    assert!(
        feedback.contains("cargo test failed"),
        "Retry code stage should receive stage failure feedback, got: {}",
        feedback
    );
}

// ---------------------------------------------------------------------------
// Retry count correctness test (off-by-one fix)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_retry_count_matches_max_retries() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // max_retries: 1 means the pipeline should get 1 retry (2 total attempts)
    let yaml = r#"
name: retry-count-test
retry_groups:
  fix:
    max_retries: 1
    verify:
      system_prompt: "You are a verifier."
      user_prompt: "Check."
stages:
  - id: code
    name: Write Code
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Write code."
    retry_group: fix
  - id: test
    name: Run Tests
    model: mock-model
    system_prompt: "You are helpful."
    user_prompt: "Test code."
    retry_group: fix
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, mut rx) = mpsc::unbounded_channel();

    // Always returns FAIL for verify
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("Code is wrong. FAIL")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let event_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            if let PipelineEvent::RetryLimitReached { response_tx, .. } = event {
                let _ = response_tx.send(false);
                continue;
            }
            events.push(event);
        }
        events
    });

    let result = runner.run(&options).await;
    assert!(result.is_err(), "Pipeline should fail");

    drop(runner);
    let events = event_handle.await.unwrap();

    // With max_retries=1:
    // Attempt 1: verify FAIL → count=1, 1 not > 1, retry (RetryGroupAttempt emitted)
    // Attempt 2: verify FAIL → count=2, 2 > 1, prompt user → decline
    // So: 2 VerifyFailed events, 1 RetryGroupAttempt event
    let verify_failed_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::VerifyFailed { .. }))
        .count();
    assert_eq!(
        verify_failed_count, 2,
        "max_retries=1 should allow 2 total attempts (1 original + 1 retry), got {verify_failed_count}"
    );

    let retry_attempt_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::RetryGroupAttempt { .. }))
        .count();
    assert_eq!(
        retry_attempt_count, 1,
        "max_retries=1 should produce exactly 1 RetryGroupAttempt, got {retry_attempt_count}"
    );
}

// ---------------------------------------------------------------------------
// Command stage tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_command_stage_runs_commands() {
    let dir = tempfile::tempdir().unwrap();
    // Initialize git so workspace tracking works
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: command-stage-pipeline
stages:
  - id: setup
    name: Setup
    commands:
      - "echo hello > greeting.txt"
      - "echo world >> greeting.txt"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Command stages don't use the agent runtime, but we still need one
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("should not be called")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Command stage pipeline should succeed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].stage_id, "setup");

    // The response_text should contain the combined command output
    assert!(results[0].response_text.contains("echo hello"));

    // Verify the file was actually created
    let content = std::fs::read_to_string(dir.path().join("greeting.txt")).unwrap();
    assert!(content.contains("hello"));

    drop(runner);
    let events = collect_events(rx).await;

    // Should have CommandRunning events
    let command_running_count = events
        .iter()
        .filter(|e| matches!(e, PipelineEvent::CommandRunning { .. }))
        .count();
    assert_eq!(command_running_count, 2, "Expected 2 CommandRunning events for 2 commands");

    // StageStarted should report model as "commands" for command stages
    let stage_started = events.iter().find(|e| matches!(e, PipelineEvent::StageStarted { .. }));
    if let Some(PipelineEvent::StageStarted { model, .. }) = stage_started {
        assert_eq!(model, "commands", "Command stages should report model as 'commands'");
    } else {
        panic!("Expected StageStarted event");
    }
}

#[tokio::test]
async fn test_command_stage_template_variables() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: template-cmd-pipeline
variables:
  greeting: "hello from template"
stages:
  - id: cmd
    name: Command with Variables
    commands:
      - "echo '{{greeting}}' > output.txt"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, _rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("unused")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    assert_eq!(results.len(), 1);

    // Verify template variable was rendered in the command
    let content = std::fs::read_to_string(dir.path().join("output.txt")).unwrap();
    assert!(
        content.contains("hello from template"),
        "Template variable should be rendered in command, got: {}",
        content
    );
}

#[tokio::test]
async fn test_command_stage_failure_stops_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: failing-cmd-pipeline
stages:
  - id: fail-cmd
    name: Failing Command
    commands:
      - "exit 1"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, mut rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("unused")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    // Drain events in a background task to prevent any potential blocking
    let event_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = runner.run(&options).await;
    assert!(result.is_err(), "Pipeline should fail when command exits non-zero");

    drop(runner);
    let events = event_handle.await.unwrap();

    let has_stage_failed = events.iter().any(|e| {
        matches!(
            e,
            PipelineEvent::StageFailed { stage_id, .. } if stage_id == "fail-cmd"
        )
    });
    assert!(has_stage_failed, "Expected StageFailed event for fail-cmd");
}

// ---------------------------------------------------------------------------
// Per-stage runtime event reporting test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stage_started_reports_runtime_name() {
    let dir = tempfile::tempdir().unwrap();

    let yaml = single_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("done")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    runner.run(&options).await.expect("Pipeline should succeed");

    drop(runner);
    let events = collect_events(rx).await;

    // Verify StageStarted reports the runtime name
    let stage_started = events.iter().find_map(|e| {
        if let PipelineEvent::StageStarted { stage_id, runtime, .. } = e {
            Some((stage_id.clone(), runtime.clone()))
        } else {
            None
        }
    });
    assert!(stage_started.is_some(), "Expected StageStarted event");
    let (_, runtime) = stage_started.unwrap();
    assert_eq!(runtime, "mock-runtime", "Runtime name should come from the AgentRuntime::name()");
}

#[tokio::test]
async fn test_stage_started_reports_allowed_tools() {
    let dir = tempfile::tempdir().unwrap();

    // Stage with custom allowed tools
    let yaml = r#"
name: tools-pipeline
stages:
  - id: stage-1
    name: First Stage
    model: mock-model
    system_prompt: "You are a test assistant."
    user_prompt: "Do something."
    allowed_tools:
      - read
      - write
      - bash
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("done")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    runner.run(&options).await.expect("Pipeline should succeed");

    drop(runner);
    let events = collect_events(rx).await;

    let tools = events.iter().find_map(|e| {
        if let PipelineEvent::StageStarted { allowed_tools, .. } = e {
            Some(allowed_tools.clone())
        } else {
            None
        }
    });
    assert!(tools.is_some(), "Expected StageStarted event");
    let tools = tools.unwrap();
    assert_eq!(tools, vec!["read", "write", "bash"]);
}

// ---------------------------------------------------------------------------
// Token usage by model aggregation test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pipeline_completed_aggregates_tokens_by_model() {
    let dir = tempfile::tempdir().unwrap();

    let yaml = single_stage_yaml();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(
            MockRuntime::new("done").with_token_usage(500, 200),
        ));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    runner.run(&options).await.expect("Pipeline should succeed");

    drop(runner);
    let events = collect_events(rx).await;

    let completed = events.iter().find(|e| matches!(e, PipelineEvent::PipelineCompleted { .. }));
    assert!(completed.is_some(), "Expected PipelineCompleted event");

    if let Some(PipelineEvent::PipelineCompleted { token_usage_by_model, .. }) = completed {
        assert!(!token_usage_by_model.is_empty(), "Should have token usage by model");
        let total_input: u64 = token_usage_by_model.iter().map(|(_, u)| u.input_tokens).sum();
        let total_output: u64 = token_usage_by_model.iter().map(|(_, u)| u.output_tokens).sum();
        assert_eq!(total_input, 500);
        assert_eq!(total_output, 200);
    }
}

// ---------------------------------------------------------------------------
// Branching: when condition tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_when_condition_true_runs_stage() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: when-true-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze the task"
  - id: frontend
    name: Frontend
    model: mock-model
    when:
      stage: analyze
      output_matches: "FRONTEND"
    system_prompt: "You are a frontend dev."
    user_prompt: "Build frontend"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // MockRuntime response contains "FRONTEND" — when condition should be true
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("Analysis complete: FRONTEND work needed")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    assert_eq!(results.len(), 2, "Both stages should run when condition is met");

    drop(runner);
    let events = collect_events(rx).await;

    // No stages should be skipped
    let skipped = events.iter().filter(|e| matches!(e, PipelineEvent::StageSkipped { .. })).count();
    assert_eq!(skipped, 0, "No stages should be skipped");
}

#[tokio::test]
async fn test_when_condition_false_skips_stage() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: when-false-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze the task"
  - id: frontend
    name: Frontend
    model: mock-model
    when:
      stage: analyze
      output_matches: "FRONTEND"
    system_prompt: "You are a frontend dev."
    user_prompt: "Build frontend"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // MockRuntime response does NOT contain "FRONTEND" — when condition should be false
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("Analysis complete: BACKEND work needed")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    assert_eq!(results.len(), 1, "Only analyze stage should complete; frontend skipped");

    drop(runner);
    let events = collect_events(rx).await;

    // Frontend stage should be skipped
    let skipped: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PipelineEvent::StageSkipped { stage_id, .. } = e {
                Some(stage_id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(skipped, vec!["frontend"], "Frontend stage should be skipped");
}

// ---------------------------------------------------------------------------
// Branching: on_branch skip tests
// ---------------------------------------------------------------------------

/// A mock runtime that returns different text based on stage ID.
/// For branch classifier stages (id starts with "__branch_classifier_"),
/// it returns the route name from the configured mapping.
struct BranchMockRuntime {
    stage_responses: std::collections::HashMap<String, String>,
    default_response: String,
}

impl BranchMockRuntime {
    fn new(responses: Vec<(&str, &str)>, default: &str) -> Self {
        Self {
            stage_responses: responses
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            default_response: default.to_string(),
        }
    }
}

#[async_trait]
impl AgentRuntime for BranchMockRuntime {
    fn name(&self) -> &str {
        "branch-mock"
    }

    async fn execute(
        &self,
        config: &fujin_config::StageConfig,
        _context: &StageContext,
        _workspace_root: &Path,
        _progress_tx: Option<mpsc::UnboundedSender<String>>,
        _cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        let response = self
            .stage_responses
            .get(&config.id)
            .cloned()
            .unwrap_or_else(|| self.default_response.clone());

        Ok(AgentOutput {
            response_text: response,
            token_usage: None,
        })
    }

    async fn health_check(&self) -> CoreResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_on_branch_skips_unmatched_stages() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: branch-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "Analyze"
    user_prompt: "Analyze"
    branch:
      prompt: "Classify"
      routes: [frontend, backend]
      default: frontend
  - id: frontend-impl
    name: Frontend
    model: mock-model
    on_branch: frontend
    system_prompt: "Frontend"
    user_prompt: "Build frontend"
  - id: backend-impl
    name: Backend
    model: mock-model
    on_branch: backend
    system_prompt: "Backend"
    user_prompt: "Build backend"
  - id: review
    name: Review
    model: mock-model
    system_prompt: "Review"
    user_prompt: "Review"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Branch classifier (stage id __branch_classifier_0) returns "frontend"
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(BranchMockRuntime::new(
            vec![("__branch_classifier_0", "frontend")],
            "stage work done",
        )));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    // analyze + frontend-impl + review = 3 completed (backend-impl skipped)
    assert_eq!(results.len(), 3, "Should have 3 completed stages (backend skipped)");

    let completed_ids: Vec<&str> = results.iter().map(|r| r.stage_id.as_str()).collect();
    assert_eq!(completed_ids, vec!["analyze", "frontend-impl", "review"]);

    drop(runner);
    let events = collect_events(rx).await;

    // Backend should be skipped
    let skipped: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PipelineEvent::StageSkipped { stage_id, .. } = e {
                Some(stage_id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(skipped, vec!["backend-impl"], "Backend stage should be skipped");

    // Branch selected event
    let branch_selected: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PipelineEvent::BranchSelected { selected_route, .. } = e {
                Some(selected_route.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(branch_selected, vec!["frontend"], "Frontend route should be selected");
}

#[tokio::test]
async fn test_convergence_stage_runs_after_branch() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: convergence-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "Analyze"
    user_prompt: "Analyze"
    branch:
      prompt: "Classify"
      routes: [a, b]
      default: a
  - id: route-a
    name: Route A
    model: mock-model
    on_branch: a
    system_prompt: "A"
    user_prompt: "Do A"
  - id: route-b
    name: Route B
    model: mock-model
    on_branch: b
    system_prompt: "B"
    user_prompt: "Do B"
  - id: final
    name: Final
    model: mock-model
    system_prompt: "Final"
    user_prompt: "Wrap up"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Classifier selects route "b"
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(BranchMockRuntime::new(
            vec![("__branch_classifier_0", "b")],
            "done",
        )));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    // analyze + route-b + final = 3 (route-a skipped)
    assert_eq!(results.len(), 3);

    let completed_ids: Vec<&str> = results.iter().map(|r| r.stage_id.as_str()).collect();
    assert_eq!(completed_ids, vec!["analyze", "route-b", "final"]);

    drop(runner);
    let events = collect_events(rx).await;

    let skipped: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PipelineEvent::StageSkipped { stage_id, .. } = e {
                Some(stage_id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(skipped, vec!["route-a"]);
}

#[tokio::test]
async fn test_verify_runs_when_last_group_stage_skipped_by_branch() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // The branch routes to "alpha", so beta-impl (the last stage in the
    // retry group by index) is skipped.  Verify should still fire after
    // alpha-impl completes because the group did real work.
    let yaml = r#"
name: branch-verify-test
retry_groups:
  grp:
    max_retries: 2
    verify:
      model: mock-model
      system_prompt: "Verify"
      user_prompt: "Check it"
      allowed_tools: ["bash"]
stages:
  - id: classify
    name: Classify
    model: mock-model
    system_prompt: "Classify"
    user_prompt: "Classify"
    branch:
      prompt: "Pick alpha or beta"
      routes: [alpha, beta]
      default: alpha
  - id: alpha-impl
    name: Alpha
    model: mock-model
    on_branch: alpha
    retry_group: grp
    system_prompt: "Alpha"
    user_prompt: "Do alpha"
  - id: beta-impl
    name: Beta
    model: mock-model
    on_branch: beta
    retry_group: grp
    system_prompt: "Beta"
    user_prompt: "Do beta"
  - id: done
    name: Done
    model: mock-model
    system_prompt: "Done"
    user_prompt: "Wrap up"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Branch classifier returns "alpha"; verify agent returns "PASS"
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(BranchMockRuntime::new(
            vec![("__branch_classifier_0", "alpha")],
            "All good. PASS",
        )));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    // classify + alpha-impl + done = 3 (beta-impl skipped)
    assert_eq!(results.len(), 3);

    let completed_ids: Vec<&str> = results.iter().map(|r| r.stage_id.as_str()).collect();
    assert_eq!(completed_ids, vec!["classify", "alpha-impl", "done"]);

    drop(runner);
    let events = collect_events(rx).await;

    // beta-impl should be skipped
    let skipped: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PipelineEvent::StageSkipped { stage_id, .. } = e {
                Some(stage_id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(skipped, vec!["beta-impl"]);

    // Verify should have run and passed even though the last group stage was skipped
    let has_verify_running = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::VerifyRunning { .. }));
    assert!(has_verify_running, "Verify should run when last group stage is skipped by branch");

    let has_verify_passed = events
        .iter()
        .any(|e| matches!(e, PipelineEvent::VerifyPassed { .. }));
    assert!(has_verify_passed, "Verify should pass");
}

// ---------------------------------------------------------------------------
// Exports: agent-set variables (stored in platform data dir, not workspace)
// ---------------------------------------------------------------------------

/// A mock runtime that simulates an agent writing an exports JSON file.
///
/// When executing a stage whose ID is in `stage_exports`, the mock finds the
/// pre-created exports directory on the filesystem (created by the pipeline
/// runner) and writes the JSON content to `<stage_id>.json` inside it.
struct ExportWriterMock {
    stage_exports: std::collections::HashMap<String, String>,
    default_response: String,
}

impl ExportWriterMock {
    fn new(exports: Vec<(&str, &str)>, default_response: &str) -> Self {
        Self {
            stage_exports: exports
                .into_iter()
                .map(|(id, json)| (id.to_string(), json.to_string()))
                .collect(),
            default_response: default_response.to_string(),
        }
    }
}

#[async_trait]
impl AgentRuntime for ExportWriterMock {
    fn name(&self) -> &str {
        "export-writer-mock"
    }

    async fn execute(
        &self,
        config: &fujin_config::StageConfig,
        _context: &StageContext,
        workspace_root: &Path,
        _progress_tx: Option<mpsc::UnboundedSender<String>>,
        _cancel_flag: Option<Arc<AtomicBool>>,
    ) -> CoreResult<AgentOutput> {
        // If this stage should write exports, find the pre-created directory
        // and write the JSON file there. The pipeline runner creates
        // <exports_dir>/<run_id>/ before executing the stage.
        if let Some(json_content) = self.stage_exports.get(&config.id) {
            let ws_exports_dir = fujin_core::paths::exports_dir(workspace_root);
            if ws_exports_dir.exists() {
                // Find the run_id subdirectory (the pipeline pre-creates it)
                if let Ok(entries) = std::fs::read_dir(&ws_exports_dir) {
                    for entry in entries.flatten() {
                        if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                            let target = entry.path().join(format!("{}.json", config.id));
                            std::fs::write(&target, json_content).map_err(|e| {
                                CoreError::AgentError {
                                    message: format!(
                                        "Mock failed to write exports to {}: {e}",
                                        target.display()
                                    ),
                                }
                            })?;
                        }
                    }
                }
            }
        }

        Ok(AgentOutput {
            response_text: self.default_response.clone(),
            token_usage: None,
        })
    }

    async fn health_check(&self) -> CoreResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_exports_loads_variables_for_downstream_stages() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: exports-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze the project. Write findings to {{exports_file}}"
    exports:
      keys: [language, framework]
  - id: implement
    name: Implement
    model: mock-model
    system_prompt: "You are a developer."
    user_prompt: "Build in {{language}} using {{framework}}."
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Mock writes exports JSON when executing the "analyze" stage
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(ExportWriterMock::new(
            vec![("analyze", r#"{"language": "rust", "framework": "actix"}"#)],
            "done",
        )));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    assert_eq!(results.len(), 2);

    drop(runner);
    let events = collect_events(rx).await;

    // Should have an ExportsLoaded event
    let exports_loaded = events.iter().find_map(|e| {
        if let PipelineEvent::ExportsLoaded { stage_id, variables, .. } = e {
            Some((stage_id.clone(), variables.clone()))
        } else {
            None
        }
    });
    assert!(exports_loaded.is_some(), "Expected ExportsLoaded event");
    let (stage_id, variables) = exports_loaded.unwrap();
    assert_eq!(stage_id, "analyze");
    assert!(variables.contains(&("language".to_string(), "rust".to_string())));
    assert!(variables.contains(&("framework".to_string(), "actix".to_string())));

    // The rendered prompt for the implement stage should contain the exported values
    let stage_context = events.iter().find_map(|e| {
        if let PipelineEvent::StageContext { stage_index, rendered_prompt, .. } = e {
            if *stage_index == 1 {
                Some(rendered_prompt.clone())
            } else {
                None
            }
        } else {
            None
        }
    });
    assert!(stage_context.is_some(), "Expected StageContext for stage 1");
    let prompt = stage_context.unwrap();
    assert!(
        prompt.contains("rust") && prompt.contains("actix"),
        "Exported variables should be rendered in downstream prompt, got: {}",
        prompt
    );

    // Verify exports file is NOT in the workspace directory
    assert!(
        !dir.path().join(".fujin/exports").exists(),
        "Exports should NOT be stored in the workspace"
    );
}

#[tokio::test]
async fn test_exports_missing_file_emits_warning() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Mock does NOT write any exports file — agent "forgot" to write it

    let yaml = r#"
name: exports-missing-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze the project."
    exports: {}
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(MockRuntime::new("done")));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    // Pipeline should still succeed — exports warnings are non-fatal
    let results = runner.run(&options).await.expect("Pipeline should succeed despite missing exports");
    assert_eq!(results.len(), 1);

    drop(runner);
    let events = collect_events(rx).await;

    // Should have an ExportsWarning event
    let has_warning = events.iter().any(|e| {
        matches!(e, PipelineEvent::ExportsWarning { message, .. } if message.contains("Failed to read"))
    });
    assert!(has_warning, "Expected ExportsWarning for missing file");
}

#[tokio::test]
async fn test_exports_missing_keys_emits_warning() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: exports-keys-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze the project. Write to {{exports_file}}"
    exports:
      keys: [language, framework, db]
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, rx) = mpsc::unbounded_channel();

    // Mock writes exports with only "language" key — framework and db are missing
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(ExportWriterMock::new(
            vec![("analyze", r#"{"language": "rust"}"#)],
            "done",
        )));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    assert_eq!(results.len(), 1);

    drop(runner);
    let events = collect_events(rx).await;

    // Should have ExportsLoaded (for the key that was present)
    let has_loaded = events.iter().any(|e| matches!(e, PipelineEvent::ExportsLoaded { .. }));
    assert!(has_loaded, "Expected ExportsLoaded event");

    // Should have warnings for missing keys
    let warnings: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PipelineEvent::ExportsWarning { message, .. } = e {
                Some(message.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(warnings.len(), 2, "Expected 2 warnings for missing keys (framework, db)");
    assert!(warnings.iter().any(|w| w.contains("framework")));
    assert!(warnings.iter().any(|w| w.contains("db")));
}

#[tokio::test]
async fn test_exports_available_in_command_stages() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let yaml = r#"
name: exports-cmd-test
stages:
  - id: analyze
    name: Analyze
    model: mock-model
    system_prompt: "You are an analyzer."
    user_prompt: "Analyze. Write to {{exports_file}}"
    exports: {}
  - id: build
    name: Build
    commands:
      - "echo {{build_target}} > build_mode.txt"
"#
    .to_string();
    let config = parse_config(&yaml);

    let (tx, _rx) = mpsc::unbounded_channel();

    // Mock writes exports JSON with build_target when executing "analyze"
    let runner = PipelineRunner::new(config, yaml, dir.path().to_path_buf(), tx)
        .with_runtime(Box::new(ExportWriterMock::new(
            vec![("analyze", r#"{"build_target": "release"}"#)],
            "done",
        )));

    let options = RunOptions {
        resume: false,
        dry_run: false,
    };

    let results = runner.run(&options).await.expect("Pipeline should succeed");
    assert_eq!(results.len(), 2);

    // Verify the exported variable was rendered in the command
    let content = std::fs::read_to_string(dir.path().join("build_mode.txt")).unwrap();
    assert!(
        content.contains("release"),
        "Exported variable should be rendered in command stage, got: {}",
        content
    );
}
