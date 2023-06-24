use std::collections::HashMap;
use std::env;

use dotenvy::dotenv;
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
use serenity::model::id::ChannelId;
use serenity::model::prelude::GuildId;
use serenity::prelude::TypeMap;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit};
use songbird::TrackEvent::End;
use songbird::tracks::TrackHandle;
use tokio::sync::RwLockReadGuard;

struct Handler;

pub struct GuildManager;

pub struct GuildData {
    pub track_handle: Option<TrackHandle>,
    pub songs: Vec<String>,
}

pub struct DubaGuild {
    pub guilds: HashMap<u64, GuildData>,
}

impl serenity::prelude::TypeMapKey for GuildManager {
    type Value = DubaGuild;
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        println!("Message received {}", msg.content);
        if msg.content == "!ping" {
            // Sending a message can fail, due to a network error, an
            // authentication error, or lack of permissions to post in the
            // channel, so log to stdout when some error happens, with a
            // description of it.

            if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
                println!("Error sending message: {:?}", why);
            }
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[group]
#[commands(deafen, leave, mute, play, ping, undeafen, unmute, pause, unpause, next, stop)]
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
            c.prefix("~")
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
        let duba_guild = DubaGuild {
            guilds: HashMap::new()
        };

        w.insert::<GuildManager>(duba_guild);
    }

    tokio::spawn(async move {
        let _ = client.start().await.map_err(|why| println!("Client ended: {:?}", why));
    });


    tokio::signal::ctrl_c().await.expect("TODO: panic message");
    println!("Received Ctrl-C, shutting down.");
}

#[command]
#[only_in(guilds)]
async fn deafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

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
        check_msg(msg.channel_id.say(&ctx.http, "Already deafened").await);
    } else {
        if let Err(e) = handler.deafen(true).await {
            check_msg(msg.channel_id.say(&ctx.http, format!("Failed: {:?}", e)).await);
        }

        check_msg(msg.channel_id.say(&ctx.http, "Deafened").await);
    }

    Ok(())
}


async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let channel_id = guild
        .voice_states.get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let connect_to = match channel_id {
        Some(channel) => channel,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Ok(());
        }
    };

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

    let _handler = manager.join(guild_id, connect_to).await;

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();
    let has_handler = manager.get(guild_id).is_some();

    if has_handler {
        if let Err(e) = manager.remove(guild_id).await {
            check_msg(msg.channel_id.say(&ctx.http, format!("Failed: {:?}", e)).await);
        }

        check_msg(msg.channel_id.say(&ctx.http, "Left voice channel").await);
    } else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn mute(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

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

    if handler.is_mute() {
        check_msg(msg.channel_id.say(&ctx.http, "Already muted").await);
    } else {
        if let Err(e) = handler.mute(true).await {
            check_msg(msg.channel_id.say(&ctx.http, format!("Failed: {:?}", e)).await);
        }

        check_msg(msg.channel_id.say(&ctx.http, "Now muted").await);
    }

    Ok(())
}

