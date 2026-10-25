use dotenvy::dotenv;
use rand::seq::SliceRandom;
use rand::rng;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use serde_json::ser::State::Empty;
use serenity::all::FullEvent;
use serenity::all::standard::Configuration;
use serenity::client::Context;
use serenity::framework::standard::CommandError;
use serenity::model::channel::ReactionType::Unicode;
use serenity::model::guild::Guild;
use serenity::model::id::{ChannelId, UserId};
use serenity::model::prelude::{GuildId, VoiceState};
use serenity::prelude::{TypeMap, TypeMapKey};
use serenity::{
    Result as SerenityResult, async_trait,
    client::{Client, EventHandler},
    framework::{
        StandardFramework,
        standard::{
            Args, CommandResult,
            macros::{command, group},
        },
    },
    model::{channel::Message, gateway::Ready},
    prelude::GatewayIntents,
};
use songbird::TrackEvent::End;
use songbird::input::{Compose, YoutubeDl};
use songbird::tracks::TrackHandle;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit};
use std::cmp::min;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use tokio::sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use tracing::info;

use crate::models::{DubaBot, ServerData, Song};
use crate::playlists::songs_list_from_playlist_url;

mod models;
mod playlists;

const UNKNOWN_TRACK_TITLE: &str = "UNKNOWN TRACK";

