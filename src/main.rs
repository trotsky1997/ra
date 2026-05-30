use std::sync::Arc;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    eprintln!("{BANNER}");

    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "bash:echo hello from ra && uname -sr".to_string()
    });

    // 选模型：有 PI_API_KEY 就接真后端，否则用 mock 跑骨架。
    let model: Arc<dyn Model> = match std::env::var("PI_API_KEY") {
        Ok(key) if !key.is_empty() => {
            let base = std::env::var("PI_BASE_URL")
                .unwrap_or_else(|_| "https://pi-api-us.macaron.xin".into());
            let model_id =
                std::env::var("PI_MODEL").unwrap_or_else(|_| "gpt-5.5".into());
            eprintln!("[ra] using PiModel base={base} model={model_id}");
            Arc::new(PiModel::new(base, key, model_id))
        }
        _ => {
            eprintln!("[ra] PI_API_KEY not set; using MockModel");
            Arc::new(MockModel)
        }
    };

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
                            format!("\n[tool_start] {} input={}\n", c.name, c.input)
                                .as_bytes(),
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
