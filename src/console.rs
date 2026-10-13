use std::io::{BufRead, IsTerminal};

use tokio::sync::watch;

use crate::error::{AgentError, AgentResult};
use crate::launch::PendingEditor;
use crate::server::LauncherStatus;

pub async fn run(app: &str, no_browser: bool) -> AgentResult<()> {
    ignore_input();
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
        "Keep this console open. Press Ctrl+C or close the console to stop only this bridge. Enter does not stop it. Your browser and saved draft stay open."
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
            _ = &mut stop => { user_stopped = true; break; }
        }
    }
    drop(bridge);
    let _ = reader_task.join();
    println!("This bridge has stopped. Other workspaces are unchanged.");
    if !user_stopped && std::io::stdin().is_terminal() {
        wait_for_close(stop).await;
    }
    Ok(())
}

fn ignore_input() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            // Discard input without treating Enter or EOF as a stop request.
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        });
    });
}

pub async fn wait_for_close(stop: impl std::future::Future<Output = std::io::Result<()>>) {
    ignore_input();
    tokio::pin!(stop);
    tokio::select! {
        // Register the control-event listener before showing the close instruction.
        biased;
        _ = &mut stop => {}
        _ = async {
            println!("Press Ctrl+C or close this console.");
            std::future::pending::<()>().await;
        } => {}
    }
}
