//! Interactive terminal UI for Ra, backed by opentui_rust.
//!
//! Layout: a bordered chat scrollback occupies most of the screen, with
//! a 3-row input box pinned to the bottom. The user types into the
//! input box; pressing Enter submits via `Session::prompt`. Streaming
//! TextDelta / ToolCallStart / ToolCallEnd events arrive on the
//! Session's broadcast bus and update the chat in real time.
//!
//! opentui_rust is a renderer engine, not a widget framework — we
//! hand-roll the layout with `OptimizedBuffer::draw_text` /
//! `draw_box`. The `Renderer` is `!Send` so we keep render and input
//! handling on the main thread; only the Session prompt-future is
//! spawned, and it communicates back exclusively via the broadcast bus.
//!
//! Gated behind the `tui` cargo feature because opentui_rust requires
//! nightly Rust (edition 2024).

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use opentui_rust::buffer::BoxStyle;
use opentui_rust::input::{Event as InputEvent, InputParser, KeyCode, KeyEvent, KeyModifiers};
use opentui_rust::renderer::RendererOptions;
use opentui_rust::terminal::{enable_raw_mode, terminal_size};
use opentui_rust::{Renderer, Rgba, Style};
use tokio::sync::{broadcast, mpsc};

use crate::config::RaConfig;
use crate::events::Event;
use crate::session::Session;
use crate::session_runner::{RunnerHost, SessionRunner};
use crate::store::{SessionMeta, SessionStore};
use crate::tools::default_builtins;

/// One rendered line in the scrollback. Kept minimal; everything else
/// is derived (palette, prefix, wrap) at draw time.
#[derive(Debug, Clone)]
enum ChatEntry {
    User(String),
    Agent(String),
    Tool {
        name: String,
        input: serde_json::Value,
        output: String,
        is_error: bool,
    },
    System(String),
}

/// In-flight tool call collation. `ToolCallStart` opens an entry,
/// `ToolCallUpdate` appends chunks, `ToolCallEnd` closes it.
#[derive(Debug, Clone, Default)]
struct PartialTool {
    name: String,
    input: serde_json::Value,
    chunks: String,
}

/// Public entry point — wired into `main.rs` behind `#[cfg(feature = "tui")]`.
pub async fn run(config: &RaConfig) -> Result<()> {
    if !is_a_tty() {
        anyhow::bail!(
            "ra tui requires an interactive terminal (stdin/stdout must be a TTY)"
        );
    }
    // Initialise ATOF / NeMo Relay once per process — same call ACP
    // and A2A make. Idempotent inside nemo_obs.
    crate::nemo_obs::init();

    let (session, prompt_templates) = build_session(config).await?;
    let session_id = ulid::Ulid::new().to_string();
    let mut app = TuiApp::new(session.clone(), session_id, prompt_templates, config).await?;

    // Wrap the entire interactive loop in one Agent-typed scope so
    // every nested LLM/tool/hook scope nests under it in the trace.
    // `with_task_scope` pins the task-local stack across .await points
    // so scopes pop cleanly across worker-thread migrations.
    let outcome = crate::nemo_obs::with_task_scope(async {
        let _agent = crate::nemo_obs::agent_scope("tui/session");
        app.run_loop().await
    })
    .await;

    let _ = app.ui_tx.send(TuiEvent::Quit);
    app.save_trajectory(config).await;
    outcome
}

/// Refuse to start if stdin isn't a TTY — opentui's raw-mode setup
/// would otherwise spew terminal escapes at whatever we're piped to.
fn is_a_tty() -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: libc::isatty just queries the fd; no aliasing concerns.
    let stdin_fd = std::io::stdin().as_raw_fd();
    let stdout_fd = std::io::stdout().as_raw_fd();
    unsafe { libc::isatty(stdin_fd) == 1 && libc::isatty(stdout_fd) == 1 }
}

