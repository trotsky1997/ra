use std::sync::Arc;

use clap::{Parser, Subcommand};
use llm::builder::LLMBackend;
use ra::{
    acp_server::ModelFactory, default_builtins, Event, LlmModel, LlmModelConfig, MockModel, Model,
    Session,
};
use tokio::io::{self, AsyncWriteExt};

const BANNER: &str = r#"
       ___,,,___
   ,~~"   _   "~~,
  /     ,' `.     \
 |  __ /     \ __  |
 | / .'\\___//`. \ |
  \\__/         \__//
       Ra · 𓂀
   rust-native agent
"#;

#[derive(Parser)]
#[command(name = "ra", version, about = "Ra — rust-native agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Legacy positional prompt; equivalent to `ra run <PROMPT>`.
    prompt: Option<String>,

    /// Path to TOML config (defaults to RA_CONFIG env, then ./ra.toml,
    /// then ~/.ra.toml).
    #[arg(long, global = true)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a project-local ra.toml and .ra/skills/ layout.
    Init {
        /// Overwrite generated files that already exist.
        #[arg(long)]
        force: bool,
        /// Also create .ra/skills/example/SKILL.md as a starter skill.
        #[arg(long)]
        example_skill: bool,
    },
    /// Run a single prompt and stream events to stdout (print mode).
    Run {
        /// Prompt text. Use prefixes like `bash:` / `read:` with the mock model.
        prompt: Option<String>,
    },
    /// Serve Ra as an ACP-compatible agent over stdio (JSON-RPC 2.0).
    Acp,
    /// Serve Ra as an A2A-compatible agent over HTTP + gRPC.
    Serve {
        /// HTTP port (JSON-RPC, REST, agent card).
        #[arg(long, default_value_t = 3000)]
        http_port: u16,
        /// gRPC port.
        #[arg(long, default_value_t = 50051)]
        grpc_port: u16,
    },
    /// Resume a previously-saved trajectory and continue with a new prompt.
    Resume {
        /// Session id (the bare ULID, e.g. `01ABC…`). See `ra sessions` to list.
        id: String,
        /// Follow-up prompt sent after the saved history is hydrated.
        prompt: String,
    },
    /// List saved sessions in the current cwd's bucket.
    Sessions,
    /// Interactive terminal UI. Requires the `tui` cargo feature
    /// (and a nightly toolchain — opentui_rust uses edition 2024).
    Tui,
}

enum RuntimeCmd {
    Run { prompt: Option<String> },
    Acp,
    Serve { http_port: u16, grpc_port: u16 },
    Resume { id: String, prompt: String },
    Sessions,
    Tui,
    LegacyPrompt { prompt: Option<String> },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cmd = match cli.cmd {
        Some(Cmd::Init {
            force,
            example_skill,
        }) => return run_init(force, example_skill),
        Some(Cmd::Acp) => RuntimeCmd::Acp,
        Some(Cmd::Run { prompt }) => RuntimeCmd::Run { prompt },
        Some(Cmd::Serve {
            http_port,
            grpc_port,
        }) => RuntimeCmd::Serve {
            http_port,
            grpc_port,
        },
        Some(Cmd::Resume { id, prompt }) => RuntimeCmd::Resume { id, prompt },
        Some(Cmd::Sessions) => RuntimeCmd::Sessions,
        Some(Cmd::Tui) => RuntimeCmd::Tui,
        None => RuntimeCmd::LegacyPrompt { prompt: cli.prompt },
    };

    let config = ra::config::RaConfig::load(cli.config.as_deref())?;
    apply_run_config(&config.run);
    apply_obs_config(&config.obs);

    match cmd {
        RuntimeCmd::Acp => run_acp(&config).await,
        RuntimeCmd::Run { prompt } => run_print(prompt, &config).await,
        RuntimeCmd::Serve {
            http_port,
            grpc_port,
        } => run_serve(http_port, grpc_port, &config).await,
        RuntimeCmd::Resume { id, prompt } => run_resume(&id, prompt, &config).await,
        RuntimeCmd::Sessions => run_list_sessions(&config).await,
        RuntimeCmd::Tui => run_tui(&config).await,
        RuntimeCmd::LegacyPrompt { prompt } => run_print(prompt, &config).await,
    }
}

