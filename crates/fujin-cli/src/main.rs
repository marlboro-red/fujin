use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use console::style;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;
use fujin_config::{PipelineConfig, resolve_includes, validate, validate_includes, KNOWN_RUNTIMES};
use fujin_core::{
    CheckpointManager, PipelineRunner, RunOptions,
    create_runtime, paths,
    event::PipelineEvent,
    artifact::FileChangeKind,
    util::truncate_chars,
};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::mpsc;

/// fujin — AI Agent Pipeline CLI
///
/// Execute multi-stage AI agent pipelines using Claude Code or GitHub Copilot CLI.
/// Run with no arguments to launch the interactive TUI.
#[derive(Parser)]
#[command(name = "fujin", version, about)]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a pipeline
    Run {
        /// Path or name of the pipeline YAML config
        #[arg(short, long)]
        config: String,

        /// Resume from the last checkpoint
        #[arg(long)]
        resume: bool,

        /// Validate and show plan without running
        #[arg(long)]
        dry_run: bool,

        /// Override template variables (key=value)
        #[arg(long = "var", value_parser = parse_var)]
        vars: Vec<(String, String)>,
    },

    /// Validate a pipeline config without running it
    Validate {
        /// Path to the pipeline YAML config
        #[arg(short, long)]
        config: PathBuf,
    },

    /// Check agent runtime availability
    Agents {
        /// Run health checks
        #[arg(long)]
        check: bool,
    },

    /// Initialize a new pipeline config from a template
    Init {
        /// Template to use
        #[arg(long, default_value = "simple")]
        template: String,

        /// Output filename for the config
        #[arg(short, long, default_value = "pipeline.yaml")]
        output: PathBuf,

        /// List all available templates (built-in + user)
        #[arg(long)]
        list: bool,

        /// Write output to the current directory instead of the global configs directory
        #[arg(long)]
        local: bool,
    },

    /// Set up the data directory and install built-in templates
    Setup,

    /// Open a pipeline config in an editor (defaults to VS Code)
    Edit {
        /// Path or name of the pipeline YAML config to edit
        config: String,

        /// Editor command to use (defaults to "code")
        #[arg(long, default_value = "code")]
        editor: String,
    },

    /// Manage checkpoints
    Checkpoint {
        #[command(subcommand)]
        action: CheckpointAction,
    },
}

#[derive(Subcommand)]
enum CheckpointAction {
    /// List all checkpoints
    List,

    /// Show details of a specific checkpoint
    Show {
        /// Run ID to show
        run_id: String,
    },

    /// Remove all checkpoints
    Clean,
}

fn parse_var(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid variable format: '{s}' (expected key=value)"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load .env if present
    let _ = dotenvy::dotenv();

    // If no subcommand, launch the TUI
    let Some(command) = cli.command else {
        return fujin_tui::run_tui().await;
    };

    // Initialize tracing (only for CLI subcommands, not TUI)
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();

    match command {
        Commands::Run {
            config,
            resume,
            dry_run,
            vars,
        } => cmd_run(config, resume, dry_run, vars).await,

        Commands::Validate { config } => cmd_validate(config),

        Commands::Agents { check } => cmd_agents(check).await,

        Commands::Init {
            template,
            output,
            list,
            local,
        } => cmd_init(template, output, list, local),

        Commands::Setup => cmd_setup(),

        Commands::Edit { config, editor } => cmd_edit(config, editor),

        Commands::Checkpoint { action } => cmd_checkpoint(action),
    }
}

