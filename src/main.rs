use std::sync::Arc;

use clap::{Parser, Subcommand};
use llm::builder::LLMBackend;
use ra::{
    acp_server::ModelFactory, BashTool, Event, LlmModel, LlmModelConfig, MockModel, Model, ReadTool,
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
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a single prompt and stream events to stdout (print mode).
    Run {
        /// Prompt text. Use prefixes like `bash:` / `read:` with the mock model.
        prompt: Option<String>,
    },
    /// Serve Ra as an ACP-compatible agent over stdio (JSON-RPC 2.0).
    Acp,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Some(Cmd::Acp) => run_acp().await,
        Some(Cmd::Run { prompt }) => run_print(prompt).await,
        None => run_print(cli.prompt).await,
    }
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
                let model = std::env::var("RA_MODEL")
                    .unwrap_or_else(|_| "claude-opus-4-5".into());
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
                let model =
                    std::env::var("RA_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".into());
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
fn build_model() -> (Arc<dyn Model>, Arc<dyn ModelFactory>) {
    let factory = EnvModelFactory::new();
    let model: Arc<dyn Model> = if let Some(entry) = factory.first().cloned() {
        eprintln!(
            "[ra] default model: {} (backend={:?})",
            entry.id, entry.backend
        );
        // Build via factory to keep the resolution path identical to set_model.
        factory
            .build(&entry.id)
            .expect("default model build failed")
    } else {
        eprintln!("[ra] no API key set; using MockModel");
        Arc::new(MockModel)
    };
    (model, Arc::new(factory))
}

async fn run_acp() -> anyhow::Result<()> {
    eprintln!("{BANNER}");
    eprintln!("[ra] starting ACP server on stdio (protocol v1)");
    let (model, factory) = build_model();
    ra::acp_server::run(model, factory)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(())
}

async fn run_print(prompt: Option<String>) -> anyhow::Result<()> {
    eprintln!("{BANNER}");

    let prompt = prompt.unwrap_or_else(|| "bash:echo hello from ra && uname -sr".to_string());
    let (model, _factory) = build_model();

    let session = Arc::new(Session::new(
        model,
        vec![Arc::new(ReadTool), Arc::new(BashTool)],
    ));

    let mut rx = session.subscribe();
    let printer = tokio::spawn(async move {
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
    });

    let _outcome = session.prompt(prompt).await?;
    printer.await?;
    Ok(())
}
