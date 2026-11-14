//! Metadata readers: item kinds and MIME types by extension, and the audio and video readers
//! (spec §8, "Tag reading" and "Reading metadata safely").

pub mod audio;
pub mod video;

use crate::model::{AudioRead, Picture, ReadError, VideoRead};
use serde::Serialize;
use std::path::Path;

/// An item's kind (spec §5, item kinds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Audio,
    Video,
    Image,
    Subtitle,
    Lyrics,
    Other,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Image => "image",
            Self::Subtitle => "subtitle",
            Self::Lyrics => "lyrics",
            Self::Other => "other",
        }
    }
}

/// Which reader handles a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    Audio,
    Video,
}

/// What the extension says about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    pub kind: ItemKind,
    pub mime: Option<&'static str>,
    pub reader: Option<Reader>,
}

/// Classifies a file by its extension, case-insensitively.
pub fn file_type(name: &str) -> FileType {
    let ext = name
        .rfind('.')
        .filter(|&dot| dot > 0)
        .map(|dot| name[dot + 1..].to_ascii_lowercase())
        .unwrap_or_default();
    let (kind, mime, reader) = match ext.as_str() {
        "mp3" => (ItemKind::Audio, "audio/mpeg", Some(Reader::Audio)),
        "m4a" | "m4b" | "m4r" => (ItemKind::Audio, "audio/mp4", Some(Reader::Audio)),
        "aac" => (ItemKind::Audio, "audio/aac", Some(Reader::Audio)),
        "flac" => (ItemKind::Audio, "audio/flac", Some(Reader::Audio)),
        "ogg" | "oga" => (ItemKind::Audio, "audio/ogg", Some(Reader::Audio)),
        "opus" => (ItemKind::Audio, "audio/opus", Some(Reader::Audio)),
        "spx" => (ItemKind::Audio, "audio/ogg", Some(Reader::Audio)),
        "wav" => (ItemKind::Audio, "audio/wav", Some(Reader::Audio)),
        "aif" | "aiff" | "aifc" => (ItemKind::Audio, "audio/aiff", Some(Reader::Audio)),
        "ape" => (ItemKind::Audio, "audio/x-ape", Some(Reader::Audio)),
        "wv" => (ItemKind::Audio, "audio/x-wavpack", Some(Reader::Audio)),
        "mpc" => (ItemKind::Audio, "audio/x-musepack", Some(Reader::Audio)),
        "dsf" => (ItemKind::Audio, "audio/x-dsf", None),
        "wma" => (ItemKind::Audio, "audio/x-ms-wma", None),
        "mka" => (ItemKind::Audio, "audio/x-matroska", None),
        "caf" => (ItemKind::Audio, "audio/x-caf", None),
        "mkv" => (ItemKind::Video, "video/x-matroska", Some(Reader::Video)),
        "webm" => (ItemKind::Video, "video/webm", Some(Reader::Video)),
        "mp4" => (ItemKind::Video, "video/mp4", Some(Reader::Video)),
        "m4v" => (ItemKind::Video, "video/x-m4v", Some(Reader::Video)),
        "mov" => (ItemKind::Video, "video/quicktime", Some(Reader::Video)),
        "avi" => (ItemKind::Video, "video/x-msvideo", None),
        "ts" | "m2ts" | "mts" => (ItemKind::Video, "video/mp2t", None),
        "mpg" | "mpeg" => (ItemKind::Video, "video/mpeg", None),
        "wmv" => (ItemKind::Video, "video/x-ms-wmv", None),
        "jpg" | "jpeg" => (ItemKind::Image, "image/jpeg", None),
        "png" => (ItemKind::Image, "image/png", None),
        "webp" => (ItemKind::Image, "image/webp", None),
        "gif" => (ItemKind::Image, "image/gif", None),
        "heic" => (ItemKind::Image, "image/heic", None),
        "bmp" => (ItemKind::Image, "image/bmp", None),
        "tif" | "tiff" => (ItemKind::Image, "image/tiff", None),
        "srt" => (ItemKind::Subtitle, "application/x-subrip", None),
        "ass" | "ssa" => (ItemKind::Subtitle, "text/x-ssa", None),
        "vtt" => (ItemKind::Subtitle, "text/vtt", None),
        "sup" | "idx" | "sub" => (ItemKind::Subtitle, "application/octet-stream", None),
        "lrc" => (ItemKind::Lyrics, "text/plain", None),
        _ => {
            return FileType {
                kind: ItemKind::Other,
                mime: None,
                reader: None,
            }
        }
    };
    FileType {
        kind,
        mime: Some(mime),
        reader,
    }
}