fn run_init(force: bool, example_skill: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let report = ra::init::init_project(
        &cwd,
        ra::init::InitOptions {
            force,
            example_skill,
        },
    )?;

    println!("Initialized Ra project in {}", cwd.display());
    print_init_paths("created", &cwd, &report.created);
    print_init_paths("overwritten", &cwd, &report.overwritten);
    print_init_paths("skipped existing", &cwd, &report.skipped);
    if report.created.is_empty() && report.overwritten.is_empty() {
        println!("No files changed.");
    }
    Ok(())
}

fn print_init_paths(label: &str, cwd: &std::path::Path, paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    println!("{label}:");
    for path in paths {
        let display = path.strip_prefix(cwd).unwrap_or(path);
        println!("  {}", display.display());
    }
}

#[cfg(feature = "tui")]
async fn run_tui(config: &ra::config::RaConfig) -> anyhow::Result<()> {
    ra::tui::run(config).await
}

#[cfg(not(feature = "tui"))]
async fn run_tui(_config: &ra::config::RaConfig) -> anyhow::Result<()> {
    eprintln!(
        "[ra] this binary was built without the `tui` feature; \
         rebuild with `cargo +nightly build --features tui` (opentui_rust requires nightly)"
    );
    Ok(())
}

/// One advertised model option. The id is what the client sends back over
/// `session/set_model`; the spec is consumed by `EnvModelFactory::build`.
#[derive(Clone)]
struct ModelEntry {
    id: String,
    name: String,
    backend: LLMBackend,
    api_key: String,
    backend_model: String,
    base_url: Option<String>,
}

/// Default factory: enumerates whatever providers we have credentials for.
struct EnvModelFactory {
    entries: Vec<ModelEntry>,
}

impl EnvModelFactory {
    fn new() -> Self {
        let mut entries = Vec::new();

        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            if !key.is_empty() {
                let model = std::env::var("RA_MODEL").unwrap_or_else(|_| "claude-opus-4-5".into());
                entries.push(ModelEntry {
                    id: format!("anthropic/{model}"),
                    name: format!("Anthropic {model}"),
                    backend: LLMBackend::Anthropic,
                    api_key: key,
                    backend_model: model,
                    base_url: None,
                });
            }
        }

        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            if !key.is_empty() {
                let model = std::env::var("RA_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".into());
                let base_url = std::env::var("OPENAI_BASE_URL").ok();
                entries.push(ModelEntry {
                    id: format!("openai/{model}"),
                    name: format!("OpenAI {model}"),
                    backend: LLMBackend::OpenAI,
                    api_key: key,
                    backend_model: model,
                    base_url,
                });
            }
        }

        if let Ok(key) = std::env::var("PI_API_KEY") {
            if !key.is_empty() {
                let base = std::env::var("PI_BASE_URL")
                    .unwrap_or_else(|_| "https://pi-api-us.macaron.xin/v1/".into());
                let model = std::env::var("PI_MODEL")
                    .or_else(|_| std::env::var("RA_MODEL"))
                    .unwrap_or_else(|_| "gpt-5.5".into());
                entries.push(ModelEntry {
                    id: format!("pi/{model}"),
                    name: format!("pi {model}"),
                    backend: LLMBackend::OpenAI,
                    api_key: key,
                    backend_model: model,
                    base_url: Some(base),
                });
            }
        }

        Self { entries }
    }

    fn first(&self) -> Option<&ModelEntry> {
        self.entries.first()
    }
}

impl ModelFactory for EnvModelFactory {
    fn build(&self, model_id: &str) -> Option<Arc<dyn Model>> {
        let entry = self.entries.iter().find(|e| e.id == model_id)?;
        let m = LlmModel::build(LlmModelConfig {
            backend: entry.backend.clone(),
            api_key: entry.api_key.clone(),
            model: entry.backend_model.clone(),
            base_url: entry.base_url.clone(),
        })
        .ok()?;
        Some(Arc::new(m))
    }

    fn default_model_id(&self) -> String {
        self.first()
            .map(|e| e.id.clone())
            .unwrap_or_else(|| "mock".to_string())
    }