/// Build the Session the same way `run_print` does — model + tools +
/// hooks + RTK + system prompt — so TUI mode honours every config knob.
/// Also returns the prompt-template map so the TUI runner can expand
/// `/skill-name args...` the same way ACP/A2A do.
async fn build_session(
    config: &RaConfig,
) -> Result<(Arc<Session>, Arc<HashMap<String, String>>)> {
    use crate::skills::ResourceBundle;
    let factory = build_model_factory(config);
    let model = factory
        .first_model()
        .ok_or_else(|| anyhow::anyhow!("no model configured; set ANTHROPIC_API_KEY / OPENAI_API_KEY / PI_API_KEY or [[models]] in ra.toml"))?;

    let hooks_engine = crate::hooks::HookEngine::from_config(&config.hooks);
    let hooks = if hooks_engine.is_empty() {
        None
    } else {
        Some(Arc::new(hooks_engine))
    };
    let rtk = crate::tools::RtkRewriter::from_config(&config.rtk);

    let mut bundle = ResourceBundle::default();
    if config.skills.enabled {
        let mut globs = if config.skills.discover {
            crate::skills::default_discover_globs()
        } else {
            Vec::new()
        };
        globs.extend(config.skills.paths.iter().cloned());
        bundle.skills = crate::skills::load_skills(&globs);
    }
    if config.prompts.enabled {
        bundle.prompts = crate::skills::load_prompts(&config.prompts.paths);
    }
    if config.agents_md.enabled {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        bundle.agents_md = crate::skills::discover_agents_md(&cwd);
    }
    let system_prompt = bundle.build_system_prompt();
    let prompt_templates = Arc::new(bundle.prompt_map());

    let mut sess = Session::new(model, default_builtins(&config.tools.builtin)).with_rtk(rtk);
    if let Some(h) = hooks {
        sess = sess.with_hooks(h);
    }
    let session = Arc::new(sess);
    if let Some(sp) = system_prompt {
        session.set_system_prompt(sp).await;
    }
    Ok((session, prompt_templates))
}

// ---- Tiny model-factory shim ---------------------------------------------
//
// `main.rs` already encapsulates env-var → Model resolution behind its own
// EnvModelFactory. We don't want to copy 80 lines or move that helper to a
// public location for one caller, so the TUI uses a thin trait that the
// caller (main.rs) plugs in via `crate::config`. For now we duplicate just
// enough: load Anthropic / OpenAI / pi keys from env and pick the first.

trait FirstModel {
    fn first_model(&self) -> Option<Arc<dyn crate::model::Model>>;
}

struct EnvFirstModel;
impl FirstModel for EnvFirstModel {
    fn first_model(&self) -> Option<Arc<dyn crate::model::Model>> {
        // Anthropic
        if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
            if !k.is_empty() {
                let model = std::env::var("RA_MODEL").unwrap_or_else(|_| "claude-opus-4-5".into());
                return crate::llm_model::LlmModel::build(crate::llm_model::LlmModelConfig {
                    backend: llm::builder::LLMBackend::Anthropic,
                    api_key: k, model, base_url: None,
                }).ok().map(|m| Arc::new(m) as Arc<dyn crate::model::Model>);
            }
        }
        // OpenAI
        if let Ok(k) = std::env::var("OPENAI_API_KEY") {
            if !k.is_empty() {
                let model = std::env::var("RA_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".into());
                let base_url = std::env::var("OPENAI_BASE_URL").ok();
                return crate::llm_model::LlmModel::build(crate::llm_model::LlmModelConfig {
                    backend: llm::builder::LLMBackend::OpenAI,
                    api_key: k, model, base_url,
                }).ok().map(|m| Arc::new(m) as Arc<dyn crate::model::Model>);
            }
        }
        // pi (Responses API endpoint)
        if let Ok(k) = std::env::var("PI_API_KEY") {
            if !k.is_empty() {
                let base = std::env::var("PI_BASE_URL")
                    .unwrap_or_else(|_| "https://pi-api-us.macaron.xin/v1/".into());
                let model = std::env::var("PI_MODEL")
                    .or_else(|_| std::env::var("RA_MODEL"))
                    .unwrap_or_else(|_| "gpt-5.5".into());
                return crate::llm_model::LlmModel::build(crate::llm_model::LlmModelConfig {
                    backend: llm::builder::LLMBackend::OpenAI,
                    api_key: k, model, base_url: Some(base),
                }).ok().map(|m| Arc::new(m) as Arc<dyn crate::model::Model>);
            }
        }
        // Fall back to MockModel so the TUI is still demoable without keys.
        Some(Arc::new(crate::model::MockModel) as Arc<dyn crate::model::Model>)
    }
}