#[command]
async fn ping(ctx: &Context, msg: &Message) -> CommandResult {
    check_msg(msg.channel_id.say(&ctx.http, "Pong!").await);

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    join(ctx, msg).await?;

    let url = match args.single::<String>() {
        Ok(url) => url,
        Err(_) => {
            check_msg(msg.channel_id.say(&ctx.http, "Must provide a URL to a video or audio").await);

            return Ok(());
        }
    };

    if !url.starts_with("http") {
        check_msg(msg.channel_id.say(&ctx.http, "Must provide a valid URL").await);

        return Ok(());
    }

    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    push_song_to_guild(ctx, &guild_id, url.clone()).await;

    let songs = get_songs_from_guild(ctx, &guild_id).await;
    let is_playing: bool;

    {
        let data = ctx.data.read().await;
        let track_handle: &Option<TrackHandle> = get_track_handle(&data, &guild_id).await;
        is_playing = track_handle.is_none()
    }

    if songs.len() == 1 && is_playing {
        play_next_song(ctx, &guild_id, &msg.channel_id).await;
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn pause(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let data = ctx.data.read().await;

    match get_track_handle(&data, &guild_id).await {
        Some(track_handle) => {
            track_handle.pause()?
        }
        None => {
            check_msg(msg.channel_id.say(&ctx.http, "o_O Already stopped").await);
        }
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn stop(ctx: &Context, msg: &Message) -> CommandResult {
    stop_current_track(ctx, msg).await
}

async fn stop_current_track(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    {
        let data = ctx.data.read().await;

        match get_track_handle(&data, &guild_id).await {
            Some(track_handle) => {
                track_handle.stop()?
            }
            None => {
                check_msg(msg.channel_id.say(&ctx.http, "o_O Already stopped").await);
            }
        }
    }

    remove_track_handle(ctx, &guild_id).await;

    Ok(())
}


#[command]
#[only_in(guilds)]
async fn unpause(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

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
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    println!("NEXT - Next command invoked from guild {}!", guild_id.0);

    let songs = get_songs_from_guild(ctx, &guild_id).await;

    println!("NEXT - There are {} songs in the queue!", songs.len());

    if songs.len() == 1 {
        println!("NEXT - Stopping current song");
        stop_current_track(ctx, msg).await?;

        println!("NEXT - Playing next song");
        play_next_song(ctx, &guild_id, &msg.channel_id).await;
    }

    Ok(())
}

async fn play_next_song(ctx: &Context, guild_id: &GuildId, channel_id: &ChannelId) {
    if let Some(song) = get_next_song(ctx, guild_id).await {
        println!("PLAY_NEXT_SONG - Next song is {song}");

        let manager = songbird::get(ctx).await
            .expect("Songbird Voice client placed in at initialisation.").clone();

        if let Some(handler_lock) = manager.get(*guild_id) {
            let mut handler = handler_lock.lock().await;

            let source = match songbird::ytdl(&song).await {
                Ok(source) => source,
                Err(why) => {
                    println!("Err starting source: {:?}", why);

                    check_msg(channel_id.say(&ctx.http, "Error sourcing ffmpeg").await);

                    return;
                }
            };

            let track_handle = handler.play_source(source);

            track_handle.add_event(
                Event::Track(End),
                SongEndNotifier {
                    guild_id: *guild_id,
                    channel_id: *channel_id,
                    ctx: ctx.clone(),
                },
            ).expect("Add event END failed");

            set_new_track_handle(track_handle, ctx, guild_id).await;

            check_msg(channel_id.say(&ctx.http, "Playing song").await);
        } else {
            check_msg(channel_id.say(&ctx.http, "Not in a voice channel to play in").await);
        }
    }
}

async fn set_new_track_handle(track_handle: TrackHandle, ctx: &Context, guild_id: &GuildId) {
    let data = &mut ctx.data.write().await;
    let duba_guild = data.get_mut::<GuildManager>().expect("Guild get failed");
    let guilds = &mut duba_guild.guilds;
    let guild = guilds.get_mut(&guild_id.0).expect("Guild data should exist");

    guild.track_handle = Some(track_handle)
}

async fn remove_track_handle(ctx: &Context, guild_id: &GuildId) {
    let data = &mut ctx.data.write().await;
    let duba_guild = data.get_mut::<GuildManager>().expect("Guild get failed");
    let guilds = &mut duba_guild.guilds;
    let guild = guilds.get_mut(&guild_id.0).expect("Guild data should exist");

    guild.track_handle = None;
}

async fn get_track_handle<'a>(data: &'a RwLockReadGuard<'a, TypeMap>, guild_id: &'a GuildId) -> &'a Option<TrackHandle> {
    let duba_guild = data.get::<GuildManager>().unwrap();
    let guilds = &duba_guild.guilds;
    let guild = guilds.get(&guild_id.0).unwrap();

    &guild.track_handle
}

async fn get_songs_from_guild(ctx: &Context, guild_id: &GuildId) -> Vec<String> {
    let data = ctx.data.read().await;
    let duba_guild = data.get::<GuildManager>().expect("Guild get failed");
    let guilds = &duba_guild.guilds;
    let guild = guilds.get(&guild_id.0).expect("Guild should exist");

    let count = guild.songs.len();

    println!("There are {count} songs:");

    guild.songs.to_vec()
}

async fn get_next_song(ctx: &Context, guild_id: &GuildId) -> Option<String> {
    let data = &mut ctx.data.write().await;
    let duba_guild = data.get_mut::<GuildManager>().expect("Guild get failed");
    let guilds = &mut duba_guild.guilds;
    let guild = guilds.get_mut(&guild_id.0)?;

    let song = guild.songs.pop();

    match &song {
        None => println!("GET_NEXT_SONG - Queue is empty"),
        Some(song) => println!("GET_NEXT_SONG - Next song is {song}"),
    };

    song
}

async fn push_song_to_guild(ctx: &Context, guild_id: &GuildId, url: String) {
    let data = &mut ctx.data.write().await;
    let duba_guild = data.get_mut::<GuildManager>().expect("Guild get failed");
    let guilds = &mut duba_guild.guilds;
    let guild = guilds.get_mut(&guild_id.0);

    match guild {
        Some(data) => {
            data.songs.push(url);
        }
        None => {
            let new_guild_data = GuildData {
                track_handle: None,
                songs: vec![url],
            };

            guilds.insert(guild_id.0, new_guild_data);
        }
    };
}

#[command]
#[only_in(guilds)]
async fn undeafen(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        if let Err(e) = handler.deafen(false).await {
            check_msg(msg.channel_id.say(&ctx.http, format!("Failed: {:?}", e)).await);
        }

        check_msg(msg.channel_id.say(&ctx.http, "Undeafened").await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Not in a voice channel to undeafen in").await);
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn unmute(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        if let Err(e) = handler.mute(false).await {
            check_msg(msg.channel_id.say(&ctx.http, format!("Failed: {:?}", e)).await);
        }

        check_msg(msg.channel_id.say(&ctx.http, "Unmuted").await);
    } else {
        check_msg(msg.channel_id.say(&ctx.http, "Not in a voice channel to unmute in").await);
    }

    Ok(())
}

/// Checks that a message successfully sent; if not, then logs why to stdout.
fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        println!("Error sending message: {:?}", why);
    }
}

struct SongEndNotifier {
    guild_id: GuildId,
    channel_id: ChannelId,
    ctx: Context,
}

#[async_trait]
impl VoiceEventHandler for SongEndNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        println!("End notifier triggered");

        remove_track_handle(&self.ctx, &self.guild_id).await;
        play_next_song(&self.ctx, &self.guild_id, &self.channel_id).await;

        None
    }
}