    fn available(&self) -> Vec<agent_client_protocol::schema::ModelInfo> {
        use agent_client_protocol::schema::{ModelId, ModelInfo};
        self.entries
            .iter()
            .map(|e| ModelInfo::new(ModelId::from(e.id.clone()), e.name.clone()))
            .collect()
    }
}

/// Build the default Model + a factory for runtime model swapping.
/// Build the default Model + a factory for runtime model swapping.
///
/// Resolution: if the loaded config has any `[[models]]` entries, those
/// drive the factory. Otherwise we fall back to the env-only path
/// (ANTHROPIC_API_KEY / OPENAI_API_KEY / PI_API_KEY).
fn build_model(config: &ra::config::RaConfig) -> (Arc<dyn Model>, Arc<dyn ModelFactory>) {
    let factory = if !config.models.is_empty() {
        let mut entries = Vec::with_capacity(config.models.len());
        for m in &config.models {
            let backend = match parse_backend(&m.backend) {
                Some(b) => b,
                None => {
                    eprintln!(
                        "[ra::config] unknown backend '{}' for model '{}', skipping",
                        m.backend, m.name
                    );
                    continue;
                }
            };
            let api_key = match m.resolve_api_key() {
                Some(k) => k,
                None => {
                    eprintln!("[ra::config] no API key for model '{}', skipping", m.name);
                    continue;
                }
            };
            entries.push(ModelEntry {
                id: format!("{}/{}", m.name, m.model_id),
                name: format!("{} {}", m.name, m.model_id),
                backend,
                api_key,
                backend_model: m.model_id.clone(),
                base_url: m.base_url.clone(),
            });
        }
        // Honour explicit default if specified
        if let Some(default_name) = &config.model.default {
            if let Some(idx) = entries
                .iter()
                .position(|e| e.id.starts_with(&format!("{}/", default_name)))
            {
                if idx != 0 {
                    entries.swap(0, idx);
                }
            } else {
                eprintln!("[ra::config] model.default = '{default_name}' not found in [[models]]");
            }
        }
        EnvModelFactory { entries }
    } else {
        EnvModelFactory::new()
    };

    let model: Arc<dyn Model> = if let Some(entry) = factory.first().cloned() {
        eprintln!(
            "[ra] default model: {} (backend={:?})",
            entry.id, entry.backend
        );
        factory
            .build(&entry.id)
            .expect("default model build failed")
    } else {
        eprintln!("[ra] no API key set; using MockModel");
        Arc::new(MockModel)
    };
    (model, Arc::new(factory))
}

/// Map config backend string → llm crate enum.
fn parse_backend(s: &str) -> Option<LLMBackend> {
    match s.to_ascii_lowercase().as_str() {
        "openai" => Some(LLMBackend::OpenAI),
        "anthropic" => Some(LLMBackend::Anthropic),
        "google" => Some(LLMBackend::Google),
        "deepseek" => Some(LLMBackend::DeepSeek),
        "ollama" => Some(LLMBackend::Ollama),
        "groq" => Some(LLMBackend::Groq),
        "xai" => Some(LLMBackend::XAI),
        _ => None,
    }
}

/// Apply the `[run]` section of the config: data_dir → RA_HOME, etc.
/// Mutates process env so downstream code (which already reads env) picks
/// up the new values.
fn apply_run_config(run: &ra::config::RunSection) {
    if let Some(d) = &run.data_dir {
        let expanded = shellexpand::tilde(d).to_string();
        // SAFETY: process env is single-threaded at this point (main).
        unsafe { std::env::set_var("RA_HOME", expanded) };
    }
}

/// Apply the `[obs]` section: copies into the env vars `nemo_obs::init`
/// already reads, so the existing observability code path is the one
/// source of truth.
fn apply_obs_config(obs: &ra::config::ObsSection) {
    if let Some(b) = &obs.backend {
        if !b.is_empty() && std::env::var("RA_OBS_BACKEND").is_err() {
            unsafe { std::env::set_var("RA_OBS_BACKEND", b) };
        }
    }
    if let Some(ep) = &obs.otel_endpoint {
        if !ep.is_empty() && std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_err() {
            unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", ep) };
        }
    }
}