/// The version of the reader that handles a kind of file. Files read by an older version are
/// re-read in the background after a server update.
pub fn reader_version(reader: Option<Reader>) -> u32 {
    match reader {
        Some(Reader::Audio) => audio::READER_VERSION,
        Some(Reader::Video) => video::READER_VERSION,
        None => 0,
    }
}

#[derive(Serialize)]
struct AudioFragment<'a> {
    audio: &'a crate::model::AudioInfo,
    tags: &'a crate::model::AudioTags,
}

#[derive(Serialize)]
struct VideoFragment<'a> {
    video: &'a crate::model::VideoInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<&'a crate::model::VideoTags>,
}

/// The audio reader's result as the JSON object merged into the item (`audio` and `tags`).
pub fn audio_fragment(read: &AudioRead) -> String {
    serde_json::to_string(&AudioFragment {
        audio: &read.audio,
        tags: &read.tags,
    })
    .expect("metadata serializes")
}

/// The video reader's result as the JSON object merged into the item (`video` and `tags`).
pub fn video_fragment(read: &VideoRead) -> String {
    serde_json::to_string(&VideoFragment {
        video: &read.video,
        tags: (!read.tags.is_empty()).then_some(&read.tags),
    })
    .expect("metadata serializes")
}

/// Reads a file's metadata and returns it as a JSON object to merge into its item, or `None`
/// when no reader handles the file. Panics in a reader are caught and reported as unreadable.
pub fn read_metadata(path: &Path, reader: Option<Reader>) -> Result<Option<String>, ReadError> {
    let Some(reader) = reader else {
        return Ok(None);
    };
    let path = path.to_path_buf();
    catch(move || match reader {
        Reader::Audio => audio::read_audio(&path).map(|read| Some(audio_fragment(&read))),
        Reader::Video => video::read_video(&path).map(|read| Some(video_fragment(&read))),
    })
}

/// The first embedded picture of an audio or video file.
pub fn read_art(path: &Path, reader: Option<Reader>) -> Result<Option<Picture>, ReadError> {
    let path = path.to_path_buf();
    match reader {
        Some(Reader::Audio) => catch(move || audio::read_audio_art(&path)),
        Some(Reader::Video) => catch(move || video::read_video_art(&path)),
        None => Ok(None),
    }
}

/// An audio file's embedded lyrics.
pub fn read_lyrics(path: &Path, reader: Option<Reader>) -> Result<Option<String>, ReadError> {
    let path = path.to_path_buf();
    match reader {
        Some(Reader::Audio) => catch(move || audio::read_audio_lyrics(&path)),
        _ => Ok(None),
    }
}

fn catch<T>(body: impl FnOnce() -> Result<T, ReadError>) -> Result<T, ReadError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err(ReadError::Unreadable("the reader panicked".into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        assert_eq!(file_type("a.FLAC").kind, ItemKind::Audio);
        assert_eq!(file_type("a.mkv").reader, Some(Reader::Video));
        assert_eq!(file_type("Movie.en.srt").kind, ItemKind::Subtitle);
        assert_eq!(file_type("song.lrc").kind, ItemKind::Lyrics);
        assert_eq!(file_type("notes.txt").kind, ItemKind::Other);
        assert_eq!(file_type("notes.txt").mime, None);
        assert_eq!(file_type("README").kind, ItemKind::Other);
    }
}
