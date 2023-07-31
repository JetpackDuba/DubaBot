use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serenity::framework::standard::CommandError;

use crate::models::Song;

#[derive(Serialize, Deserialize)]
pub struct PlaylistSong {
    #[serde(rename = "_type")]
    pub type_field: String,
    #[serde(rename = "ie_key")]
    pub ie_key: String,
    pub id: String,
    pub url: String,
    pub title: String,
    pub description: Option<String>,
    pub duration: Option<i64>,
    #[serde(rename = "playlist_count")]
    pub playlist_count: i64,
    pub playlist: String,
    #[serde(rename = "playlist_id")]
    pub playlist_id: String,
    #[serde(rename = "playlist_title")]
    pub playlist_title: String,
    #[serde(rename = "n_entries")]
    pub n_entries: i64,
    #[serde(rename = "playlist_index")]
    pub playlist_index: i64,
    #[serde(rename = "__last_playlist_index")]
    pub last_playlist_index: i64,
    #[serde(rename = "playlist_autonumber")]
    pub playlist_autonumber: i64,
    pub epoch: i64,
    #[serde(rename = "duration_string")]
    pub duration_string: String,
}

pub fn songs_list_from_playlist_url(url: &str) -> Result<Vec<Song>, CommandError> {
    println!("Getting songs from playlist {url}");

    let output = Command::new("yt-dlp")
        .arg("-j")
        .arg("--flat-playlist")
        .arg(url)
        .output()
        .expect("yt-dlp command failed to start");

    let error = String::from_utf8(output.stderr).map_err(|_| CommandError::from("Error reading stderr"))?;
    let result = String::from_utf8(output.stdout).map_err(|_| CommandError::from("Error reading stdout"))?;

    println!("Error: {error}");
    println!("Result: {result}");

    if !result.is_empty() {
        let lines: Vec<&str> = result.split('\n').collect();

        let playlist_songs = lines
            .iter()
            .filter_map(|line| {
                let playlist_song: PlaylistSong = serde_json::from_str(line).ok()?;

                let duration: Option<Duration> = playlist_song.duration
                    .and_then(|duration| {
                        let d: u64 = duration.try_into().ok()?;
                        Some(Duration::from_nanos(d))
                    });

                let song = Song {
                    title: playlist_song.title,
                    url: playlist_song.url,
                    duration,
                };

                Some(song)
            })
            .collect::<Vec<Song>>();

        if playlist_songs.len() < lines.len() {
            println!("Some songs have been skips due to errors during parsing");
        }

        Ok(playlist_songs)
    } else {
        Err(CommandError::from(error))
    }
}