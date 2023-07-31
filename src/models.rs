use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use songbird::tracks::TrackHandle;

#[derive(Clone)]
pub struct Song {
    pub title: String,
    pub url: String,
    pub duration: Option<Duration>,
}

pub struct ServerData {
    pub track_handle: Option<TrackHandle>,
    pub queue: VecDeque<Song>,
}

pub struct DubaServers {
    pub servers: HashMap<u64, ServerData>,
}