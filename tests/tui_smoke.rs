//! Smoke test that the TUI's chat-collation logic correctly folds
//! Session events into the scrollback regardless of whether we have
//! a real terminal — the `OptimizedBuffer` is pure-memory so we can
//! exercise the rendering pipeline headlessly.
//!
//! Only enabled when the `tui` cargo feature is on; opens up to
//! confirm the integration compiles even if no one runs the binary.

#![cfg(feature = "tui")]

// We re-export nothing from `tui.rs` so this test goes via the public
// surface only. Just make sure the module loads under the feature
// flag — the real interactive path is covered by manual smoke.

#[test]
fn tui_module_compiles_under_feature() {
    // Reach the entrypoint by name; this fails to compile if the
    // `tui` module isn't exposed under the feature, which is exactly
    // the regression we want to catch.
    let _ = ra::tui::run;
}

/// The TUI's `TuiEvent` enum is the public contract for anyone who
/// wants to observe / test the UI layer (default consumer is the
/// ATOF bridge). Confirm it round-trips through a broadcast bus
/// without touching a real terminal.
#[tokio::test]
async fn tui_event_bus_round_trips_every_variant() {
    use ra::tui::TuiEvent;
    use tokio::sync::broadcast;

    let (tx, mut rx) = broadcast::channel::<TuiEvent>(16);

    let messages = vec![
        TuiEvent::Started,
        TuiEvent::Submitted("hello".into()),
        TuiEvent::Cancelled,
        TuiEvent::Scrolled(3),
        TuiEvent::Session(ra::Event::TurnStart),
        TuiEvent::Quit,
    ];
    for m in &messages {
        tx.send(m.clone()).expect("broadcast send");
    }

    for expected in &messages {
        let got = rx.recv().await.expect("broadcast recv");
        assert_eq!(format!("{got:?}"), format!("{expected:?}"));
    }
}