type Error = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() {
    dotenv().expect(".env file not found");

    tracing_subscriber::fmt::init();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let server = Arc::new(RwLock::new(DubaBot {
        servers: HashMap::new(),
    }));

    let framework = poise::Framework::builder()
        .setup(move |_ctx, _ready, _framework| Box::pin(async move { Ok(server) }))
        .options(poise::FrameworkOptions {
            event_handler: |ctx, event| {
                Box::pin(event_handler(ctx.serenity_context, event, ctx.user_data))
            },
            ..Default::default()
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = serenity::Client::builder(token, intents)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Err creating client");

    tokio::spawn(async move {
        let _ = client
            .start()
            .await
            .map_err(|why| info!("Client ended: {why:?}"));
    });

    tokio::signal::ctrl_c()
        .await
        .expect("Control-C interruption failed!");

    info!("Received Ctrl-C, shutting down.");
}

async fn event_handler(
    ctx: &Context,
    event: &FullEvent,
    data: &Arc<RwLock<DubaBot>>,
) -> Result<(), Error> {
    match event {
        FullEvent::Ready { data_about_bot, .. } => {
            println!("Logged in as {}", data_about_bot.user.name);
        }
        FullEvent::Message { new_message } => {
            info!(
                "Received message {} with user ID {}",
                new_message.content, new_message.author.id
            );
            if (new_message.author.id != ctx.cache.current_user().id) {
                // Ignore messages from the bot itself
                message_input(ctx, data, new_message).await?;
            }
        }
        FullEvent::VoiceStateUpdate { old, new } => {
            if new.channel_id.is_none() {
                user_disconnected(ctx, data, old, new).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn user_disconnected(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    old: &Option<VoiceState>,
    new: &VoiceState,
) -> Result<(), Error> {
    if let Some(guild_id) = new.guild_id {
        if ctx.cache.current_user().id == new.user_id.get() {
            info!("Bot ID matches disconnected user");
            stop_and_clear_queue(ctx, data, &guild_id).await;
        } else {
            info!("Bot ID does not match disconnected user");
            if let Some(channel_id) = old.as_ref().and_then(|v| v.channel_id)
                && is_channel_empty(ctx, guild_id, channel_id).await?
            {
                stop_and_clear_queue(ctx, data, &guild_id).await;
                let _ = leave(ctx, &guild_id, &channel_id, false).await;
            }
        }
    }
    Ok(())
}

async fn stop_and_clear_queue(ctx: &Context, data: &Arc<RwLock<DubaBot>>, guild_id: &GuildId) {
    let mut duba_bot = data.write().await;

    if let Err(error) = clear_queue(&mut duba_bot, &guild_id).await {
        info!("{:#?}", error)
    }

    if let Err(error) = stop_current_track(ctx, &mut duba_bot, &guild_id, None).await {
        info!("{:#?}", error)
    }
}

async fn is_channel_empty(
    ctx: &Context,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Result<bool, Error> {
    let members = ctx
        .http
        .as_ref()
        .get_guild_members(guild_id, None,None)
        .await?;

    Ok(members.is_empty())
}

async fn message_input(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    new_message: &Message,
) -> Result<(), Error> {
    let message = new_message.content.clone();
    let guild_id = new_message.guild_id;

    match guild_id {
        None => {
            info!(
                "No Guild ID given for message {} from author {}",
                new_message.content, new_message.author.name
            );
            return Ok(());
        }
        Some(guild_id) => {
            let play_prefix = "!play ";
            if message.to_lowercase().starts_with(play_prefix) {
                play(ctx, data, new_message, &message[play_prefix.len()..]).await;
            } else if message == "!ping" {
                // Sending a message can fail, due to a network error, an
                // authentication error, or lack of permissions to post in the
                // channel, so log to stdout when some error happens, with a
                // description of it.
                if let Err(why) = new_message.channel_id.say(&ctx.http, "Pong!").await {
                    info!("Error sending message: {why:?}");
                }
            } else if message == "!pause" {
                pause(ctx, data, new_message).await?;
            } else if message == "!unpause" {
                unpause(ctx, data, new_message, &guild_id).await?;
            } else if message == "!stop" {
                stop(ctx, data, new_message, &guild_id).await?;
            } else if message == "!next" {
                next(ctx, data, new_message, &guild_id).await?;
            } else if message == "!queue" {
                queue(ctx, data, new_message, &guild_id).await?;
            } else if message == "!shuffle" {
                shuffle(ctx, data, new_message, &guild_id).await?;
            } else if message == "!help" {
                help(ctx, new_message).await?;
            }
        }
    }

    Ok(())
}

async fn play(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    message: &Message,
    message_content: &str,
) {
    join(ctx, message).await.expect("Error joining");
    let loading_emoji = Unicode("⏳".to_string());

    message
        .react(&ctx.http, loading_emoji.clone())
        .await
        .expect("Could not react");

    let mut duba_bot = data.write().await;
    let songs = get_song_from_string(message_content).await;

    let user_id = ctx.cache.current_user().id;

    message
        .delete_reaction(&ctx.http, Some(user_id), loading_emoji.clone())
        .await
        .expect("Could not delete emoji");

    match songs {
        Ok(songs) => {
            for song in songs {
                let _ = push_song_to_guild(
                    &message.guild_id.unwrap(),
                    song,
                    true,
                    &mut duba_bot.servers,
                )
                .await;
            }

            let song_notifier = SongEndNotifier {
                guild_id: message.guild_id.unwrap(),
                channel_id: message.channel_id,
                duba_bot: data.clone(),
                ctx: ctx.clone(),
            };

            play_next_if_queue_empty(
                ctx,
                &mut duba_bot,
                &message.guild_id.unwrap(),
                &message.channel_id,
                song_notifier,
            )
            .await;

            message
                .react(&ctx.http, Unicode("👍".to_string()))
                .await
                .expect("Could not delete emoji");
        }
        Err(_) => {
            message
                .react(&ctx.http, Unicode("💀".to_string()))
                .await
                .expect("Could not delete emoji");
        }
    }
}

async fn pause(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    new_message: &Message,
) -> Result<(), Error> {
    let guild_id = get_guild_id(ctx, new_message)?;
    let data = data.read().await;

    match get_track_handle(&data, &guild_id).await {
        Some(track_handle) => track_handle.pause()?,
        None => check_msg(
            new_message
                .channel_id
                .say(&ctx.http, "o_O Already stopped")
                .await,
        ),
    }
    Ok(())
}

async fn stop(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    new_message: &Message,
    guild_id: &GuildId,
) -> Result<(), Error> {
    let mut duba_bot = data.write().await;

    clear_queue(&mut duba_bot, &guild_id).await?;
    stop_current_track(ctx, &mut duba_bot, &guild_id, Some(&new_message.channel_id)).await?;
    leave(ctx, guild_id, &new_message.channel_id, true).await?;
    Ok(())
}

async fn queue(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    new_message: &Message,
    guild_id: &GuildId,
) -> Result<(), Error> {
    let duba_bot = data.read().await;

    let default: VecDeque<Song> = VecDeque::new();
    let songs = get_songs_from_guild(&duba_bot, &guild_id)
        .await
        .unwrap_or(&default);

    if songs.is_empty() {
        check_msg(
            new_message
                .channel_id
                .say(&ctx.http, "The queue is empty!")
                .await,
        );
    } else {
        let max_songs = 20;
        let mut songs_titles: Vec<String> = Vec::with_capacity(min(songs.len(), max_songs));

        for (index, song) in songs.iter().take(max_songs).enumerate() {
            let song_index = index + 1;
            songs_titles.push(format!("{song_index} - {}", song.title));
        }

        let songs_formatted = songs_titles.join("\n");

        check_msg(
            new_message
                .channel_id
                .say(&ctx.http, format!("**Queue**:\n```{songs_formatted}```"))
                .await,
        );
    }
    Ok(())
}

async fn help(ctx: &Context, msg: &Message) -> CommandResult {
    let message = r#"
**Commands:**
- **play [URL|Title]** - Reprodueix (o afegeix a la cua) una nova pista donat un enllaç de YouTube o el títol d'un video (permet llistes de reproducció).
- **pause** - Pausa la pista actual.
- **unpause** - Reprendre la pista actualment pausada.
- **stop** - Atura la pista actual i neteja la cua.
- **pn [URL|Title]** - Adds track to the top of the queue to be played next.
- **next** - Reprodueix la següent pista.
- **queue** - Mostra la cua de pistes.
- **goto [INDEX]** - Reprodueix immediatament una pista específica de la cua (descarta pistes prèvies a la indicada).
- **shuffle** - Reordena la cua a l'atzar.
    "#;

    check_msg(msg.channel_id.say(&ctx.http, message).await);

    Ok(())
}

async fn get_song_from_string(data: &str) -> Result<Vec<Song>, CommandError> {
    if data.starts_with("http") && data.contains("&list=") || data.contains("?list=") {
        info!("Detected playlist in {data}");

        songs_list_from_playlist_url(data)
    } else {
        let client = HttpClient::new();
        let mut input = if data.starts_with("http") {
            YoutubeDl::new(client, data)
        } else {
            YoutubeDl::new_search(client, data)
        };

        let metadata = input.aux_metadata().await.unwrap();

        let source_url = metadata.source_url.ok_or(CommandError::from(format!(
            "Could not load song for input {data}"
        )))?;

        let song_name = metadata.title.unwrap_or(UNKNOWN_TRACK_TITLE.to_string());
        let song_duration = metadata.duration;

        let song = Song {
            title: song_name,
            url: source_url,
            duration: song_duration,
        };

        Ok(vec![song])
    }
}

async fn play_next_if_queue_empty(
    ctx: &Context,
    duba_bot: &mut DubaBot,
    guild_id: &GuildId,
    channel_id: &ChannelId,
    song_end_notifier: SongEndNotifier,
) {
    info!("play_next_if_queue_empty start");

    let default: VecDeque<Song> = VecDeque::new();
    let songs = get_songs_from_guild(duba_bot, guild_id)
        .await
        .unwrap_or(&default);

    let track_handle: Option<&TrackHandle> = get_track_handle2(duba_bot, guild_id).await;
    let is_not_playing = track_handle.is_none();
    let queue_is_empty = songs.is_empty();

    if !queue_is_empty && is_not_playing {
        while play_next_song(
            ctx,
            guild_id,
            channel_id,
            duba_bot,
            song_end_notifier.clone(),
        )
        .await
        .is_err()
        {
            info!("Next song failed")
        }
    }

    info!("play_next_if_queue_empty end");
}

async fn clear_queue(duba_bot: &mut DubaBot, guild_id: &GuildId) -> CommandResult {
    if let Some(server) = duba_bot.servers.get_mut(&guild_id.get()) {
        server.queue.clear();
    }

    Ok(())
}

async fn unpause(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    msg: &Message,
    guild_id: &GuildId,
) -> CommandResult {
    let data = data.write().await;
    match get_track_handle(&data, &msg.guild_id.unwrap()).await {
        Some(track_handle) => track_handle.play()?,
        None => {
            check_msg(msg.channel_id.say(&ctx.http, "o_O Already stopped").await);
        }
    }

    Ok(())
}

async fn next(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    msg: &Message,
    guild_id: &GuildId,
) -> CommandResult {
    info!("NEXT - Next command invoked from guild {}!", guild_id.get());

    let mut duba_bot = data.write().await;

    let default_songs = VecDeque::new();
    let songs_queue = get_songs_from_guild(&duba_bot, &guild_id)
        .await
        .unwrap_or(&default_songs);

    info!("NEXT - There are {} songs in the queue!", songs_queue.len());

    let is_queue_empty = songs_queue.is_empty();

    if !is_queue_empty {
        info!("NEXT - Stopping current song");
        // Stopping the current song will automatically start the next one
        stop_current_track(ctx, &mut duba_bot, &guild_id, Some(&msg.channel_id)).await?;
    }

    Ok(())
}

async fn shuffle(
    ctx: &Context,
    data: &Arc<RwLock<DubaBot>>,
    msg: &Message,
    guild_id: &GuildId,
) -> CommandResult {
    let mut duba_bot = data.write().await;

    info!(
        "Shuffle - Next command invoked from guild {}!",
        guild_id.get()
    );

    let server = duba_bot
        .servers
        .get_mut(&msg.guild_id.unwrap().get())
        .expect("No server");

    let songs = &mut server.queue;
    songs.make_contiguous().shuffle(&mut rng());

    msg.react(&ctx.http, Unicode("👍".to_string())).await?;

    Ok(())
}

// #[command]
// #[only_in(guilds)]
// async fn goto(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
//     let guild_id = get_guild_id(ctx, msg)?;
//
//     let index = match args.single::<usize>() {
//         Ok(url) => url,
//         Err(_) => {
//             check_msg(
//                 msg.channel_id
//                     .say(
//                         &ctx.http,
//                         "Invalid song index. Check the queue to list the songs.",
//                     )
//                     .await,
//             );
//
//             return Ok(());
//         }
//     };
//
//     info!("goto - Next command invoked from guild {}!", guild_id.get());
//     let is_valid_index: bool;
//
//     {
//         let data = &mut ctx.data.write().await;
//
//         let server = get_server_mut(data, &guild_id)?;
//         let songs = &mut server.queue;
//
//         is_valid_index = index < songs.len();
//
//         if is_valid_index {
//             for _ in 1..index {
//                 songs.pop_front();
//             }
//         }
//     }
//
//     if is_valid_index {
//         stop_current_track(ctx, &guild_id, Some(&msg.channel_id)).await?;
//     }
//
//     Ok(())
// }

async fn stop_current_track(
    ctx: &Context,
    duba_bot: &mut DubaBot,
    guild_id: &GuildId,
    channel_id: Option<&ChannelId>,
) -> CommandResult {
    match get_track_handle(duba_bot, guild_id).await {
        Some(track_handle) => track_handle.stop()?,
        None => {
            let error_message = "o_O Already stopped";
            if let Some(channel) = channel_id {
                check_msg(channel.say(&ctx.http, error_message).await);
            } else {
                return Err(CommandError::from(error_message));
            }
        }
    }

    remove_track_handle(duba_bot, &guild_id.get()).await?;

    Ok(())
}

async fn leave(ctx: &Context, guild_id: &GuildId, channel_id: &ChannelId, notify: bool) -> CommandResult {
    leave_current_channel(ctx, guild_id, channel_id, notify).await
}

async fn leave_current_channel(ctx: &Context, guild_id: &GuildId, channel_id: &ChannelId, notify: bool) -> CommandResult {
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let has_handler = manager.get(guild_id.clone()).is_some();

    if has_handler {
        if let Err(e) = manager.remove(guild_id.clone()).await {
            if notify {
                check_msg(
                    channel_id
                        .say(&ctx.http, format!("Failed: {e:?}"))
                        .await,
                );
            }
        }

        if notify {
            check_msg(channel_id.say(&ctx.http, "Left voice channel").await);
        }

    }

    Ok(())
}

async fn play_next_song(
    ctx: &Context,
    guild_id: &GuildId,
    channel_id: &ChannelId,
    duba_bot: &mut DubaBot,
    song_end_notifier: SongEndNotifier,
) -> Result<(), CommandError> {
    if let Some(song) = get_next_song(duba_bot, guild_id).await {
        info!(
            "PLAY_NEXT_SONG - Next song is {} - {}",
            song.title, song.url
        );

        let manager = songbird::get(ctx)
            .await
            .expect("Songbird Voice client placed in at initialisation.")
            .clone();

        if let Some(handler_lock) = manager.get(*guild_id) {
            let mut handler = handler_lock.lock().await;
            let http_client = HttpClient::new();

            let source = YoutubeDl::new(http_client, song.url.clone());

            handler.stop(); // Just in case something was playing before
            let track_handle = handler.play(source.into());

            track_handle
                .add_event(Event::Track(End), song_end_notifier)
                .expect("Add event END failed");

            set_new_track_handle(track_handle, guild_id, duba_bot).await?;

            check_msg(
                channel_id
                    .say(
                        &ctx.http,
                        format!("Playing song [{}]({})", song.title, song.url),
                    )
                    .await,
            );
        } else {
            check_msg(
                channel_id
                    .say(&ctx.http, "Not in a voice channel to play in")
                    .await,
            );
        }
    }

    Ok(())
}

async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let channel_id = get_guild(ctx, msg)
        .voice_states
        .get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let connect_to = match channel_id {
        Some(channel) => channel,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Err(CommandError::from("Not in a voice channel"));
        }
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let _handler = manager.join(guild_id, connect_to).await;

    Ok(())
}

async fn deafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let handler_lock = match manager.get(guild_id) {
        Some(handler) => handler,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Ok(());
        }
    };

    let mut handler = handler_lock.lock().await;

    if handler.is_deaf() {
        info!("Already deafen!")
    } else if let Err(e) = handler.deafen(true).await {
        info!("Deafen failed due to {e:?}")
    }

    Ok(())
}

async fn set_new_track_handle(
    track_handle: TrackHandle,
    guild_id: &GuildId,
    duba_bot: &mut DubaBot,
) -> Result<(), CommandError> {
    let server = duba_bot.servers.get_mut(&guild_id.get()).unwrap();

    server.track_handle = Some(track_handle);

    Ok(())
}

async fn remove_track_handle(duba_bot: &mut DubaBot, guild_id: &u64) -> Result<(), CommandError> {
    if let Some(server) = duba_bot.servers.get_mut(guild_id) {
        server.track_handle = None;
    }

    Ok(())
}

async fn get_track_handle<'a>(data: &'a DubaBot, guild_id: &GuildId) -> Option<&'a TrackHandle> {
    let guilds = &data.servers;
    let guild = guilds.get(&guild_id.get())?;

    guild.track_handle.as_ref()
}

async fn get_track_handle2<'a>(data: &'a DubaBot, guild_id: &GuildId) -> Option<&'a TrackHandle> {
    let guilds = &data.servers;
    let guild = guilds.get(&guild_id.get())?;

    guild.track_handle.as_ref()
}