fn load_graphify_workflow(config: &ra::config::RaConfig) -> Option<ra::graphify::GraphifyWorkflow> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let workflow = ra::graphify::workflow_from_config(&config.graphify, &cwd);
    if let Some(w) = &workflow {
        if let Some(project) = &w.project {
            eprintln!(
                "[ra] Graphify graph {} at {} ({} node(s), {} edge(s))",
                w.status.kind.as_str(),
                project.graph_path.display(),
                project.stats.node_count,
                project.stats.edge_count
            );
        } else {
            eprintln!(
                "[ra] Graphify graph {} at {}",
                w.status.kind.as_str(),
                w.graph_path.display()
            );
        }
    }
    workflow
}

/// Build A2aTool list from `[[a2a.remote_agents]]`. Each agent may carry
/// `auth.bearer_env` to point at a Bearer-token env var; when present, we
/// resolve it here and bake it into the per-agent reqwest client.
async fn load_a2a_tools_from_config(config: &ra::config::RaConfig) -> Vec<Arc<dyn ra::Tool>> {
    let mut out: Vec<Arc<dyn ra::Tool>> = Vec::new();
    for agent in &config.a2a.remote_agents {
        let bearer = agent
            .auth
            .as_ref()
            .and_then(|a| a.bearer_env.as_ref())
            .and_then(|env| std::env::var(env).ok())
            .filter(|s| !s.is_empty());
        if let Some(t) =
            ra::a2a_tool::load_remote_tool_with_bearer(&agent.name, &agent.url, bearer.as_deref())
                .await
        {
            out.push(t);
        }
    }
    out
}