/// Resolve a config argument to a file path.
///
/// If the value contains a path separator or exists as-is, use it directly.
/// Otherwise, treat it as a name and search:
///   1. `./<name>.yaml` in the current directory
///   2. `<configs_dir>/<name>.yaml` in the global data directory
fn resolve_config(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);

    // If it looks like an explicit path (has separator or has extension), try it directly
    if value.contains('/') || value.contains('\\') || path.extension().is_some() {
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        // Also check the global configs directory
        let global = paths::configs_dir().join(value);
        if global.exists() {
            return Ok(global);
        }
        anyhow::bail!("Config file not found: {value}");
    }

    // Treat as a name — try current dir first, then global configs dir
    let local = PathBuf::from(format!("{value}.yaml"));
    if local.exists() {
        return Ok(local);
    }

    let global = paths::configs_dir().join(format!("{value}.yaml"));
    if global.exists() {
        return Ok(global);
    }

    anyhow::bail!(
        "Config not found: '{value}'\n  Searched:\n    ./{value}.yaml\n    {}",
        global.display()
    );
}

/// Spawn a task that listens for PipelineEvents and renders CLI output.
fn spawn_cli_display(mut rx: mpsc::UnboundedReceiver<PipelineEvent>) {
    tokio::spawn(async move {
        let mut spinner: Option<ProgressBar> = None;

        while let Some(event) = rx.recv().await {
            match event {
                PipelineEvent::PipelineStarted {
                    pipeline_name,
                    total_stages,
                    runtime,
                    ..
                } => {
                    let rt_label = match runtime.as_str() {
                        "copilot-cli" => "copilot",
                        "claude-code" => "claude",
                        other => other,
                    };
                    println!(
                        "\n{} {} ({}, {})",
                        style("Pipeline:").bold().cyan(),
                        style(&pipeline_name).bold(),
                        style(format!("{total_stages} stages")).dim(),
                        style(rt_label).dim()
                    );
                    println!("{}", style("─".repeat(60)).dim());
                }

                PipelineEvent::Resuming {
                    run_id,
                    start_index,
                    total_stages,
                } => {
                    println!(
                        "{} Resuming from stage {}/{} (run: {})",
                        style("↻").yellow(),
                        start_index + 1,
                        total_stages,
                        style(&run_id).dim()
                    );
                }

                PipelineEvent::StageStarted {
                    stage_index,
                    stage_name,
                    model,
                    runtime,
                    allowed_tools,
                    isolated,
                    ..
                } => {
                    // Clear any existing spinner
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                    }
                    let rt_label = match runtime.as_str() {
                        "copilot-cli" => "copilot",
                        "claude-code" => "claude",
                        other => other,
                    };
                    println!(
                        "\n{} Stage {}: {}",
                        style("▶").green().bold(),
                        stage_index + 1,
                        style(&stage_name).bold()
                    );
                    println!(
                        "  {} {} · model={}",
                        style("⚙").dim(),
                        style(rt_label).magenta().bold(),
                        style(&model).cyan()
                    );
                    if !allowed_tools.is_empty() {
                        println!(
                            "  {} tools: {}",
                            style("⚙").dim(),
                            style(allowed_tools.join(", ")).dim()
                        );
                    }
                    if isolated {
                        println!(
                            "  {} {}",
                            style("⚙").dim(),
                            style("isolated workspace").dim()
                        );
                    }
                }

                PipelineEvent::AgentRunning { stage_id, .. } => {
                    let s = ProgressBar::new_spinner();
                    s.set_style(
                        ProgressStyle::with_template("  {spinner:.cyan} {msg}")
                            .unwrap()
                            .tick_strings(&[
                                "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
                            ]),
                    );
                    s.set_message(format!("Running {stage_id}..."));
                    s.enable_steady_tick(std::time::Duration::from_millis(100));
                    spinner = Some(s);
                }

                PipelineEvent::StageCompleted {
                    duration,
                    artifacts,
                    token_usage,
                    ..
                } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                        spinner = None;
                    }
                    println!(
                        "  {} Completed in {:.1}s — {}",
                        style("✓").green().bold(),
                        duration.as_secs_f64(),
                        style(artifacts.summary()).dim()
                    );

                    if let Some(ref usage) = token_usage {
                        println!(
                            "  {} Tokens: {} in / {} out",
                            style("📊").dim(),
                            usage.input_tokens,
                            usage.output_tokens
                        );
                    }

                    for change in &artifacts.changes {
                        let icon = match change.kind {
                            FileChangeKind::Created => style("+").green(),
                            FileChangeKind::Modified => style("~").yellow(),
                            FileChangeKind::Deleted => style("-").red(),
                        };
                        println!("    {} {}", icon, change.path.display());
                    }
                }

                PipelineEvent::StageFailed {
                    stage_id,
                    error,
                    checkpoint_saved,
                    ..
                } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                        spinner = None;
                    }
                    eprintln!(
                        "  {} Stage '{}' failed: {}",
                        style("✗").red().bold(),
                        stage_id,
                        error
                    );
                    if checkpoint_saved {
                        eprintln!(
                            "  {} Checkpoint saved. Resume with: fujin run --resume -c <config>",
                            style("💾").dim()
                        );
                    }
                }

                PipelineEvent::PipelineCompleted {
                    total_duration,
                    stages_completed,
                    token_usage_by_model,
                } => {
                    println!("{}", style("─".repeat(60)).dim());
                    println!(
                        "{} Pipeline complete in {:.1}s — {} stages executed",
                        style("✓").green().bold(),
                        total_duration.as_secs_f64(),
                        stages_completed
                    );

                    if !token_usage_by_model.is_empty() {
                        println!();
                        println!("  {}", style("Token usage by model:").bold());
                        let mut total_in: u64 = 0;
                        let mut total_out: u64 = 0;
                        for (model, usage) in &token_usage_by_model {
                            total_in += usage.input_tokens;
                            total_out += usage.output_tokens;
                            println!(
                                "    {} {:>10} in / {:>10} out",
                                style(format!("{:<30}", model)).cyan(),
                                usage.input_tokens,
                                usage.output_tokens,
                            );
                        }
                        if token_usage_by_model.len() > 1 {
                            println!(
                                "    {} {:>10} in / {:>10} out",
                                style(format!("{:<30}", "Total")).bold(),
                                total_in,
                                total_out,
                            );
                        }
                    }
                }

                PipelineEvent::PipelineFailed { error } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                    }
                    eprintln!(
                        "\n{} Pipeline failed: {}",
                        style("✗").red().bold(),
                        error
                    );
                }

                PipelineEvent::CommandRunning {
                    command_index,
                    command,
                    total_commands,
                    ..
                } => {
                    if let Some(ref s) = spinner {
                        s.set_message(format!(
                            "Running command [{}/{}]: {}",
                            command_index + 1,
                            total_commands,
                            truncate_chars(&command, 60)
                        ));
                    } else {
                        let s = ProgressBar::new_spinner();
                        s.set_style(
                            ProgressStyle::with_template("  {spinner:.cyan} {msg}")
                                .unwrap()
                                .tick_strings(&[
                                    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
                                ]),
                        );
                        s.set_message(format!(
                            "Running command [{}/{}]: {}",
                            command_index + 1,
                            total_commands,
                            truncate_chars(&command, 60)
                        ));
                        s.enable_steady_tick(std::time::Duration::from_millis(100));
                        spinner = Some(s);
                    }
                }

                PipelineEvent::PipelineCancelled { stage_index } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                    }
                    eprintln!(
                        "\n  {} Pipeline cancelled at stage {}",
                        style("⚠").yellow(),
                        stage_index + 1
                    );
                }

                PipelineEvent::RetryGroupAttempt {
                    group_name,
                    attempt,
                    max_retries,
                    failed_stage_id,
                    error,
                    ..
                } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                        spinner = None;
                    }
                    eprintln!(
                        "  {} Retry group '{}' — attempt {}/{} (stage '{}' failed: {})",
                        style("↻").yellow().bold(),
                        group_name,
                        attempt,
                        max_retries,
                        failed_stage_id,
                        truncate_chars(&error, 80)
                    );
                }

                PipelineEvent::RetryLimitReached {
                    group_name,
                    total_attempts,
                    response_tx,
                } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                        spinner = None;
                    }
                    eprintln!(
                        "\n  {} Retry group '{}' exhausted {} attempts.",
                        style("⚠").yellow().bold(),
                        group_name,
                        total_attempts
                    );
                    eprint!("  Continue with another round of retries? [y/N] ");
                    use std::io::{self, BufRead};
                    let answer = io::stdin()
                        .lock()
                        .lines()
                        .next()
                        .and_then(|l| l.ok())
                        .unwrap_or_default();
                    let should_continue = matches!(
                        answer.trim().to_lowercase().as_str(),
                        "y" | "yes"
                    );
                    let _ = response_tx.send(should_continue);
                }

                PipelineEvent::VerifyRunning { group_name, .. } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                        spinner = None;
                    }
                    eprintln!(
                        "  {} Verifying retry group '{}'…",
                        style("⟳").cyan().bold(),
                        group_name
                    );
                }

                PipelineEvent::VerifyPassed { group_name, .. } => {
                    eprintln!(
                        "  {} Retry group '{}' verification passed",
                        style("✓").green().bold(),
                        group_name
                    );
                }

                PipelineEvent::VerifyFailed { group_name, response, .. } => {
                    eprintln!(
                        "  {} Retry group '{}' verification failed: {}",
                        style("✗").red().bold(),
                        group_name,
                        truncate_chars(&response, 80)
                    );
                }

                PipelineEvent::StageSkipped {
                    stage_index,
                    stage_id,
                    reason,
                } => {
                    if let Some(ref s) = spinner {
                        s.finish_and_clear();
                        spinner = None;
                    }
                    println!(
                        "\n{} Stage {}: {} {}",
                        style("⊘").dim(),
                        stage_index + 1,
                        style(&stage_id).dim(),
                        style(format!("({})", reason)).dim()
                    );
                }

                PipelineEvent::BranchEvaluating { .. } => {
                    if let Some(ref s) = spinner {
                        s.set_message("Evaluating branch classifier...");
                    }
                }

                PipelineEvent::BranchSelected {
                    selected_route,
                    available_routes,
                    ..
                } => {
                    println!(
                        "  {} Branch selected: {} (from: {})",
                        style("⑂").cyan().bold(),
                        style(&selected_route).green().bold(),
                        style(available_routes.join(", ")).dim()
                    );
                }

                PipelineEvent::ExportsLoaded {
                    variables,
                    ..
                } => {
                    println!(
                        "  {} Exported {} variable{}",
                        style("↗").cyan().bold(),
                        variables.len(),
                        if variables.len() == 1 { "" } else { "s" }
                    );
                    for (key, value) in &variables {
                        println!(
                            "    {} = {}",
                            style(key).bold(),
                            style(truncate_chars(value, 60)).dim()
                        );
                    }
                }

                PipelineEvent::ExportsWarning {
                    stage_id,
                    message,
                    ..
                } => {
                    eprintln!(
                        "  {} exports (stage '{}'): {}",
                        style("⚠").yellow(),
                        stage_id,
                        message
                    );
                }

                // Tick / activity / context-building are handled by the spinner
                _ => {}
            }
        }
    });
}