fn build_model_factory(_config: &RaConfig) -> EnvFirstModel {
    EnvFirstModel
}

// ---- RunnerHost for TUI --------------------------------------------------
//
// The TUI doesn't need model-swap support (no ACP session/set_model), so
// the host is minimal: save the trajectory on each turn, report a fixed
// context window, and list whatever the env factory found.

struct TuiRunnerHost {
    session: Arc<Session>,
    session_id: String,
    config_model_name: Option<String>,
    ctx_window: u64,
}

#[async_trait]
impl RunnerHost for TuiRunnerHost {
    async fn save_session(&self, _session_id: &str) {
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return,
        };
        let store = match SessionStore::for_cwd(&cwd) {
            Ok(s) => s,
            Err(_) => return,
        };
        let messages = self.session.snapshot_messages().await;
        if messages.is_empty() {
            return;
        }
        let traj = crate::atif_codec::encode(
            &self.session_id,
            self.config_model_name.clone(),
            &messages,
        );
        if let Err(e) = store.save(&traj).await {
            eprintln!("[ra::tui] failed to save session: {e:#}");
        }
    }

    fn default_ctx_window(&self) -> u64 {
        self.ctx_window
    }

    fn list_models_for_display(&self) -> Vec<(String, String)> {
        // TUI doesn't support model switching; return the active model name.
        let name = self
            .config_model_name
            .clone()
            .unwrap_or_else(|| "mock".into());
        vec![(name.clone(), name)]
    }
}

// ---- Session browser modal -----------------------------------------------

/// State for the Ctrl-R session browser overlay.
struct SessionBrowser {
    sessions: Vec<SessionMeta>,
    selected: usize,
}

impl SessionBrowser {
    fn new(sessions: Vec<SessionMeta>) -> Self {
        Self { sessions, selected: 0 }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn move_down(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1).min(self.sessions.len() - 1);
        }
    }

    fn selected_meta(&self) -> Option<&SessionMeta> {
        self.sessions.get(self.selected)
    }
}

/// UI-layer events emitted by the TUI as the user drives it. Exposed
/// over a `tokio::sync::broadcast` so anyone (default consumer:
/// `nemo_obs`/ATOF; future: tests, log sinks) can observe what
/// happened in the chat without scraping the renderer.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// TUI started (after Session is built and the renderer is up).
    Started,
    /// User submitted a prompt; carries the trimmed text.
    Submitted(String),
    /// User pressed Ctrl-C while a prompt was running.
    Cancelled,
    /// User scrolled the scrollback by `delta` lines (positive = up).
    Scrolled(i32),
    /// One Session-bus event, threaded through verbatim.
    Session(Event),
    /// TUI is exiting cleanly.
    Quit,
}

struct TuiApp {
    session: Arc<Session>,
    runner: Arc<SessionRunner>,
    session_id: String,
    renderer: Renderer,
    _raw_guard: opentui_rust::terminal::RawModeGuard,
    parser: InputParser,
    stdin_rx: mpsc::UnboundedReceiver<u8>,
    event_rx: broadcast::Receiver<Event>,

    /// UI-layer event bus. The TUI publishes here whenever something
    /// observable happens; the default consumer (spawned in `new`)
    /// translates events into ATOF marks so TUI sessions show up in
    /// observability traces. Public-facing for testing too.
    ui_tx: broadcast::Sender<TuiEvent>,

    chat: Vec<ChatEntry>,
    current_text: String,
    current_tools: HashMap<String, PartialTool>,
    input_buf: String,
    scroll: usize,

    in_flight: Option<tokio::task::JoinHandle<()>>,
    last_ctrl_c: Option<std::time::Instant>,
    quit: bool,
    width: u32,
    height: u32,

    /// When Some, the session browser overlay is open.
    browser: Option<SessionBrowser>,
}

