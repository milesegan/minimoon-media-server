//! The metadata MMP sends for an item, as the readers produce it (spec §5, wire schema).
//!
//! These are MMP's own types, not serializations of a reader's structs, so a change to the
//! reading code doesn't change the protocol. Every optional field is left out of the JSON when
//! it's `None`: absent means unknown.

use serde::{Deserialize, Serialize};

/// Audio properties of an audio item (`audio` in the wire schema).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// kbit/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
}

/// An audio item's tags, as read (`tags` on an audio item).
///
/// Text is exactly as the file holds it (trimmed, as the apps' reader does). `compilation`,
/// `has_lyrics` and `has_embedded_art` are always present whenever `tags` is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTags {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_track_gain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_album_gain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_number_total: Option<u32>,
    /// The file's year field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// The year of the file's recording-date field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_year: Option<u32>,
    /// The raw number on the format's own scale (MP4 `rate` 0–100, Vorbis `RATING` 0–255).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<u32>,
    pub compilation: bool,
    pub has_lyrics: bool,
    pub has_embedded_art: bool,
}

/// What the audio reader returns for one file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioRead {
    pub audio: AudioInfo,
    pub tags: AudioTags,
}

/// The container's stream facts for a video item (`video` in the wire schema).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// `matroska` or `mp4`.
    pub container: String,
    /// Every track, in container order.
    pub streams: Vec<Stream>,
}

/// One track of a video file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stream {
    /// `video`, `audio`, `subtitle` or another track type the container names.
    #[serde(rename = "type")]
    pub kind: String,
    /// The container's own track identifier (Matroska `TrackNumber`, MP4 `track_ID`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The nominal frame rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<f64>,
    /// `sdr`, `pq` or `hlg`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<String>,
    /// Present only when Dolby Vision is detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dolby_vision_profile: Option<u8>,
    /// `true` only when detected; never `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr10_plus: Option<bool>,
    /// The decoded channel count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// BCP 47 when the container has it, otherwise its ISO 639-2 code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forced: Option<bool>,
}

impl Stream {
    /// A stream of the given type with every fact unknown.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            track_number: None,
            codec: None,
            width: None,
            height: None,
            frame_rate: None,
            transfer: None,
            dolby_vision_profile: None,
            hdr10_plus: None,
            channels: None,
            language: None,
            title: None,
            default: None,
            forced: None,
        }
    }
}

/// What a video container itself holds as tags (`tags` on a video item).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTags {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl VideoTags {
    /// True when the container held none of the fields.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// What the video reader returns for one file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoRead {
    pub video: VideoInfo,
    pub tags: VideoTags,
}

/// An embedded picture: its bytes and MIME type.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub data: Vec<u8>,
    pub mime: String,
}

/// Why a metadata read failed (spec §8, reader version and read status).
#[derive(Clone, Debug, PartialEq)]
pub enum ReadError {
    /// A timeout or I/O error, which may succeed later.
    Temporary(String),
    /// An unsupported or malformed file, which won't.
    Unreadable(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Temporary(message) => write!(f, "temporary read failure: {message}"),
            Self::Unreadable(message) => write!(f, "unreadable: {message}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl ReadError {
    /// Classifies an I/O error: anything but a malformed file is worth retrying.
    pub fn from_io(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
                Self::Unreadable(error.to_string())
            }
            _ => Self::Temporary(error.to_string()),
        }
    }
}