async fn cmd_run(
    config_value: String,
    resume: bool,
    dry_run: bool,
    vars: Vec<(String, String)>,
) -> Result<()> {
    let config_path = resolve_config(&config_value)?;

    // Load config
    let config_yaml = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;

    let mut config = PipelineConfig::from_yaml(&config_yaml)
        .with_context(|| "Failed to parse pipeline config")?;

    // Validate includes before resolution
    let include_validation = validate_includes(&config);
    if !include_validation.is_valid() {
        eprintln!("{} Include validation failed:", style("✗").red().bold());
        for err in &include_validation.errors {
            eprintln!("  {} {err}", style("•").red());
        }
        anyhow::bail!("Invalid include directives");
    }

    // Resolve includes (flatten included pipelines into this config)
    let base_dir = config_path.parent().unwrap_or(Path::new("."));
    config = resolve_includes(config, base_dir)
        .with_context(|| "Failed to resolve includes")?;

    // Re-serialize for checkpoint hashing (now includes resolved stages)
    let config_yaml = serde_yml::to_string(&config)
        .with_context(|| "Failed to re-serialize resolved config")?;

    // Apply variable overrides
    let overrides: HashMap<String, String> = vars.into_iter().collect();
    config.apply_overrides(&overrides);

    // Validate
    let validation = validate(&config);
    if !validation.is_valid() {
        eprintln!("{} Config validation failed:", style("✗").red().bold());
        for err in &validation.errors {
            eprintln!("  {} {err}", style("•").red());
        }
        anyhow::bail!("Invalid pipeline config");
    }

    for warning in &validation.warnings {
        eprintln!("  {} {warning}", style("⚠").yellow());
    }

    // Handle dry run before creating the runner
    if dry_run {
        print_dry_run(&config);
        return Ok(());
    }

    // Models run in the directory where fujin was invoked.
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root = std::fs::canonicalize(&workspace_root).unwrap_or(workspace_root);

    // Create event channel and spawn CLI display
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_cli_display(rx);

    // Create and run pipeline
    let runner = PipelineRunner::new(config, config_yaml, workspace_root, tx);

    let options = RunOptions {
        resume,
        dry_run: false,
    };

    let results = runner.run(&options).await?;

    println!(
        "\n{} {} stage(s) completed successfully.",
        style("Done!").green().bold(),
        results.len()
    );

    Ok(())
}