impl TuiApp {
    async fn new(
        session: Arc<Session>,
        session_id: String,
        prompt_templates: Arc<HashMap<String, String>>,
        config: &RaConfig,
    ) -> Result<Self> {
        let (tw, th) = terminal_size().unwrap_or((100, 32));
        let width = u32::from(tw);
        let height = u32::from(th);
        let opts = RendererOptions {
            use_alt_screen: true,
            hide_cursor: false,
            enable_mouse: false,
            ..Default::default()
        };
        let renderer = Renderer::new_with_options(width, height, opts)?;
        let raw_guard = enable_raw_mode()?;

        let config_model_name = config.model.default.clone()
            .or_else(|| config.models.first().map(|m| m.name.clone()));
        let host = Arc::new(TuiRunnerHost {
            session: session.clone(),
            session_id: session_id.clone(),
            config_model_name,
            ctx_window: 200_000,
        });
        let runner = Arc::new(
            SessionRunner::new(session.clone(), session_id.clone(), host)
                .with_prompt_templates(prompt_templates),
        );

        // stdin → mpsc bridge: reading stdin blocks the OS thread, so we
        // do it on a dedicated blocking task and ship bytes back.
        let (tx, rx) = mpsc::unbounded_channel::<u8>();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 64];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        for &b in &buf[..n] {
                            if tx.send(b).is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let event_rx = session.subscribe();
        let (ui_tx, _) = broadcast::channel::<TuiEvent>(128);
        // Default consumer: bridge UI events into ATOF marks so TUI
        // sessions show up in observability traces alongside ACP/A2A
        // sessions. Spawned once and detached; if everyone stops
        // sending the channel just closes and the task exits.
        spawn_atof_bridge(ui_tx.subscribe());
        let mut app = Self {
            session,
            runner,
            session_id,
            renderer,
            _raw_guard: raw_guard,
            parser: InputParser::new(),
            stdin_rx: rx,
            event_rx,
            ui_tx,
            chat: vec![ChatEntry::System(
                "Ra TUI — type a prompt and press Enter. Ctrl-C cancels, Ctrl-R browses sessions, Ctrl-D quits.".into(),
            )],
            current_text: String::new(),
            current_tools: HashMap::new(),
            input_buf: String::new(),
            scroll: 0,
            in_flight: None,
            last_ctrl_c: None,
            quit: false,
            width,
            height,
            browser: None,
        };
        let _ = app.ui_tx.send(TuiEvent::Started);
        app.draw()?;
        Ok(app)
    }

