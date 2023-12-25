use std::cmp::min;
use std::collections::{HashMap, VecDeque};
use std::env;

use dotenvy::dotenv;
use rand::seq::SliceRandom;
use rand::thread_rng;
use serenity::{
    async_trait,
    client::{Client, EventHandler},
    framework::{
        standard::{
            Args, CommandResult,
            macros::{command, group},
        },
        StandardFramework,
    },
    model::{channel::Message, gateway::Ready},
    prelude::GatewayIntents,
    Result as SerenityResult,
};
use serenity::client::Context;
use serenity::framework::standard::CommandError;
use serenity::model::channel::ReactionType::Unicode;
use serenity::model::guild::Guild;
use serenity::model::id::{ChannelId, UserId};
use serenity::model::prelude::{GuildId, VoiceState};
use serenity::prelude::TypeMap;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit, ytdl};
use songbird::input::ytdl_search;
use songbird::TrackEvent::End;
use songbird::tracks::TrackHandle;
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};
use tracing::info;

use crate::models::{DubaServers, ServerData, Song};
use crate::playlists::songs_list_from_playlist_url;

mod playlists;
mod models;

struct Handler;

pub struct ServersManager;

impl serenity::prelude::TypeMapKey for ServersManager {
    type Value = DubaServers;
}

pub struct BotDataMap;

pub struct BotData {
    pub id: u64,
}

impl serenity::prelude::TypeMapKey for BotDataMap {
    type Value = BotData;
}

const UNKNOWN_TRACK_TITLE: &str = "UNKNOWN TRACK";

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        info!("Message received {}", msg.content);
        if msg.content == "!ping" {
            // Sending a message can fail, due to a network error, an
            // authentication error, or lack of permissions to post in the
            // channel, so log to stdout when some error happens, with a
            // description of it.

            if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
                info!("Error sending message: {why:?}");
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);

        let bot_data = BotData { id: ready.user.id.0 };
        let data = &mut ctx.data.write().await;
        data.insert::<BotDataMap>(bot_data);
    }

    async fn voice_state_update(&self, ctx: Context, _: Option<VoiceState>, new: VoiceState) {
        if new.channel_id.is_none() {
            let bot_id: Option<u64>;

            {
                let data = ctx.data.read().await;
                bot_id = data.get::<BotDataMap>().map(|data| data.id);
            }

            if let (Some(bot_id), Some(guild_id)) = (bot_id, new.guild_id) {
                if bot_id == new.user_id.0 {
                    info!("Bot ID matches disconnected user");

                    if let Err(error) = clear_queue(&ctx, &guild_id).await {
                        info!("{:#?}", error)
                    }

                    if let Err(error) = stop_current_track(&ctx, &guild_id, None).await {
                        info!("{:#?}", error)
                    }
                } else {
                    info!("Bot ID does not match disconnected user");
                }
            }
        }
    }
}

#[group]
#[commands(play, pause, unpause, next, stop, queue, shuffle, goto, pn, help)] // TODO add Shuffle and Help commands
struct General;

#[tokio::main]
async fn main() {
    dotenv().expect(".env file not found");

    tracing_subscriber::fmt::init();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN")
        .expect("Expected a token in the environment");

    let framework = StandardFramework::new()
        .configure(|c| {
            c.prefix("!")
        })
        .group(&GENERAL_GROUP);

    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Err creating client");

    {
        let mut w = client.data.write().await;

        let duba_servers = DubaServers {
            servers: HashMap::new()
        };

        w.insert::<ServersManager>(duba_servers);
    }

    tokio::spawn(async move {
        let _ = client.start().await.map_err(|why| info!("Client ended: {why:?}"));
    });

    tokio::signal::ctrl_c().await.expect("Control-C interruption failed!");

    info!("Received Ctrl-C, shutting down.");
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, args: Args) -> CommandResult {
    play_song_with_reaction(ctx, msg, args, true).await
}