/// Print a dry run plan showing what would execute.
fn print_dry_run(config: &PipelineConfig) {
    println!(
        "\n{} (no stages will be executed)\n",
        style("DRY RUN").bold().yellow()
    );

    for (i, stage) in config.stages.iter().enumerate() {
        println!(
            "  {} Stage {}: {} [model={}{}]",
            style("○ pending").dim(),
            i + 1,
            style(&stage.name).bold(),
            stage.model,
            stage
                .timeout_secs
                .map(|t| format!(", timeout={t}s"))
                .unwrap_or_default(),
        );

        if !stage.allowed_tools.is_empty() {
            println!("      tools: {}", stage.allowed_tools.join(", "));
        }
    }

    if !config.variables.is_empty() {
        println!("\n  {}:", style("Variables").bold());
        for (k, v) in &config.variables {
            let display_val = truncate_chars(v, 60);
            println!("    {} = {}", style(k).cyan(), display_val);
        }
    }
}

fn cmd_validate(config_path: PathBuf) -> Result<()> {
    let config_yaml = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;

    let mut config = PipelineConfig::from_yaml(&config_yaml)
        .with_context(|| "Failed to parse pipeline config")?;

    // Validate and resolve includes
    let include_validation = validate_includes(&config);
    if !include_validation.is_valid() {
        eprintln!("{} Include validation failed:", style("✗").red().bold());
        for err in &include_validation.errors {
            eprintln!("  {} {err}", style("•").red());
        }
        anyhow::bail!("Invalid include directives");
    }

    let base_dir = config_path.parent().unwrap_or(Path::new("."));
    config = resolve_includes(config, base_dir)
        .with_context(|| "Failed to resolve includes")?;

    let validation = validate(&config);

    if validation.is_valid() {
        println!(
            "{} Config is valid: {} ({} stages)",
            style("✓").green().bold(),
            config.name,
            config.stages.len()
        );

        for (i, stage) in config.stages.iter().enumerate() {
            println!(
                "  {}. {} (id={}, model={})",
                i + 1,
                stage.name,
                stage.id,
                stage.model
            );
        }
    } else {
        eprintln!("{} Config validation failed:", style("✗").red().bold());
        for err in &validation.errors {
            eprintln!("  {} {err}", style("•").red());
        }
    }

    for warning in &validation.warnings {
        eprintln!("  {} {warning}", style("⚠").yellow());
    }

    if !validation.is_valid() {
        anyhow::bail!("Validation failed");
    }

    Ok(())
}

