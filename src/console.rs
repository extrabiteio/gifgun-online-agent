use std::io::{BufRead, IsTerminal};

use tokio::sync::{oneshot, watch};

use crate::error::{AgentError, AgentResult};
use crate::launch::PendingEditor;
use crate::server::LauncherStatus;

pub async fn run(app: &str, no_browser: bool) -> AgentResult<()> {
    println!("GifGun Agent {}", env!("CARGO_PKG_VERSION"));
    println!(
        "Opening a separate editor workspace. Sign in and approve the connection in the browser."
    );
    run_session(app, no_browser).await
}

async fn run_session(app: &str, no_browser: bool) -> AgentResult<()> {
    let pending = PendingEditor::start(app, no_browser, true)?;
    let instruction = crate::handoff::instruction(&pending.session_id)?;
    println!("Workspace: {}", pending.session_id);
    if no_browser {
        println!("Editor: {}", pending.editor_url);
    }
    println!(
        "Keep this console open. Press Enter or Ctrl+C to stop only this bridge. Your browser and saved draft stay open."
    );
    let mut bridge = pending.own()?;
    let reader = bridge
        .reader
        .take()
        .ok_or_else(|| AgentError::State("the launcher status channel is unavailable".into()))?;
    let (sender, mut status) = watch::channel(LauncherStatus::Waiting);
    let reader_task = std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };
            let Ok(value) = serde_json::from_str::<LauncherStatus>(&line) else {
                break;
            };
            sender.send_replace(value);
        }
    });
    let (entered, mut input) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read_line(&mut String::new());
        let _ = entered.send(());
    });
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    let mut previous = None;
    let mut user_stopped = false;
    loop {
        let current = *status.borrow_and_update();
        if previous != Some(current) {
            match current {
                LauncherStatus::Waiting => println!("Waiting for the editor."),
                LauncherStatus::ApprovalPending => {
                    println!("Approve this connection in the browser.")
                }
                LauncherStatus::Connected => println!(
                    "Connected. Copy the following instruction into your AI agent:\n\n--- AI instruction ---\n{instruction}\n--- End of AI instruction ---"
                ),
                LauncherStatus::Reconnecting => println!(
                    "The editor is disconnected. Return to the same workspace to reconnect. Do not give a new agent the old instruction while disconnected."
                ),
                LauncherStatus::Stopped => break,
            }
            previous = Some(current);
        }
        tokio::select! {
            changed = status.changed() => { if changed.is_err() { break; } }
            _ = &mut input => { user_stopped = true; break; }
            _ = &mut stop => { user_stopped = true; break; }
        }
    }
    drop(bridge);
    let _ = reader_task.join();
    println!("This bridge has stopped. Other workspaces are unchanged.");
    if !user_stopped && std::io::stdin().is_terminal() {
        println!("Press Enter to close this console.");
        let _ = input.await;
    }
    Ok(())
}