#[command]
#[only_in(guilds)]
async fn pn(ctx: &Context, msg: &Message, args: Args) -> CommandResult {
    play_song_with_reaction(ctx, msg, args, false).await
}

#[command]
#[only_in(guilds)]
async fn help(ctx: &Context, msg: &Message) -> CommandResult {
    let message = r#"
**Commands:**
    **play [URL|Title]** - Plays (or adds to the queue) new tracks given a URL or a video title (supports youtube playlists).
    **pause** - Pauses the current track.
    **unpause** - Unpauses the currently paused track.
    **stop** - Stops the current song and clears the queue.
    **pn [URL|Title]** - Adds track to the top of the queue to be played next.
    **next** - Plays next track.
    **queue** - Shows the queue of tracks.
    **goto [INDEX]** - Plays immediately the specific track of the queue (discards all previous tracks).
    **shuffle** - Reorders the queue randomly.
    "#;

    check_msg(msg.channel_id.say(&ctx.http, message).await);

    Ok(())
}

async fn play_song_with_reaction(ctx: &Context, msg: &Message, args: Args, insert_last: bool) -> CommandResult {
    let bot_id: Option<u64>;

    {
        let data = ctx.data.read().await;
        bot_id = data.get::<BotDataMap>().map(|data| data.id);
    }

    let loading_emoji = Unicode("⏳".to_string());

    msg.react(&ctx.http, loading_emoji.clone()).await?;

    let play_song_result = play_song(ctx, msg, args, insert_last).await;

    msg.react(&ctx.http, loading_emoji.clone()).await?;

    if let Some(bot_id) = bot_id {
        msg.channel_id.delete_reaction(&ctx.http, msg.id, Some(UserId(bot_id)), loading_emoji.clone()).await?;
    }

    let answer_emoji = match play_song_result {
        Ok(_) => {
            "👍"
        }
        Err(_) => {
            "💀"
        }
    };

    msg.react(&ctx.http, Unicode(answer_emoji.to_string())).await?;

    Ok(())
}

async fn play_song(ctx: &Context, msg: &Message, args: Args, insert_last: bool) -> CommandResult {
    join(ctx, msg).await?;
    deafen(ctx, msg).await?;

    let user_input = args.message();

    info!("User input is {user_input}");

    let guild_id = get_guild_id(ctx, msg)?;

    if user_input.starts_with("http") && user_input.contains("&list=") || user_input.contains("?list=") {
        info!("Detected playlist in {user_input}");

        let songs = songs_list_from_playlist_url(&user_input)?;
        push_songs_list_to_server(ctx, &guild_id, songs).await?;
    } else {
        let input = if user_input.starts_with("http") {
            ytdl(user_input).await
        } else {
            ytdl_search(user_input).await
        }.map_err(|_| CommandError::from(format!("Could not load song for input {user_input}")))?;

        let source_url = input.metadata.source_url.ok_or(CommandError::from(format!("Could not load song for input {user_input}")))?;
        let song_name = input.metadata.title.unwrap_or(UNKNOWN_TRACK_TITLE.to_string());
        let song_duration = input.metadata.duration;

        let song = Song {
            title: song_name,
            url: source_url,
            duration: song_duration,
        };

        push_song_to_guild(ctx, &guild_id, song, insert_last).await?;
    }

    play_next_if_queue_empty(ctx, &guild_id, msg).await;

    Ok(())
}

async fn play_next_if_queue_empty(ctx: &Context, guild_id: &GuildId, msg: &Message) {
    info!("play_next_if_queue_empty start");
    let is_not_playing: bool;
    let queue_is_empty: bool;

    {
        let data = ctx.data.read().await;
        let songs = get_songs_from_guild(&data, guild_id).await;

        let track_handle: Option<&TrackHandle> = get_track_handle(&data, guild_id).await;
        is_not_playing = track_handle.is_none();
        queue_is_empty = songs.is_empty()
    }

    if !queue_is_empty && is_not_playing {
        while play_next_song(ctx, guild_id, &msg.channel_id).await.is_err() {
            info!("Next song failed")
        }
    }

    info!("play_next_if_queue_empty end");
}