async fn cmd_agents(check: bool) -> Result<()> {
    println!("{}", style("Agent Runtimes").bold());
    println!("{}", style("─".repeat(40)).dim());

    let runtime_labels: HashMap<&str, &str> = HashMap::from([
        ("claude-code", "Claude Code CLI"),
        ("copilot-cli", "GitHub Copilot CLI"),
    ]);

    if check {
        for name in KNOWN_RUNTIMES {
            let label = runtime_labels.get(name).unwrap_or(name);
            print!("  {label}... ");
            match create_runtime(name) {
                Ok(runtime) => match runtime.health_check().await {
                    Ok(()) => println!("{}", style("available").green()),
                    Err(e) => {
                        println!("{}", style("unavailable").red());
                        eprintln!("    {e}");
                    }
                },
                Err(e) => {
                    println!("{}", style("error").red());
                    eprintln!("    {e}");
                }
            }
        }
    } else {
        for name in KNOWN_RUNTIMES {
            let label = runtime_labels.get(name).unwrap_or(name);
            println!("  • {name} ({label})");
        }
        println!("\n  Use --check to test availability.");
    }

    Ok(())
}

/// Built-in templates compiled into the binary.
const BUILT_IN_TEMPLATES: &[(&str, &str)] = &[
    ("simple", include_str!("../configs/simple.yaml")),
    ("code-and-docs", include_str!("../configs/code-and-docs.yaml")),
];