async fn run_acp(config: &ra::config::RaConfig) -> anyhow::Result<()> {
    if config.run.banner {
        eprintln!("{BANNER}");
    }
    eprintln!("[ra] starting ACP server on stdio (protocol v1)");
    let (model, factory) = build_model(config);
    let mut extra_tools = default_builtins(&config.tools.builtin);
    extra_tools.extend(ra::a2a_tool::load_remote_tools_from_env().await);
    extra_tools.extend(load_a2a_tools_from_config(config).await);
    extra_tools.extend(ra::mcp::load_mcp_tools(&config.mcp.servers).await);
    let graphify_workflow = load_graphify_workflow(config);
    if let Some(workflow) = &graphify_workflow {
        extra_tools.extend(ra::graphify::tools_for_workflow(workflow));
    }
    let (system_prompt, prompt_templates) =
        load_skills_and_prompts(config, graphify_workflow.clone());
    let hooks = build_hooks(config);
    let rtk = ra::RtkRewriter::from_config(&config.rtk);
    ra::acp_server::run(
        model,
        factory,
        extra_tools,
        system_prompt,
        prompt_templates,
        hooks,
        rtk,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(())
}

async fn run_serve(
    http_port: u16,
    grpc_port: u16,
    config: &ra::config::RaConfig,
) -> anyhow::Result<()> {
    let http_port = config.a2a.serve.http_port.unwrap_or(http_port);
    let grpc_port = config.a2a.serve.grpc_port.unwrap_or(grpc_port);
    if config.run.banner {
        eprintln!("{BANNER}");
    }
    eprintln!("[ra] starting A2A server (HTTP :{http_port}, gRPC :{grpc_port})");
    let (model, factory) = build_model(config);
    let mut extra_tools = default_builtins(&config.tools.builtin);
    extra_tools.extend(ra::a2a_tool::load_remote_tools_from_env().await);
    extra_tools.extend(load_a2a_tools_from_config(config).await);
    extra_tools.extend(ra::mcp::load_mcp_tools(&config.mcp.servers).await);
    let graphify_workflow = load_graphify_workflow(config);
    if let Some(workflow) = &graphify_workflow {
        extra_tools.extend(ra::graphify::tools_for_workflow(workflow));
    }
    let (system_prompt, prompt_templates) =
        load_skills_and_prompts(config, graphify_workflow.clone());
    let hooks = build_hooks(config);
    let bearer = config
        .a2a
        .serve
        .auth
        .as_ref()
        .and_then(|a| a.bearer_env.as_ref())
        .and_then(|env| std::env::var(env).ok())
        .filter(|s| !s.is_empty());
    ra::a2a_server::run(
        model,
        factory,
        http_port,
        grpc_port,
        extra_tools,
        system_prompt,
        prompt_templates,
        hooks,
        bearer,
        ra::RtkRewriter::from_config(&config.rtk),
    )
    .await
}

fn build_hooks(config: &ra::config::RaConfig) -> Option<Arc<ra::hooks::HookEngine>> {
    let engine = ra::hooks::HookEngine::from_config(&config.hooks);
    if engine.is_empty() {
        None
    } else {
        eprintln!("[ra] hooks: configured");
        Some(Arc::new(engine))
    }
}

/// Read `[skills]`, `[prompts]`, `[agents_md]`, `[openspec]`, and
/// `[resources]` and return the composed system prompt + slash-template
/// map. Thin wrapper over `ra::skills::build_resource_bundle` (shared
/// with the TUI).
fn load_skills_and_prompts(
    config: &ra::config::RaConfig,
    graphify: Option<ra::graphify::GraphifyWorkflow>,
) -> (
    Option<String>,
    Arc<std::collections::HashMap<String, String>>,
) {
    let mut bundle = ra::skills::build_resource_bundle(config, true);
    bundle.graphify = graphify;
    let system_prompt = bundle.build_system_prompt();
    let templates = bundle.prompt_map();
    (system_prompt, Arc::new(templates))
}

async fn run_print(prompt: Option<String>, config: &ra::config::RaConfig) -> anyhow::Result<()> {
    if config.run.banner {
        eprintln!("{BANNER}");
    }

    let prompt = prompt.unwrap_or_else(|| "bash:echo hello from ra && uname -sr".to_string());
    let (model, _factory) = build_model(config);

    let hooks = build_hooks(config);
    let rtk = ra::RtkRewriter::from_config(&config.rtk);
    let graphify_workflow = load_graphify_workflow(config);
    let (system_prompt, _templates) = load_skills_and_prompts(config, graphify_workflow.clone());
    let mut tools = default_builtins(&config.tools.builtin);
    if let Some(workflow) = &graphify_workflow {
        tools.extend(ra::graphify::tools_for_workflow(workflow));
    }
    let mut sess = Session::new(model, tools).with_rtk(rtk);
    if let Some(h) = hooks {
        sess = sess.with_hooks(h);
    }
    let session = Arc::new(sess);
    if let Some(sp) = system_prompt {
        session.set_system_prompt(sp).await;
    }

    let printer = spawn_event_printer(session.subscribe());

    let _outcome = session.prompt(prompt).await?;
    printer.await?;

    // Persist the trajectory to disk so `ra resume <id>` can pick it up.
    // ACP/A2A paths do this through SessionRunner; the print-mode path
    // doesn't currently use SessionRunner, so we save inline here.
    let session_id = ulid::Ulid::new().to_string();
    let cwd = std::env::current_dir()?;
    if let Ok(store) = ra::store::SessionStore::for_cwd(&cwd) {
        let messages = session.snapshot_messages().await;
        let traj = ra::atif_codec::encode(&session_id, build_model_name(config), &messages);
        match store.save(&traj).await {
            Ok(_) => eprintln!(
                "[ra] saved session {session_id} (resume with `ra resume {session_id} <prompt>`)"
            ),
            Err(e) => eprintln!("[ra] warning: could not save session: {e:#}"),
        }
    }
    Ok(())
}

fn spawn_event_printer(
    mut rx: tokio::sync::broadcast::Receiver<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stdout = io::stdout();
        loop {
            match rx.recv().await {
                Ok(Event::AgentStart) => {
                    let _ = stdout.write_all(b"\n[agent_start]\n").await;
                }
                Ok(Event::TurnStart) => {
                    let _ = stdout.write_all(b"[turn_start]\n").await;
                }
                Ok(Event::TextDelta(s)) => {
                    let _ = stdout.write_all(s.as_bytes()).await;
                    let _ = stdout.flush().await;
                }
                Ok(Event::ThinkingDelta(s)) => {
                    let _ = stdout.write_all(format!("[think] {s}").as_bytes()).await;
                }
                Ok(Event::ToolCallStart(c)) => {
                    let _ = stdout
                        .write_all(
                            format!("\n[tool_start] {} input={}\n", c.name, c.input).as_bytes(),
                        )
                        .await;
                }
                Ok(Event::ToolCallUpdate { id, chunk }) => {
                    let _ = stdout
                        .write_all(format!("[tool_update {id}] {chunk}\n").as_bytes())
                        .await;
                }
                Ok(Event::ToolCallEnd(r)) => {
                    let head: String = r.content.chars().take(120).collect();
                    let _ = stdout
                        .write_all(
                            format!("[tool_end {} err={}]\n{head}\n", r.call_id, r.is_error)
                                .as_bytes(),
                        )
                        .await;
                }
                Ok(Event::TurnEnd) => {
                    let _ = stdout.write_all(b"[turn_end]\n").await;
                }
                Ok(Event::AgentEnd) => {
                    let _ = stdout.write_all(b"[agent_end]\n").await;
                    break;
                }
                Ok(Event::Error(e)) => {
                    let _ = stdout.write_all(format!("[error] {e}\n").as_bytes()).await;
                }
                Err(_) => break,
            }
        }
    })
}

