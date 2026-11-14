//! The video reader: a video file's stream facts and container tags (spec §5, video item).
//!
//! Native Rust, chosen by the reader test (`docs/video-reader-test.md`): `matroska-demuxer` and
//! `mp4-atom` for the containers, `scuffle-h265`, `h264-reader` and `scuffle-av1` for colour in
//! the codec headers, and `lofty` for MP4 tags and covers. The pieces no crate covers are
//! hand-written: the Dolby Vision configuration record, Matroska attachment headers, raw EBML
//! and MP4 box scanning, the HDR10+ SEI check in the first HEVC frame, and the AAC
//! AudioSpecificConfig.
//!
//! Every read is bounded: no single structure (a `moov`, a Matroska element, an attachment, the
//! first frame) larger than [`MAX_STRUCTURE_BYTES`] is read, and a panic in a parsing crate on
//! malformed input becomes an [`ReadError::Unreadable`] instead of taking the server down.

mod codec;
mod mkv;
mod mp4;

use crate::model::{Picture, ReadError, VideoRead};
use std::fs::File;
use std::io::Read;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

/// The video reader's version. Bump it whenever a change would read existing files
/// differently, so the server re-reads them (spec §8, reader version).
pub const READER_VERSION: u32 = 1;

/// The largest single structure the reader will read into memory.
pub(crate) const MAX_STRUCTURE_BYTES: usize = 64 * 1024 * 1024;

/// Reads a video file's streams and container tags.
///
/// The container is chosen from the file's first bytes, not its extension: Matroska and WebM
/// (`.mkv`, `.mka`, `.webm`) or MP4 and QuickTime (`.mp4`, `.m4v`, `.mov`).
pub fn read_video(path: &Path) -> Result<VideoRead, ReadError> {
    guarded(|| match sniff(path)? {
        Container::Matroska => mkv::read(path),
        Container::Mp4 => mp4::read(path),
    })
}

/// The first embedded cover: a Matroska attachment (one named `cover…` if there is one, else
/// the first image) or the MP4 `covr` atom. `Ok(None)` when the file has none.
pub fn read_video_art(path: &Path) -> Result<Option<Picture>, ReadError> {
    guarded(|| match sniff(path)? {
        Container::Matroska => mkv::read_art(path),
        Container::Mp4 => mp4::read_art(path),
    })
}

#[derive(Debug, PartialEq)]
enum Container {
    Matroska,
    Mp4,
}

/// Identifies the container from the first 12 bytes: the EBML magic, or a known top-level MP4
/// box type in bytes 4–8.
fn sniff(path: &Path) -> Result<Container, ReadError> {
    let mut head = [0u8; 12];
    let mut file = File::open(path).map_err(io_error)?;
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    container_of(&head[..filled]).ok_or_else(|| bad("not a Matroska or MP4 file"))
}

fn container_of(head: &[u8]) -> Option<Container> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Container::Matroska);
    }
    const MP4_TOP_LEVEL: [&[u8; 4]; 9] = [
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"uuid", b"styp",
    ];
    let kind = head.get(4..8)?;
    MP4_TOP_LEVEL
        .iter()
        .any(|known| kind == *known)
        .then_some(Container::Mp4)
}

/// Runs a read, turning a panic in a parsing crate into an unreadable file.
fn guarded<T>(read: impl FnOnce() -> Result<T, ReadError>) -> Result<T, ReadError> {
    panic::catch_unwind(AssertUnwindSafe(read))
        .unwrap_or_else(|_| Err(bad("the video reader panicked on this file")))
}

pub(crate) fn io_error(error: std::io::Error) -> ReadError {
    ReadError::from_io(&error)
}

pub(crate) fn bad(message: impl Into<String>) -> ReadError {
    ReadError::Unreadable(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_containers_from_their_first_bytes() {
        assert_eq!(
            container_of(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81]),
            Some(Container::Matroska)
        );
        assert_eq!(
            container_of(b"\x00\x00\x00\x20ftypisom\x00\x00"),
            Some(Container::Mp4)
        );
        assert_eq!(container_of(b"\x00\x00\x00\x08wide"), Some(Container::Mp4));
        assert_eq!(container_of(b"RIFF\x00\x00\x00\x00AVI "), None);
        assert_eq!(container_of(b"\x1A\x45"), None);
    }

    #[test]
    fn non_video_files_are_unreadable_and_missing_files_temporary() {
        let directory =
            std::env::temp_dir().join(format!("mmp-video-reader-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let text = directory.join("notes.mkv");
        std::fs::write(&text, b"just some text, not a video").unwrap();
        assert!(matches!(read_video(&text), Err(ReadError::Unreadable(_))));

        // A truncated Matroska header is malformed, not a temporary failure.
        let truncated = directory.join("truncated.mkv");
        std::fs::write(&truncated, [0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42]).unwrap();
        assert!(matches!(
            read_video(&truncated),
            Err(ReadError::Unreadable(_))
        ));

        let missing = directory.join("missing.mkv");
        assert!(matches!(read_video(&missing), Err(ReadError::Temporary(_))));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