async fn get_songs_from_guild<'a>(
    data: &'a DubaBot,
    guild_id: &GuildId,
) -> Option<&'a VecDeque<Song>> {
    let guilds = &data.servers;

    if let Some(guild) = guilds.get(&guild_id.get()) {
        let count = guild.queue.len();

        info!("There are {count} songs:");

        Some(&guild.queue)
    } else {
        None
    }
}

async fn get_next_song(duba_bot: &mut DubaBot, guild_id: &GuildId) -> Option<Song> {
    let server = duba_bot.servers.get_mut(&guild_id.get())?;
    let song = server.queue.pop_front();

    match &song {
        None => info!("GET_NEXT_SONG - Queue is empty"),
        Some(song) => info!("GET_NEXT_SONG - Next song is {} - {}", song.title, song.url),
    };

    song
}

async fn push_song_to_guild(
    guild_id: &GuildId,
    song: Song,
    insert_last: bool,
    guilds: &mut HashMap<u64, ServerData>,
) -> Result<(), CommandError> {
    let guild = guilds.get_mut(&guild_id.get());

    match guild {
        Some(data) => {
            if insert_last {
                data.queue.push_back(song);
            } else {
                data.queue.push_front(song);
            }
        }
        None => {
            let new_guild_data = ServerData {
                track_handle: None,
                queue: VecDeque::from([song]),
            };

            guilds.insert(guild_id.get(), new_guild_data);
        }
    };

    Ok(())
}

