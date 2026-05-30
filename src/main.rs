use std::sync::Arc;

use clap::{Parser, Subcommand};
use ra::{BashTool, Event, MockModel, Model, PiModel, ReadTool, Session};
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

/// Build a Model based on environment. Banner + log go to stderr only.
fn build_model() -> Arc<dyn Model> {
    match std::env::var("PI_API_KEY") {
        Ok(key) if !key.is_empty() => {
            let base = std::env::var("PI_BASE_URL")
                .unwrap_or_else(|_| "https://pi-api-us.macaron.xin".into());
            let model_id = std::env::var("PI_MODEL").unwrap_or_else(|_| "gpt-5.5".into());
            eprintln!("[ra] using PiModel base={base} model={model_id}");
            Arc::new(PiModel::new(base, key, model_id))
        }
        _ => {
            eprintln!("[ra] PI_API_KEY not set; using MockModel");
            Arc::new(MockModel)
        }
    }
}

async fn run_acp() -> anyhow::Result<()> {
    eprintln!("{BANNER}");
    eprintln!("[ra] starting ACP server on stdio (protocol v1)");
    let model = build_model();
    ra::acp_server::run(model).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(())
}

async fn run_print(prompt: Option<String>) -> anyhow::Result<()> {
    eprintln!("{BANNER}");

    let prompt = prompt.unwrap_or_else(|| "bash:echo hello from ra && uname -sr".to_string());
    let model = build_model();

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

    session.prompt(prompt).await?;
    printer.await?;
    Ok(())
}