    async fn run_loop(&mut self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_millis(33));
        let mut input_buf: Vec<u8> = Vec::new();
        while !self.quit {
            tokio::select! {
                biased;
                ev = self.event_rx.recv() => match ev {
                    Ok(e) => self.on_session_event(e),
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                },
                Some(byte) = self.stdin_rx.recv() => {
                    input_buf.push(byte);
                    while !input_buf.is_empty() {
                        match self.parser.parse(&input_buf) {
                            Ok((evt, consumed)) => {
                                input_buf.drain(..consumed);
                                self.on_input_event(evt);
                            }
                            Err(_) => break, // incomplete or empty
                        }
                    }
                },
                _ = tick.tick() => {}
            }
            // Settle the in-flight handle.
            if let Some(h) = &self.in_flight {
                if h.is_finished() {
                    self.in_flight = None;
                }
            }
            self.draw()?;
        }
        Ok(())
    }

    fn on_session_event(&mut self, e: Event) {
        // Re-publish on the UI bus so the ATOF bridge (and any other
        // subscriber) sees the event in TUI-context, even though it
        // originated from the Session bus.
        let _ = self.ui_tx.send(TuiEvent::Session(e.clone()));
        match e {
            Event::AgentStart | Event::TurnStart | Event::TurnEnd => {}
            Event::AgentEnd => {
                // Flush any in-flight assistant text into the scrollback.
                if !self.current_text.is_empty() {
                    self.chat.push(ChatEntry::Agent(std::mem::take(&mut self.current_text)));
                }
            }
            Event::TextDelta(s) | Event::ThinkingDelta(s) => {
                self.current_text.push_str(&s);
            }
            Event::ToolCallStart(c) => {
                self.flush_current_text();
                self.current_tools.insert(c.id.clone(), PartialTool {
                    name: c.name.clone(),
                    input: c.input.clone(),
                    chunks: String::new(),
                });
            }
            Event::ToolCallUpdate { id, chunk } => {
                if let Some(t) = self.current_tools.get_mut(&id) {
                    if !t.chunks.is_empty() {
                        t.chunks.push('\n');
                    }
                    t.chunks.push_str(&chunk);
                }
            }
            Event::ToolCallEnd(r) => {
                let partial = self.current_tools.remove(&r.call_id);
                let (name, input) = partial
                    .map(|p| (p.name, p.input))
                    .unwrap_or_else(|| ("?".into(), serde_json::Value::Null));
                self.chat.push(ChatEntry::Tool {
                    name,
                    input,
                    output: r.content,
                    is_error: r.is_error,
                });
            }
            Event::Error(e) => {
                self.chat.push(ChatEntry::System(format!("error: {e}")));
            }
        }
    }

    fn flush_current_text(&mut self) {
        if !self.current_text.is_empty() {
            self.chat.push(ChatEntry::Agent(std::mem::take(&mut self.current_text)));
        }
    }

    fn on_input_event(&mut self, ev: InputEvent) {
        let key = match ev {
            InputEvent::Key(k) => k,
            InputEvent::Resize(r) => {
                self.width = u32::from(r.width);
                self.height = u32::from(r.height);
                let _ = self.renderer.resize(self.width, self.height);
                return;
            }
            _ => return,
        };
        self.handle_key(key);
    }

    fn handle_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CTRL);

        // When the browser is open, all keys go to it.
        if self.browser.is_some() {
            match k.code {
                KeyCode::Esc => {
                    self.browser = None;
                }
                KeyCode::Up => {
                    if let Some(b) = &mut self.browser {
                        b.move_up();
                    }
                }
                KeyCode::Down => {
                    if let Some(b) = &mut self.browser {
                        b.move_down();
                    }
                }
                KeyCode::Enter => {
                    self.restore_selected_session();
                }
                _ => {}
            }
            return;
        }

        match k.code {
            KeyCode::Char('c') if ctrl => self.handle_ctrl_c(),
            KeyCode::Char('d') if ctrl => self.quit = true,
            KeyCode::Char('r') if ctrl => self.open_session_browser(),
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input_buf.pop();
            }
            KeyCode::Char(c) if !ctrl => {
                self.input_buf.push(c);
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_add(1);
                let _ = self.ui_tx.send(TuiEvent::Scrolled(1));
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_sub(1);
                let _ = self.ui_tx.send(TuiEvent::Scrolled(-1));
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(10);
                let _ = self.ui_tx.send(TuiEvent::Scrolled(10));
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                let _ = self.ui_tx.send(TuiEvent::Scrolled(-10));
            }
            _ => {}
        }
    }

    /// Open the session browser overlay. Loads sessions from disk synchronously
    /// (the list is small and the store is local); if the store is unavailable
    /// we show an error in the chat instead.
    fn open_session_browser(&mut self) {
        if self.in_flight.is_some() {
            self.chat.push(ChatEntry::System(
                "(cannot browse sessions while a prompt is running)".into(),
            ));
            return;
        }
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => {
                self.chat.push(ChatEntry::System(format!("session browser: cwd error: {e}")));
                return;
            }
        };
        let store = match SessionStore::for_cwd(&cwd) {
            Ok(s) => s,
            Err(e) => {
                self.chat.push(ChatEntry::System(format!("session browser: store error: {e}")));
                return;
            }
        };
        // Blocking list — acceptable here because we're on the main thread
        // between ticks and the bucket is local disk.
        let sessions = match std::fs::read_dir(store.bucket()) {
            Err(_) => Vec::new(),
            Ok(_) => {
                // Use the sync path: spawn_blocking isn't available without
                // an async context we can await, so we call the underlying
                // fs directly. SessionStore::list() is async; replicate the
                // cheap sync subset here.
                let mut out = Vec::new();
                if let Ok(dir) = std::fs::read_dir(store.bucket()) {
                    for entry in dir.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) != Some("json") {
                            continue;
                        }
                        let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                            Some(s) => s.to_string(),
                            None => continue,
                        };
                        let modified = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        let title = read_title_sync(&path);
                        out.push(SessionMeta { session_id, path, modified, title });
                    }
                }
                out.sort_by_key(|m| std::cmp::Reverse(m.modified));
                out
            }
        };
        if sessions.is_empty() {
            self.chat.push(ChatEntry::System("(no saved sessions found)".into()));
            return;
        }
        self.browser = Some(SessionBrowser::new(sessions));
    }

    /// Restore the session selected in the browser. Clears the current chat
    /// and hydrates the session from the selected trajectory.
    fn restore_selected_session(&mut self) {
        let meta = match self.browser.as_ref().and_then(|b| b.selected_meta()) {
            Some(m) => m.clone(),
            None => return,
        };
        self.browser = None;

        let bytes = match std::fs::read(&meta.path) {
            Ok(b) => b,
            Err(e) => {
                self.chat.push(ChatEntry::System(format!("restore error: {e}")));
                return;
            }
        };
        let traj: crate::atif::Trajectory = match serde_json::from_slice(&bytes) {
            Ok(t) => t,
            Err(e) => {
                self.chat.push(ChatEntry::System(format!("restore parse error: {e}")));
                return;
            }
        };
        let messages = crate::atif_codec::decode(&traj);
        let session = self.session.clone();
        let session_id = meta.session_id.clone();
        // Restore messages on the session. We need an async context; spawn a
        // task and let the next tick pick up the result via the event bus.
        tokio::spawn(async move {
            session.restore_messages(messages).await;
        });
        // Update our session_id so future saves go to the restored session's file.
        self.session_id = session_id.clone();
        self.chat.clear();
        self.chat.push(ChatEntry::System(format!(
            "Restored session {session_id}{}. Continue typing.",
            meta.title.as_deref().map(|t| format!(" — {t}")).unwrap_or_default(),
        )));
        self.scroll = 0;
    }

    fn handle_ctrl_c(&mut self) {
        if self.in_flight.is_some() {
            // First Ctrl-C cancels the running prompt.
            let s = self.session.clone();
            tokio::spawn(async move {
                s.cancel().await;
            });
            self.chat.push(ChatEntry::System("(cancelled)".into()));
            let _ = self.ui_tx.send(TuiEvent::Cancelled);
            return;
        }
        // No prompt running: double-tap quits.
        let now = std::time::Instant::now();
        match self.last_ctrl_c {
            Some(t) if now.duration_since(t) < Duration::from_secs(2) => self.quit = true,
            _ => {
                self.last_ctrl_c = Some(now);
                self.chat.push(ChatEntry::System(
                    "(press Ctrl-C again to quit, or Ctrl-D)".into(),
                ));
            }
        }
    }

    fn submit(&mut self) {
        let text = self.input_buf.trim().to_string();
        self.input_buf.clear();
        if text.is_empty() {
            return;
        }
        self.chat.push(ChatEntry::User(text.clone()));
        self.scroll = 0;
        let _ = self.ui_tx.send(TuiEvent::Submitted(text.clone()));
        let runner = self.runner.clone();
        self.in_flight = Some(tokio::spawn(async move {
            crate::nemo_obs::with_task_scope(async move {
                let _scope = crate::nemo_obs::agent_scope("tui/prompt");
                // Route through SessionRunner so /skill-name args... and
                // built-in slash commands (/clear, /compact, /models, /mode)
                // work identically to ACP/A2A.
                runner.run_input(text, |_ev| {}).await;
            })
            .await;
        }));
    }

    async fn save_trajectory(&self, config: &RaConfig) {
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return,
        };
        let store = match crate::store::SessionStore::for_cwd(&cwd) {
            Ok(s) => s,
            Err(_) => return,
        };
        let messages = self.session.snapshot_messages().await;
        if messages.is_empty() {
            return;
        }
        let id = &self.session_id;
        let model_name = config.model.default.clone()
            .or_else(|| config.models.first().map(|m| m.name.clone()));
        let traj = crate::atif_codec::encode(id, model_name, &messages);
        if let Err(e) = store.save(&traj).await {
            eprintln!("[ra::tui] failed to save session: {e:#}");
        } else {
            eprintln!(
                "[ra::tui] saved session {id} (resume with `ra resume {id} <prompt>`)"
            );
        }
    }

    fn draw(&mut self) -> Result<()> {
        let width = self.width;
        let height = self.height;

        // Precompute everything that needs `&self` BEFORE we hold a
        // `&mut buffer`, otherwise the borrow checker (rightly)
        // complains about overlapping borrows of `self`.
        let input_h: u32 = 3;
        let chat_h = height.saturating_sub(input_h);
        let inner_w = width.saturating_sub(2);
        let inner_h = chat_h.saturating_sub(2);
        let lines = self.render_chat_lines(inner_w as usize);

        let buf = self.renderer.buffer();
        buf.clear(Rgba::from_rgb_u8(15, 17, 26));

        // Borders.
        let border_style = Style::fg(Rgba::from_rgb_u8(80, 88, 110));
        if width >= 2 && chat_h >= 2 {
            buf.draw_box(0, 0, width, chat_h, BoxStyle::rounded(border_style));
        }
        if width >= 2 && input_h >= 2 {
            buf.draw_box(0, chat_h, width, input_h, BoxStyle::rounded(border_style));
        }

        // ---- Chat scrollback ---------------------------------------
        let total = lines.len();
        let scroll = self.scroll.min(total.saturating_sub(inner_h as usize));
        let end = total.saturating_sub(scroll);
        let start = end.saturating_sub(inner_h as usize);
        for (i, (style, text)) in lines[start..end].iter().enumerate() {
            buf.draw_text(1, 1 + i as u32, text, *style);
        }

        // ---- Input box ---------------------------------------------
        let prompt = "> ";
        let mut shown = format!("{prompt}{}", self.input_buf);
        let max = (inner_w as usize).saturating_sub(1);
        if shown.chars().count() > max {
            let drop_n = shown.chars().count() - max;
            shown = shown.chars().skip(drop_n).collect();
        }
        let input_style = if self.in_flight.is_some() {
            Style::fg(Rgba::from_rgb_u8(140, 140, 140))
        } else {
            Style::fg(Rgba::WHITE)
        };
        buf.draw_text(1, chat_h + 1, &shown, input_style);

        // Status pill in the input border, right side.
        let status = if self.in_flight.is_some() {
            "[thinking…]"
        } else {
            "[ready]"
        };
        let status_x = width.saturating_sub(status.len() as u32 + 1);
        if status_x > 0 {
            buf.draw_text(
                status_x,
                chat_h,
                status,
                Style::fg(Rgba::from_rgb_u8(120, 200, 120)),
            );
        }

        // ---- Session browser modal ---------------------------------
        if let Some(browser) = &self.browser {
            let modal_w = width.clamp(30, 70);
            let modal_h = (browser.sessions.len() as u32 + 4).min(height.saturating_sub(4)).max(5);
            let modal_x = (width.saturating_sub(modal_w)) / 2;
            let modal_y = (height.saturating_sub(modal_h)) / 2;

            let modal_border = Style::fg(Rgba::from_rgb_u8(200, 180, 100));
            let item_normal = Style::fg(Rgba::from_rgb_u8(200, 200, 200));
            let item_selected = Style::builder()
                .fg(Rgba::from_rgb_u8(30, 30, 30))
                .bg(Rgba::from_rgb_u8(120, 180, 240))
                .build();
            let header_style = Style::fg(Rgba::from_rgb_u8(200, 180, 100));

            buf.draw_box(modal_x, modal_y, modal_w, modal_h, BoxStyle::rounded(modal_border));
            let title = " Sessions (↑↓ navigate, Enter restore, Esc cancel) ";
            buf.draw_text(modal_x + 1, modal_y, title, header_style);

            let inner_w = modal_w.saturating_sub(2) as usize;
            let visible = (modal_h.saturating_sub(2)) as usize;
            let start = if browser.selected >= visible {
                browser.selected - visible + 1
            } else {
                0
            };
            for (i, meta) in browser.sessions.iter().enumerate().skip(start).take(visible) {
                let label = format!(
                    "{} {}",
                    &meta.session_id[..meta.session_id.len().min(8)],
                    meta.title.as_deref().unwrap_or("(no title)"),
                );
                let label: String = label.chars().take(inner_w).collect();
                let padded = format!("{label:<inner_w$}");
                let row = modal_y + 1 + (i - start) as u32;
                let style = if i == browser.selected { item_selected } else { item_normal };
                buf.draw_text(modal_x + 1, row, &padded, style);
            }
        }

        self.renderer.present()?;
        Ok(())
    }

    /// Build a flat (style, text) list for the scrollback. Each
    /// ChatEntry expands into one or more wrapped lines.
    fn render_chat_lines(&self, width: usize) -> Vec<(Style, String)> {
        let user = Style::fg(Rgba::from_rgb_u8(120, 180, 240));
        let agent = Style::fg(Rgba::WHITE);
        let agent_dim = Style::fg(Rgba::from_rgb_u8(180, 180, 180));
        let tool_ok = Style::fg(Rgba::from_rgb_u8(140, 220, 140));
        let tool_err = Style::fg(Rgba::from_rgb_u8(240, 130, 130));
        let sys = Style::fg(Rgba::from_rgb_u8(150, 130, 200));

        let mut out: Vec<(Style, String)> = Vec::new();
        for entry in &self.chat {
            match entry {
                ChatEntry::User(s) => {
                    push_wrapped(&mut out, user, &format!("you  ▸ {s}"), width);
                    out.push((user, String::new()));
                }
                ChatEntry::Agent(s) => {
                    push_wrapped(&mut out, agent, &format!("ra   ▸ {s}"), width);
                    out.push((agent, String::new()));
                }
                ChatEntry::Tool { name, input, output, is_error } => {
                    let style = if *is_error { tool_err } else { tool_ok };
                    let head = format!("tool ▸ {name}({})", short_json(input, 60));
                    push_wrapped(&mut out, style, &head, width);
                    let head_out = output
                        .lines()
                        .take(8)
                        .collect::<Vec<_>>()
                        .join("\n");
                    push_wrapped(&mut out, agent_dim, &head_out, width);
                    out.push((style, String::new()));
                }
                ChatEntry::System(s) => {
                    push_wrapped(&mut out, sys, &format!("· {s}"), width);
                }
            }
        }
        // In-flight assistant streaming.
        if !self.current_text.is_empty() {
            push_wrapped(&mut out, agent, &format!("ra   ▸ {}", self.current_text), width);
        }
        out
    }
}

