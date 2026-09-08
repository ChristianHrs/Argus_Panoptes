use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use serde::Deserialize;
use tokio::sync::{
    oneshot,
    Mutex,
};

#[derive(Debug, Deserialize)]
pub struct AuthCallback {
    pub code: Option<String>,
    pub state: Option<String>,

    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Clone)]
struct CallbackState {
    sender: Arc<
        Mutex<
            Option<
                oneshot::Sender<AuthCallback>
            >
        >
    >,
}

pub async fn start_callback_server(
) -> Result<oneshot::Receiver<AuthCallback>> {
    let (sender, receiver) =
        oneshot::channel::<AuthCallback>();

    let state = CallbackState {
        sender: Arc::new(
            Mutex::new(Some(sender))
        ),
    };

    let app = Router::new()
        .route(
            "/callback",
            get(callback_handler),
        )
        .with_state(state);

    let listener =
        tokio::net::TcpListener::bind(
            "127.0.0.1:3000"
        )
        .await?;

    println!(
        "Callback server listening on \
         http://localhost:3000/callback"
    );

    tokio::spawn(async move {
        if let Err(error) =
            axum::serve(listener, app).await
        {
            eprintln!(
                "Callback server error: {error}"
            );
        }
    });

    Ok(receiver)
}

async fn callback_handler(
    State(state): State<CallbackState>,
    Query(callback): Query<AuthCallback>,
) -> Html<&'static str> {
    let mut sender =
        state.sender.lock().await;

    if let Some(sender) = sender.take() {
        let _ = sender.send(callback);
    }

    Html(
        r#"
        <html>
            <body>
                <h1>Bank connected</h1>
                <p>
                    Authorization was received.
                    You can close this window.
                </p>
            </body>
        </html>
        "#,
    )
}
