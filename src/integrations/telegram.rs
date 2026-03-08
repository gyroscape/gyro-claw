//! # Telegram Bot Integration
//!
//! Provides a secure interface to control Gyro-Claw via Telegram.
//! Supports queuing background tasks and checking status remotely.

use anyhow::Result;
use std::env;
use std::io::Write;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

fn prompt_input(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn append_to_env(key: &str, value: &str) -> Result<()> {
    use std::fs::OpenOptions;
    let mut file = OpenOptions::new().create(true).append(true).open(".env")?;
    writeln!(file, "{}={}", key, value)?;
    Ok(())
}

use crate::agent::memory::Memory;

#[derive(BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "These commands are supported:"
)]
enum Command {
    #[command(description = "See bot instructions")]
    Start,
    #[command(description = "See bot instructions")]
    Help,
    #[command(description = "Queue a goal for the background worker")]
    Run(String),
    #[command(description = "List recent background tasks")]
    Tasks,
    #[command(description = "Cancel a running task by ID")]
    Stop(String),
    #[command(description = "Check system status")]
    Status,
}

pub struct TelegramIntegration {
    memory: Memory,
    allowed_user_id: Option<u64>,
}

impl TelegramIntegration {
    pub fn new(memory: Memory) -> Self {
        let allowed_user_id = env::var("TELEGRAM_ALLOWED_USER_ID")
            .ok()
            .and_then(|id| id.parse::<u64>().ok());

        Self {
            memory,
            allowed_user_id,
        }
    }

    /// Start the bot listener loops. This call blocks indefinitely (or until cancelled).
    pub async fn start(&self) -> Result<()> {
        let mut token = env::var("TELOXIDE_TOKEN").unwrap_or_default();
        if token.is_empty() {
            println!("⚠️ TELOXIDE_TOKEN not set.");
            token = prompt_input("Enter your Telegram Bot Token (or press Enter to skip): ")?;
            if token.is_empty() {
                println!("Skipping Telegram integration.");
                return Ok(());
            }
            append_to_env("TELOXIDE_TOKEN", &token)
                .unwrap_or_else(|e| println!("Failed to save to .env: {}", e));
        }

        println!("🤖 Starting Telegram Bot Integration...");

        let mut allowed_user_id = self.allowed_user_id;
        if allowed_user_id.is_none() {
            println!("⚠️ Warning: TELEGRAM_ALLOWED_USER_ID is not set. Anyone interacting with the bot will be rejected for security.");
            let input =
                prompt_input("Enter your numeric Telegram User ID (leave blank to dismiss): ")?;
            if !input.is_empty() {
                if let Ok(uid) = input.parse::<u64>() {
                    allowed_user_id = Some(uid);
                    append_to_env("TELEGRAM_ALLOWED_USER_ID", &input)
                        .unwrap_or_else(|e| println!("Failed to save to .env: {}", e));
                    println!("✅ TELEGRAM_ALLOWED_USER_ID saved.");
                } else {
                    println!("❌ Invalid User ID. Must be a number.");
                }
            }
        }

        let bot = Bot::new(token);

        let handler = dptree::entry()
            .branch(Update::filter_message()
                // Security filter: block messages from unallowed users
                .filter(|msg: Message, md: std::sync::Arc<TelegramIntegration>| {
                    if let Some(user) = &msg.from {
                        let uid = user.id.0;
                        if let Some(allowed) = md.allowed_user_id {
                            if uid == allowed {
                                return true;
                            } else {
                                println!("⚠️ Blocked Telegram command from unauthorized UID: {}", uid);
                                println!("   If this is you, update TELEGRAM_ALLOWED_USER_ID in .env to {} and restart.", uid);
                                return false;
                            }
                        } else {
                            println!("⚠️ Blocked Telegram command. TELEGRAM_ALLOWED_USER_ID is strictly enforced but missing from .env.");
                            return false;
                        }
                    }
                    false
                })
                .filter_command::<Command>()
                .endpoint(Self::handle_command)
            );

        let mut md = self.clone();
        md.allowed_user_id = allowed_user_id;

        Dispatcher::builder(bot, handler)
            .dependencies(dptree::deps![std::sync::Arc::new(md)])
            .enable_ctrlc_handler()
            .build()
            .dispatch()
            .await;

        Ok(())
    }

    async fn handle_command(
        bot: Bot,
        msg: Message,
        cmd: Command,
        md: std::sync::Arc<TelegramIntegration>,
    ) -> ResponseResult<()> {
        match cmd {
            Command::Start | Command::Help => {
                bot.send_message(msg.chat.id, Command::descriptions().to_string())
                    .await?;
            }
            Command::Status => {
                bot.send_message(msg.chat.id, "🟢 Gyro-Claw is online and running.")
                    .await?;
            }
            Command::Run(goal) => {
                if goal.is_empty() {
                    bot.send_message(msg.chat.id, "Usage: /run <goal to accomplish>")
                        .await?;
                    return Ok(());
                }

                match md.memory.queue_background_task(&goal) {
                    Ok(id) => {
                        bot.send_message(msg.chat.id, format!("✅ Task queued with ID: #{}\nIt will be picked up by the background worker soon.", id)).await?;
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("❌ Failed to queue task: {}", e))
                            .await?;
                    }
                }
            }
            Command::Tasks => match md.memory.list_background_tasks() {
                Ok(tasks) => {
                    if tasks.is_empty() {
                        bot.send_message(msg.chat.id, "📋 No background tasks found.")
                            .await?;
                    } else {
                        let mut response = String::from("📋 **Recent Tasks:**\n");
                        for (id, goal, status, created) in tasks.into_iter().take(10) {
                            let goal_disp = if goal.len() > 30 {
                                format!("{}...", &goal[..27])
                            } else {
                                goal
                            };
                            response.push_str(&format!(
                                "#{} | {} | {}\n{}\n\n",
                                id, status, created, goal_disp
                            ));
                        }
                        bot.send_message(msg.chat.id, response).await?;
                    }
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ Failed to fetch tasks: {}", e))
                        .await?;
                }
            },
            Command::Stop(id_str) => {
                if let Ok(id) = id_str.parse::<i64>() {
                    match md.memory.cancel_background_task(id) {
                        Ok(true) => {
                            bot.send_message(msg.chat.id, format!("✅ Task #{} cancelled.", id))
                                .await?;
                        }
                        Ok(false) => {
                            bot.send_message(msg.chat.id, format!("⚠️ Task #{} could not be cancelled. It may be finished or invalid.", id)).await?;
                        }
                        Err(e) => {
                            bot.send_message(
                                msg.chat.id,
                                format!("❌ Failed to cancel task: {}", e),
                            )
                            .await?;
                        }
                    }
                } else {
                    bot.send_message(msg.chat.id, "Usage: /stop <task_id>")
                        .await?;
                }
            }
        };
        Ok(())
    }
}

// Clone implementation needed for dptree state injection
impl Clone for TelegramIntegration {
    fn clone(&self) -> Self {
        Self {
            memory: self.memory.clone(),
            allowed_user_id: self.allowed_user_id,
        }
    }
}