/// Resume a saved trajectory from disk and continue with a new prompt.
/// Bucket comes from the current cwd (same key SessionStore uses to save).
async fn run_resume(id: &str, prompt: String, config: &ra::config::RaConfig) -> anyhow::Result<()> {
    if config.run.banner {
        eprintln!("{BANNER}");
    }

    let cwd = std::env::current_dir()?;
    let store = ra::store::SessionStore::for_cwd(&cwd)?;
    let traj = store
        .load(id)
        .await
        .map_err(|e| anyhow::anyhow!("load session {id}: {e:#}"))?;
    let messages = ra::atif_codec::decode(&traj);
    eprintln!(
        "[ra] resumed session {id}: {} messages from {}",
        messages.len(),
        store.path_for(id).display()
    );

    let (model, _factory) = build_model(config);
    let hooks = build_hooks(config);
    let rtk = ra::RtkRewriter::from_config(&config.rtk);
    let graphify_workflow = load_graphify_workflow(config);
    let (system_prompt, _templates) = load_skills_and_prompts(config, graphify_workflow.clone());
    let mut tools = default_builtins(&config.tools.builtin);
    if let Some(workflow) = &graphify_workflow {
        tools.extend(ra::graphify::tools_for_workflow(workflow));
    }
    let mut sess = Session::new(model, tools).with_rtk(rtk);
    if let Some(h) = hooks {
        sess = sess.with_hooks(h);
    }
    let session = Arc::new(sess);
    if let Some(sp) = system_prompt {
        session.set_system_prompt(sp).await;
    }
    session.restore_messages(messages).await;

    let printer = spawn_event_printer(session.subscribe());
    let _outcome = session.prompt(prompt).await?;
    printer.await?;

    // Persist the (now-extended) trajectory back to disk so the same id
    // continues to resolve to the latest history.
    let updated = session.snapshot_messages().await;
    let model_name = build_model_name(config);
    let traj = ra::atif_codec::encode(id, model_name, &updated);
    if let Err(e) = store.save(&traj).await {
        eprintln!("[ra] warning: failed to save resumed trajectory: {e:#}");
    }
    Ok(())
}

/// `ra sessions` — list saved sessions in the current cwd's bucket.
async fn run_list_sessions(_config: &ra::config::RaConfig) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let store = ra::store::SessionStore::for_cwd(&cwd)?;
    let metas = store.list().await?;
    if metas.is_empty() {
        eprintln!(
            "[ra] no saved sessions in bucket {}",
            store.bucket().display()
        );
        return Ok(());
    }
    println!("# saved sessions in {}", store.bucket().display());
    for m in metas {
        let modified =
            chrono::DateTime::<chrono::Utc>::from(m.modified).format("%Y-%m-%d %H:%M:%S UTC");
        match m.title.as_deref() {
            Some(title) => println!("{}\t{modified}\t{title}", m.session_id),
            None => println!("{}\t{modified}", m.session_id),
        }
    }
    Ok(())
}

/// Try to recover the default model id for trajectory metadata.
/// Best-effort: if no models are configured (mock-only run), returns None.
fn build_model_name(config: &ra::config::RaConfig) -> Option<String> {
    if config.models.is_empty() {
        return None;
    }
    config
        .model
        .default
        .clone()
        .or_else(|| config.models.first().map(|m| m.name.clone()))
}
