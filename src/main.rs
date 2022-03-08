use std::env;
use std::sync::Arc;
use std::time::Duration;
use serenity::{
    client::bridge::gateway::GatewayIntents,
    async_trait,
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use serenity::model::interactions::application_command::ApplicationCommandOptionType;
use serenity::model::interactions::{Interaction, InteractionResponseType};
use serenity::model::prelude::application_command::{ApplicationCommand};

use tokio::time;

use serde::Deserialize;
use serenity::http::CacheHttp;
use serenity::model::id::RoleId;
use serenity::model::prelude::{ChannelId, InteractionApplicationCommandCallbackDataFlags};

#[derive(Deserialize, Clone)]
struct ContestDay {
    day: u8,
    species: u8,
    version: String,
    hints: Vec<String>,
}

impl ContestDay {
    pub fn hints_to_fields(&self) -> Vec<(String, String, bool)> {
        self.hints.iter().enumerate().map(|(i, s)| ("Hint ".to_owned() + (i + 1).to_string().as_str(), s.to_owned(), true)).collect::<Vec<(String, String, bool)>>()
    }
}

#[derive(Deserialize, Clone)]
struct ContestDetails(Vec<ContestDay>);

impl ContestDetails {
    pub fn get_day(&self, day: u8) -> Option<&ContestDay> {
        self.0.iter().find(|d| d.day == day)
    }

    pub fn get_last_day(&self) -> Option<u8> {
        self.0.iter().map(|d| d.day).max()
    }
}

struct Contest {
    current_day: Option<u8>,
    details: ContestDetails,
}

struct Handler {
    awaiting_details: Arc<Mutex<bool>>,
    register_commands: Arc<Mutex<bool>>,
    contest: Arc<Mutex<Option<Contest>>>,
    permission_role: u64,
    contest_channel: u64,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, message: Message) {
        if !message.attachments.is_empty() {
            let awaiting_details = self.awaiting_details.lock().await;
            if *awaiting_details && message.author.has_role(&ctx.http(), message.guild_id.unwrap(), RoleId(self.permission_role)).await.expect("Failed to retrieve role") {
                    let attachments = message.attachments.get(0).unwrap();
                    let mut contest_guard = self.contest.lock().await;
                    if let Some(t) = attachments.content_type.as_ref() {
                        if t == "application/json; charset=utf-8" {
                            if let Ok(data) = attachments.download().await {
                                if let Ok(details) = serde_json::from_slice(data.as_slice()) {
                                    if let Some(contest) = contest_guard.as_mut() {
                                        contest.details = details;
                                        if let Err(why) = message.channel_id.send_message(&ctx.http, |m| {
                                            m.content("Contest details loaded!")
                                        }).await {
                                            println!("An error occurred confirming contest details: {:?}", why);
                                        }
                                        contest.current_day = Some(0);

                                        let contest_clone = Arc::clone(&self.contest);
                                        let ctx = Arc::new(ctx);
                                        let ctx_clone = Arc::clone(&ctx);
                                        let channel_id = self.contest_channel;
                                        tokio::spawn(async move {
                                            let contest = contest_clone;
                                            let ctx = ctx_clone;
                                            let channel_id = channel_id;
                                            let mut interval = time::interval(Duration::from_secs(30));
                                            loop {
                                                interval.tick().await;
                                                let mut contest = contest.lock().await;
                                                if let Some(c) = contest.as_mut() {
                                                    if let Some(d) = c.current_day.as_mut() {
                                                        *d += 1;
                                                        if let Some(contest_day) = c.details.get_day(*d) {
                                                            let broadcast = ChannelId(channel_id)
                                                                .send_message(&ctx.http, |m| {
                                                                    m.embed(|e| {
                                                                        e.title(format!("Day {}", contest_day.day)).field("Version", &contest_day.version, false);
                                                                        e.fields(contest_day.hints_to_fields())
                                                                    })
                                                                }).await;
                                                            if let Err(why) = broadcast {
                                                                println!("Error sending message: {:?}", why);
                                                            }
                                                        } else if let Some(last_day) = c.details.get_last_day() {
                                                            if *d > last_day {
                                                                *contest = None;
                                                                let broadcast = ChannelId(channel_id)
                                                                    .send_message(&ctx.http, |m| {
                                                                        m.content("The current contest has ended!")
                                                                    }).await;
                                                                if let Err(why) = broadcast {
                                                                    println!("Error sending message: {:?}", why);
                                                                }
                                                                break;
                                                            }
                                                        }
                                                    }
                                                } else {
                                                    break;
                                                }
                                            }
                                        });
                                    }
                                } else if let Err(why) = message.channel_id.send_message(&ctx.http, |m| {
                                    m.content("Failed to load contest details. Please restart the process with /contest start")
                                }).await {
                                    println!("An error occurred confirming contest details: {:?}", why);
                                }
                            } else {
                                if let Err(why) = message.channel_id.send_message(&ctx.http, |m| {
                                    m.content("Failed to download attachment. Please restart the process with /contest start")
                                }).await {
                                    println!("An error occurred confirming contest details: {:?}", why);
                                }
                                *contest_guard = None;
                            }
                        }
                    }
                }

        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        let register_commands = self.register_commands.lock().await;
        if *register_commands {
            let command = ApplicationCommand::create_global_application_command(&ctx.http, |c| {
                c.name("contest").description("Base command for contest bot")
                    .create_option(|o| {
                        o.name("start").description("Start a contest with a given json").kind(ApplicationCommandOptionType::SubCommand)
                    })
                    .create_option(|o| {
                        o.name("stop").description("Stop the current contest").kind(ApplicationCommandOptionType::SubCommand)
                    })
            }).await;
            println!("Created the following application command: {:#?}", command);
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::ApplicationCommand(command) = interaction {
            if command.data.name.as_str() == "contest" && command.user.has_role(&ctx.http(), command.guild_id.unwrap(), RoleId(self.permission_role)).await.expect("Failed to retrieve role") {
                let option = command.data.options.get(0).expect("Expected sub command option");
                if let ApplicationCommandOptionType::SubCommand = option.kind {
                    match option.name.as_str() {
                        "start" => {
                            let mut contest_guard = self.contest.lock().await;
                            if contest_guard.as_ref().is_none() {
                                *contest_guard = Some(Contest {
                                    current_day: None,
                                    details: ContestDetails(Vec::new()),
                                });
                                if let Err(why) = command
                                    .create_interaction_response(&ctx.http, |r| {
                                        r.kind(InteractionResponseType::ChannelMessageWithSource)
                                            .interaction_response_data(|m| m.flags(InteractionApplicationCommandCallbackDataFlags::EPHEMERAL).content("Awaiting json with giveaway details."))
                                    }).await {
                                    println!("Cannot respond to slash command: {}", why);
                                }
                                let mut awaiting_details = self.awaiting_details.lock().await;
                                *awaiting_details = true;
                            }
                        }
                        "stop" => {
                            let mut contest_guard = self.contest.lock().await;
                            if contest_guard.as_ref().is_some() {
                                *contest_guard = None;
                            }
                            if let Err(why) = command
                                .create_interaction_response(&ctx.http, |r| {
                                    r.kind(InteractionResponseType::ChannelMessageWithSource)
                                        .interaction_response_data(|m| m.flags(InteractionApplicationCommandCallbackDataFlags::EPHEMERAL).content("The giveaway has been stopped."))
                                }).await {
                                println!("Cannot respond to slash command: {}", why);
                            }
                            let mut awaiting_details = self.awaiting_details.lock().await;
                            *awaiting_details = false;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
    let register_slash_commands = env::var("REGISTER_COMMANDS").unwrap_or_else(|_| "false".to_owned()).parse::<bool>().unwrap_or(false);
    let application_id: u64 = env::var("APPLICATION_ID")
        .expect("Expected an application id in the environment")
        .parse()
        .expect("application id is not a valid id");
    let permission_role: u64 = env::var("PERMISSION_ROLE")
        .expect("Expected an role id in the environment")
        .parse()
        .expect("role id is not a valid id");
    let contest_channel: u64 = env::var("CONTEST_CHANNEL")
        .expect("Expected an channel id in the environment")
        .parse()
        .expect("channel id is not a valid id");

    let handler = Handler {
        awaiting_details: Arc::new(Mutex::new(false)),
        register_commands: Arc::new(Mutex::new(register_slash_commands)),
        contest: Arc::new(Mutex::new(Option::None)),
        permission_role,
        contest_channel,
    };

    let mut client = Client::builder(token)
        .event_handler(handler)
        .application_id(application_id)
        .intents(GatewayIntents::GUILD_MESSAGES)
        .await
        .expect("Error creating client");

    if let Err(why) = client.start().await {
        println!("An error occurred while running the client: {:?}", why);
    }
}