/// Checks that a message successfully sent; if not, then logs why to stdout.
fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        info!("Error sending message: {why:?}");
    }
}

fn get_guild(ctx: &Context, msg: &Message) -> Guild {
    msg.guild(&ctx.cache).expect("Guild not found").clone()
}

fn get_guild_id(ctx: &Context, msg: &Message) -> CommandResult<GuildId> {
    let guild_id = get_guild(ctx, msg).id;

    Ok(guild_id)
}

#[derive(Clone)]
struct SongEndNotifier {
    guild_id: GuildId,
    channel_id: ChannelId,
    duba_bot: Arc<RwLock<DubaBot>>,
    ctx: Context,
}

#[async_trait]
impl VoiceEventHandler for SongEndNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        info!("End notifier triggered");

        let mut duba_bot = self.duba_bot.write().await;

        match remove_track_handle(&mut duba_bot, &self.guild_id.get()).await {
            Ok(_) => {
                // If playing next song fails, try with the another one until it works
                while (play_next_song(
                    &self.ctx,
                    &self.guild_id,
                    &self.channel_id,
                    &mut duba_bot,
                    self.clone(),
                )
                .await)
                    .is_err()
                {}
            }
            Err(_) => {
                info!("Remove track failed")
            }
        }

        None
    }
}