/// Get template content by name, checking the templates directory first, then built-ins.
fn get_template(name: &str) -> Result<String> {
    // Check user templates directory first
    let user_path = paths::templates_dir().join(format!("{name}.yaml"));
    if user_path.exists() {
        return std::fs::read_to_string(&user_path)
            .with_context(|| format!("Failed to read template: {}", user_path.display()));
    }

    // Fall back to built-in templates
    for (builtin_name, content) in BUILT_IN_TEMPLATES {
        if *builtin_name == name {
            return Ok(content.to_string());
        }
    }

    anyhow::bail!(
        "Unknown template: '{name}'. Use --list to see available templates."
    );
}

/// List all available templates (built-in + user-added).
fn list_templates() -> Vec<(String, bool)> {
    let mut templates: Vec<(String, bool)> = BUILT_IN_TEMPLATES
        .iter()
        .map(|(name, _)| (name.to_string(), true))
        .collect();

    // Scan user templates directory
    let templates_dir = paths::templates_dir();
    if let Ok(entries) = std::fs::read_dir(&templates_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let name = stem.to_string();
                    if !templates.iter().any(|(n, _)| n == &name) {
                        templates.push((name, false));
                    }
                }
            }
        }
    }

    templates.sort_by(|a, b| a.0.cmp(&b.0));
    templates
}

fn cmd_init(template: String, output: PathBuf, list: bool, local: bool) -> Result<()> {
    if list {
        let templates = list_templates();
        println!("{}", style("Available Templates").bold());
        println!("{}", style("─".repeat(40)).dim());
        for (name, is_builtin) in &templates {
            let label = if *is_builtin { "built-in" } else { "user" };
            println!("  {} {}", style(name).cyan(), style(format!("({label})")).dim());
        }
        if templates.is_empty() {
            println!("  No templates found. Run 'fujin setup' to install built-in templates.");
        }
        return Ok(());
    }

    let output = if local {
        output
    } else {
        // Default: save to global configs directory
        paths::ensure_dirs()?;
        paths::configs_dir().join(output.file_name().unwrap_or(output.as_os_str()))
    };

    if output.exists() {
        anyhow::bail!(
            "File already exists: {}. Remove it first or choose a different output path.",
            output.display()
        );
    }

    let content = get_template(&template)?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    std::fs::write(&output, content)
        .with_context(|| format!("Failed to write config to {}", output.display()))?;

    println!(
        "{} Created {} from '{}' template",
        style("✓").green().bold(),
        style(output.display()).bold(),
        template
    );

    Ok(())
}

