use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex};

#[derive(Debug, Deserialize)]
pub struct AuthCallback {
    pub code: Option<String>,
    pub state: Option<String>,

    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Clone)]
struct CallbackState {
    sender: Arc<Mutex<Option<oneshot::Sender<AuthCallback>>>>,
}

/// Port now comes from REDIRECT_URL rather than being hardcoded, so changing
/// one cannot silently break the other.
pub async fn start_callback_server(port: u16) -> Result<oneshot::Receiver<AuthCallback>> {
    let (sender, receiver) = oneshot::channel::<AuthCallback>();

    let state = CallbackState {
        sender: Arc::new(Mutex::new(Some(sender))),
    };

    let app = Router::new()
        .route("/callback", get(callback_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("cannot bind 127.0.0.1:{port} — is another instance running?"))?;

    println!("Listening on http://localhost:{port}/callback");

    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("Callback server error: {error}");
        }
    });

    Ok(receiver)
}

async fn callback_handler(
    State(state): State<CallbackState>,
    Query(callback): Query<AuthCallback>,
) -> Html<&'static str> {
    let failed = callback.error.is_some();

    let mut sender = state.sender.lock().await;

    if let Some(sender) = sender.take() {
        let _ = sender.send(callback);
    }

    if failed {
        Html(
            r#"<html><body style="font-family:system-ui;padding:3rem">
                 <h1>Authorization failed</h1>
                 <p>Check the terminal for details.</p>
               </body></html>"#,
        )
    } else {
        Html(
            r#"<html><body style="font-family:system-ui;padding:3rem">
                 <h1>Bank connected</h1>
                 <p>You can close this window.</p>
               </body></html>"#,
        )
    }
}