fn push_wrapped(out: &mut Vec<(Style, String)>, style: Style, text: &str, width: usize) {
    if width == 0 {
        return;
    }
    for line in text.lines() {
        if line.is_empty() {
            out.push((style, String::new()));
            continue;
        }
        // Naive char-based wrap; good enough for ASCII / CJK is double-wide
        // but opentui handles width at draw time.
        let mut start = 0usize;
        let chars: Vec<char> = line.chars().collect();
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            let chunk: String = chars[start..end].iter().collect();
            out.push((style, chunk));
            start = end;
        }
    }
}

fn short_json(v: &serde_json::Value, max: usize) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Cheap synchronous title lookup for the session browser. Mirrors the
/// async `store::read_title` but runs on the main thread between ticks.
fn read_title_sync(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let traj: crate::atif::Trajectory = serde_json::from_slice(&bytes).ok()?;
    traj.steps
        .iter()
        .find(|s| matches!(s.source, crate::atif::StepSource::User))
        .map(|s| {
            let mut t = s.message.as_text();
            if t.len() > 60 {
                t.truncate(60);
                t.push('…');
            }
            t
        })
}

/// Default UI-event consumer: translates each `TuiEvent` into a NeMo
/// Relay `mark` so TUI sessions appear alongside ACP / A2A sessions
/// in ATOF traces. Spawned once from `TuiApp::new` and detached;
/// exits when every `ui_tx` clone is dropped.
fn spawn_atof_bridge(mut rx: broadcast::Receiver<TuiEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let label = match &ev {
                        TuiEvent::Started => "tui.started".to_string(),
                        TuiEvent::Submitted(_) => "tui.submitted".to_string(),
                        TuiEvent::Cancelled => "tui.cancelled".to_string(),
                        TuiEvent::Scrolled(_) => continue, // too noisy for ATOF
                        TuiEvent::Quit => "tui.quit".to_string(),
                        TuiEvent::Session(e) => match e {
                            // Session-bus events already get their own
                            // ATOF scopes lower in the stack
                            // (llm.stream, tool.<name>, hook.*); we
                            // just emit one extra "tui.turn.*" mark
                            // for the boundaries so a TUI-only
                            // observer can still tell turns apart.
                            Event::AgentStart => "tui.turn.start".to_string(),
                            Event::AgentEnd => "tui.turn.end".to_string(),
                            Event::Error(_) => "tui.turn.error".to_string(),
                            _ => continue,
                        },
                    };
                    crate::nemo_obs::mark(&label);
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => {} // tolerable
            }
        }
    })
}