fn cmd_setup() -> Result<()> {
    paths::ensure_dirs()?;

    let templates_dir = paths::templates_dir();

    // Copy built-in templates to templates directory
    let mut installed = 0;
    for (name, content) in BUILT_IN_TEMPLATES {
        let dest = templates_dir.join(format!("{name}.yaml"));
        std::fs::write(&dest, content)
            .with_context(|| format!("Failed to write template: {}", dest.display()))?;
        installed += 1;
    }

    println!("{} fujin data directory set up:", style("✓").green().bold());
    println!("  Templates: {} ({installed} installed)", style(templates_dir.display()).cyan());
    println!("  Configs:   {}", style(paths::configs_dir().display()).cyan());

    Ok(())
}

fn cmd_edit(config: String, editor: String) -> Result<()> {
    let path = resolve_config(&config)?;

    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(&editor);
        c
    } else {
        std::process::Command::new(&editor)
    };
    cmd.arg(&path);

    cmd.status()
        .with_context(|| format!("Failed to launch editor '{editor}'"))?;

    Ok(())
}

fn cmd_checkpoint(action: CheckpointAction) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    match action {
        CheckpointAction::List => {
            let manager = CheckpointManager::new(&cwd);
            let checkpoints = manager.list()?;

            if checkpoints.is_empty() {
                println!("No checkpoints found.");
                return Ok(());
            }

            println!("{}", style("Checkpoints").bold());
            println!("{}", style("─".repeat(60)).dim());

            for cp in &checkpoints {
                println!(
                    "  {} — {} stages completed, next: stage {}",
                    style(&cp.run_id).cyan(),
                    cp.completed_stages,
                    cp.next_stage_index + 1,
                );
                println!(
                    "    created: {}, updated: {}",
                    cp.created_at.format("%Y-%m-%d %H:%M:%S"),
                    cp.updated_at.format("%Y-%m-%d %H:%M:%S"),
                );
            }

            Ok(())
        }

        CheckpointAction::Show { run_id } => {
            let manager = CheckpointManager::new(&cwd);

            match manager.load(&run_id)? {
                Some(cp) => {
                    println!("{}", style("Checkpoint Details").bold());
                    println!("{}", style("─".repeat(60)).dim());
                    println!("  Run ID:      {}", cp.run_id);
                    let hash_short: String = cp.config_hash.chars().take(16).collect();
                    println!("  Config hash: {}", hash_short);
                    println!("  Next stage:  {}", cp.next_stage_index);
                    println!("  Created:     {}", cp.created_at);
                    println!("  Updated:     {}", cp.updated_at);

                    if !cp.completed_stages.is_empty() {
                        println!("\n  {}:", style("Completed Stages").bold());
                        for stage in &cp.completed_stages {
                            println!(
                                "    • {} — {:.1}s, {}",
                                style(&stage.stage_id).cyan(),
                                stage.duration.as_secs_f64(),
                                stage.artifacts.summary()
                            );
                        }
                    }
                }
                None => {
                    println!("Checkpoint not found: {run_id}");
                }
            }

            Ok(())
        }

        CheckpointAction::Clean => {
            let manager = CheckpointManager::new(&cwd);
            let count = manager.clean()?;

            if count > 0 {
                println!(
                    "{} Removed {count} checkpoint(s)",
                    style("✓").green().bold()
                );
            } else {
                println!("No checkpoints to clean.");
            }

            Ok(())
        }
    }
}