#[command]
#[only_in(guilds)]
async fn pause(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;
    let data = ctx.data.read().await;

    match get_track_handle(&data, &guild_id).await {
        Some(track_handle) => track_handle.pause()?,
        None => check_msg(msg.channel_id.say(&ctx.http, "o_O Already stopped").await),
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn stop(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    clear_queue(ctx, &guild_id).await?;
    stop_current_track(ctx, &guild_id, Some(&msg.channel_id)).await?;
    leave(ctx, msg).await?;

    Ok(())
}

async fn clear_queue(ctx: &Context, guild_id: &GuildId) -> CommandResult {
    let data = &mut ctx.data.write().await;
    let server = get_server_mut(data, &guild_id)?;

    server.queue.clear();

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn queue(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let data = ctx.data.read().await;
    let songs = get_songs_from_guild(&data, &guild_id).await;

    if songs.is_empty() {
        check_msg(msg.channel_id.say(&ctx.http, "The queue is empty!").await);
    } else {
        let max_songs = 20;
        let mut songs_titles: Vec<String> = Vec::with_capacity(min(songs.len(), max_songs));

        for (index, song) in songs.iter().take(max_songs).enumerate() {
            let song_index = index + 1;
            songs_titles.push(format!("{song_index} - {}", song.title));
        }

        let songs_formatted = songs_titles.join("\n");

        check_msg(msg.channel_id.say(&ctx.http, format!("**Queue**:\n```{songs_formatted}```")).await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn unpause(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let data = ctx.data.read().await;

    match get_track_handle(&data, &guild_id).await {
        Some(track_handle) => {
            track_handle.play()?
        }
        None => {
            check_msg(msg.channel_id.say(&ctx.http, "o_O Already stopped").await);
        }
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn next(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    info!("NEXT - Next command invoked from guild {}!", guild_id.0);

    let is_queue_empty: bool;

    {
        let data = ctx.data.read().await;
        let songs_queue = get_songs_from_guild(&data, &guild_id).await;
        info!("NEXT - There are {} songs in the queue!", songs_queue.len());

        is_queue_empty = songs_queue.is_empty();
    }

    if !is_queue_empty {
        info!("NEXT - Stopping current song");
        // Stopping the current song will automatically start the next one
        stop_current_track(ctx, &guild_id, Some(&msg.channel_id)).await?;
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn shuffle(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    info!("Shuffle - Next command invoked from guild {}!", guild_id.0);
    let data = &mut ctx.data.write().await;
    let server = get_server_mut(data, &guild_id)?;

    let songs = &mut server.queue;
    songs.make_contiguous().shuffle(&mut thread_rng());

    msg.react(&ctx.http, Unicode("👍".to_string())).await?;
    
    Ok(())
}

#[command]
#[only_in(guilds)]
async fn goto(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let index = match args.single::<usize>() {
        Ok(url) => url,
        Err(_) => {
            check_msg(msg.channel_id.say(&ctx.http, "Invalid song index. Check the queue to list the songs.").await);

            return Ok(());
        }
    };

    info!("goto - Next command invoked from guild {}!", guild_id.0);
    let is_valid_index: bool;

    {
        let data = &mut ctx.data.write().await;

        let server = get_server_mut(data, &guild_id)?;
        let songs = &mut server.queue;

        is_valid_index = index < songs.len();

        if is_valid_index {
            for _ in 1..index {
                songs.pop_front();
            }
        }
    }

    if is_valid_index {
        stop_current_track(ctx, &guild_id, Some(&msg.channel_id)).await?;
    }

    Ok(())
}

async fn stop_current_track(ctx: &Context, guild_id: &GuildId, channel_id: Option<&ChannelId>) -> CommandResult {
    {
        let data = ctx.data.read().await;

        match get_track_handle(&data, guild_id).await {
            Some(track_handle) => {
                track_handle.stop()?
            }
            None => {
                let error_message = "o_O Already stopped";
                if let Some(channel) = channel_id {
                    check_msg(channel.say(&ctx.http, error_message).await);
                } else {
                    return Err(CommandError::from(error_message));
                }
            }
        }
    }

    remove_track_handle(ctx, guild_id).await?;

    Ok(())
}

async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    leave_current_channel(ctx, msg).await
}

async fn leave_current_channel(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

    let has_handler = manager.get(guild_id).is_some();

    if has_handler {
        if let Err(e) = manager.remove(guild_id).await {
            check_msg(msg.channel_id.say(&ctx.http, format!("Failed: {e:?}")).await);
        }

        check_msg(msg.channel_id.say(&ctx.http, "Left voice channel").await);
    } else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
    }

    Ok(())
}

async fn play_next_song(ctx: &Context, guild_id: &GuildId, channel_id: &ChannelId) -> Result<(), CommandError> {
    if let Some(song) = get_next_song(ctx, guild_id).await {
        info!("PLAY_NEXT_SONG - Next song is {} - {}", song.title, song.url);

        let manager = songbird::get(ctx).await
            .expect("Songbird Voice client placed in at initialisation.").clone();

        if let Some(handler_lock) = manager.get(*guild_id) {
            let mut handler = handler_lock.lock().await;

            let source = match ytdl(&song.url).await {
                Ok(source) => source,
                Err(why) => {
                    check_msg(channel_id.say(&ctx.http, format!("Could not play {} due to error {}", song.title, why)).await);

                    info!("Err starting source: {why:?}");

                    return Err(CommandError::from(why));
                }
            };

            handler.stop(); // Just in case something was playing before
            let track_handle = handler.play_source(source);

            track_handle.add_event(
                Event::Track(End),
                SongEndNotifier {
                    guild_id: *guild_id,
                    channel_id: *channel_id,
                    ctx: ctx.clone(),
                },
            ).expect("Add event END failed");

            set_new_track_handle(track_handle, ctx, guild_id).await?;

            // TODO
            // let duration_text = if let Some(duration) = &song.duration {
            //     let seconds = duration.as_secs();
            //     let minutes = seconds / 60;
            //     let display_seconds = seconds - (minutes * 60);
            //
            //     format!("\n> Duration: `{}:{:0>2}`", minutes, display_seconds)
            // } else {
            //     "".to_string()
            // };

            check_msg(
                channel_id.say(
                    &ctx.http,
                    format!("Playing song [{}]({})", song.title, song.url),
                ).await
            );
        } else {
            check_msg(channel_id.say(&ctx.http, "Not in a voice channel to play in").await);
        }
    }

    Ok(())
}

async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let channel_id = get_guild(ctx, msg)?
        .voice_states.get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let connect_to = match channel_id {
        Some(channel) => channel,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Err(CommandError::from("Not in a voice channel"));
        }
    };

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

    let _handler = manager.join(guild_id, connect_to).await;

    Ok(())
}

async fn deafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = get_guild_id(ctx, msg)?;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

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


async fn set_new_track_handle(track_handle: TrackHandle, ctx: &Context, guild_id: &GuildId) -> Result<(), CommandError> {
    let data = &mut ctx.data.write().await;
    let server = get_server_mut(data, guild_id)?;

    server.track_handle = Some(track_handle);

    Ok(())
}

async fn remove_track_handle(ctx: &Context, guild_id: &GuildId) -> Result<(), CommandError> {
    let data = &mut ctx.data.write().await;
    let server = get_server_mut(data, guild_id)?;
    server.track_handle = None;

    Ok(())
}

async fn get_track_handle<'a>(data: &'a RwLockReadGuard<'a, TypeMap>, guild_id: &GuildId) -> Option<&'a TrackHandle> {
    let duba_guild = data.get::<ServersManager>()?;

    let guilds = &duba_guild.servers;
    let guild = guilds.get(&guild_id.0)?;

    return guild.track_handle.as_ref();
}

async fn get_songs_from_guild<'a>(data: &'a RwLockReadGuard<'_, TypeMap>, guild_id: &GuildId) -> &'a VecDeque<Song> {
    let duba_guild = data.get::<ServersManager>().expect("Guild get failed");
    let guilds = &duba_guild.servers;
    let guild = guilds.get(&guild_id.0).expect("Guild should exist");

    let count = guild.queue.len();

    info!("There are {count} songs:");

    &guild.queue
}

async fn get_next_song(ctx: &Context, guild_id: &GuildId) -> Option<Song> {
    let data = &mut ctx.data.write().await;
    let server = get_server_mut(data, guild_id).ok()?;
    let song = server.queue.pop_front();

    match &song {
        None => info!("GET_NEXT_SONG - Queue is empty"),
        Some(song) => info!("GET_NEXT_SONG - Next song is {} - {}", song.title, song.url),
    };

    song
}

async fn push_song_to_guild(ctx: &Context, guild_id: &GuildId, song: Song, insert_last: bool) -> Result<(), CommandError> {
    let data = &mut ctx.data.write().await;
    let duba_guild = data.get_mut::<ServersManager>().ok_or(CommandError::from("Guild not found"))?;
    let guilds = &mut duba_guild.servers;
    let guild = guilds.get_mut(&guild_id.0);

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

            guilds.insert(guild_id.0, new_guild_data);
        }
    };

    Ok(())
}

async fn push_songs_list_to_server(ctx: &Context, guild_id: &GuildId, songs: Vec<Song>) -> Result<(), CommandError> {
    let data = &mut ctx.data.write().await;
    let duba_guild = data.get_mut::<ServersManager>().ok_or(CommandError::from("Guild not found"))?;
    let servers = &mut duba_guild.servers;
    let server = servers.get_mut(&guild_id.0);

    match server {
        Some(data) => {
            let queue = &mut data.queue;
            let mut new_songs = VecDeque::from(songs);
            queue.append(&mut new_songs);
        }
        None => {
            let new_guild_data = ServerData {
                track_handle: None,
                queue: VecDeque::from(songs),
            };

            servers.insert(guild_id.0, new_guild_data);
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

fn get_guild(ctx: &Context, msg: &Message) -> CommandResult<Guild> {
    msg.guild(&ctx.cache).ok_or(CommandError::from("Guild not found"))
}

fn get_guild_id(ctx: &Context, msg: &Message) -> CommandResult<GuildId> {
    let guild_id = get_guild(ctx, msg)?.id;

    Ok(guild_id)
}

struct SongEndNotifier {
    guild_id: GuildId,
    channel_id: ChannelId,
    ctx: Context,
}

#[async_trait]
impl VoiceEventHandler for SongEndNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        info!("End notifier triggered");

        match remove_track_handle(&self.ctx, &self.guild_id).await {
            Ok(_) => {
                // If playing next song fails, try with the another one until it works
                while (play_next_song(&self.ctx, &self.guild_id, &self.channel_id).await).is_err() {}
            }
            Err(_) => { info!("Remove track failed") }
        }

        None
    }
}

fn get_server_mut<'a>(data: &'a mut RwLockWriteGuard<TypeMap>, guild_id: &GuildId) -> Result<&'a mut ServerData, CommandError> {
    let duba_guild = data.get_mut::<ServersManager>().ok_or(CommandError::from("Guild not found"))?;
    let servers = &mut duba_guild.servers;
    let server = servers.get_mut(&guild_id.0).ok_or(CommandError::from("Guild not found"))?;

    Ok(server)
}