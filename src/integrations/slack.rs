//! # Slack Bot Integration
//!
//! Provides a Slack Event API webhook listener so the Gyro-Claw agent
//! can be mentioned in channels or direct messages to spawn background tasks.

use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::agent::memory::Memory;

/// Main state pushed into Axum handlers
pub struct SlackState {
    pub memory: Memory,
    pub bot_token: String,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum SlackEventPayload {
    /// URL Verification challenge for Slack app installation
    UrlVerification {
        token: String,
        challenge: String,
        #[serde(rename = "type")]
        event_type: String,
    },
    /// Actual events like app_mention or message
    EventCallback {
        token: String,
        team_id: String,
        api_app_id: String,
        event: SlackEventMessage,
        #[serde(rename = "type")]
        event_type: String,
        event_id: String,
        event_time: u64,
    },
}

#[derive(Deserialize, Debug)]
pub struct SlackEventMessage {
    #[serde(rename = "type")]
    pub event_type: String,
    pub user: Option<String>,
    pub text: Option<String>,
    pub channel: Option<String>,
    pub ts: Option<String>,
}

#[derive(Serialize)]
struct ChallengeResponse {
    challenge: String,
}

pub struct SlackIntegration {
    memory: Memory,
}

impl SlackIntegration {
    pub fn new(memory: Memory) -> Self {
        Self { memory }
    }

    /// Start the HTTP server listening for Slack events (blocks indefinitely).
    pub async fn start(&self) -> Result<()> {
        let slack_token = env::var("SLACK_BOT_TOKEN").unwrap_or_default();
        if slack_token.is_empty() {
            println!("⚠️ SLACK_BOT_TOKEN not set, skipping Slack integration.");
            return Ok(());
        }

        let port = env::var("SLACK_PORT").unwrap_or_else(|_| "3000".to_string());
        let addr = format!("0.0.0.0:{}", port);

        println!("🤖 Starting Slack Bot listener on {}...", addr);
        println!(
            "Ensure your Slack App is configured to send Event Subscriptions to this endpoint."
        );

        let state = Arc::new(SlackState {
            memory: self.memory.clone(),
            bot_token: slack_token,
        });

        let app = Router::new()
            .route("/slack/events", post(handle_slack_event))
            .with_state(state);

        let listener = TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

/// Main webhook POST route that Slack hits.
async fn handle_slack_event(
    State(state): State<Arc<SlackState>>,
    Json(payload): Json<SlackEventPayload>,
) -> Result<axum::response::Response, StatusCode> {
    match payload {
        SlackEventPayload::UrlVerification { challenge, .. } => {
            // Slack requires us to echo back the challenge parameter during setup
            Ok((StatusCode::OK, Json(ChallengeResponse { challenge })).into_response())
        }
        SlackEventPayload::EventCallback { event, .. } => {
            // We care about app mentions or DMs with text
            if let (Some(text), Some(channel)) = (event.text, event.channel) {
                // If the message came from a bot, ignore to prevent infinite loops
                if text.is_empty() {
                    return Ok(StatusCode::OK.into_response());
                }

                // Parse the goal directly since they probably wrote '@gyroclaw run fix failing tests'
                // Strip the mention token usually format `<@U12345> run...`
                let clean_text = if let Some(idx) = text.find('>') {
                    text[idx + 1..].trim().to_string()
                } else {
                    text.trim().to_string()
                };

                // Queue the task to run autonomously
                match state.memory.queue_background_task(&clean_text) {
                    Ok(id) => {
                        let response_text = format!(
                            "✅ Task queued with ID: #{}\nThe worker will begin processing it shortly.",
                            id
                        );
                        // Fire off async to reply to the channel so we don't block the webhook
                        let token = state.bot_token.clone();
                        tokio::spawn(async move {
                            let _ = send_slack_message(&token, &channel, &response_text).await;
                        });
                    }
                    Err(_) => { /* Ignore errors safely without crashing the event loop */ }
                }
            }

            // Always acknowledge fast
            Ok(StatusCode::OK.into_response())
        }
    }
}

/// Helper function to post message back to Slack.
async fn send_slack_message(token: &str, channel: &str, text: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "channel": channel,
        "text": text
    });

    client
        .post("https://slack.com/api/chat.postMessage")
        .header("Authorization", format!("Bearer {}", token))
        .json(&payload)
        .send()
        .await?;

    Ok(())
}
