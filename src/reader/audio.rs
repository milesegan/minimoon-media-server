//! The audio tag reader: a read-only copy of the reading side of the MiniMoon apps'
//! `minimoon_tags` crate (spec §8, "Tag reading: a copy, not a dependency").
//!
//! The server reads tags with the same code the apps use, so a library looks the same on every
//! device. Only reading is copied here: `read_tag` and its helpers, the native-text and sort-tag
//! reading, and embedded-picture reading. The crate's writing, tag-copying and C interface stay
//! out, because the server must never write to the files it shares.
//!
//! When the apps' reader changes, update this copy to match and bump [`READER_VERSION`], so every
//! indexed file is re-read.
//!
//! Differences from the apps' `read_tag`, all in what it does with a result rather than how it
//! reads tags:
//! - a file with no bitrate or sample rate is still read; those fields are left out rather than
//!   failing the whole read;
//! - duration is in milliseconds and the channel count is reported (spec §10, step 7);
//! - it reads embedded pictures in the same parse, to report `hasEmbeddedArt`.

use crate::model::{AudioInfo, AudioRead, AudioTags, Picture, ReadError};
use lofty::ape::ApeTag;
use lofty::config::{apply_global_options, GlobalOptions, ParseOptions, ParsingMode};
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::id3::v2::{Frame, Id3v2Tag};
use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4File};
use lofty::ogg::tag::VorbisComments;
use lofty::probe::Probe;
use lofty::properties::FileProperties;
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag as LoftyTag, TagType};
use std::borrow::Cow;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

/// The version of this reader. Bump it whenever a change could alter what it reads from an
/// already-indexed file, so the server re-reads every file (spec §8, index).
pub const READER_VERSION: u32 = 2;

/// Per-item allocation limit for tag work, in MiB, as in the apps' crate.
const LOFTY_SIZE_LIMIT: usize = 32usize;

/// Rating points per star on the apps' 0–255 scale, where a track shows `rating / 51` stars.
const RATING_POINTS_PER_STAR: u32 = 51;

/// The `ilst` atom an MP4 rating lives in, as text from 0 to 100.
const MP4_RATING: AtomIdent<'static> = AtomIdent::Fourcc(*b"rate");

/// Reads an audio file's properties and tags.
///
/// A file that can't be opened or read fails with [`ReadError::Temporary`]; one lofty can't
/// parse fails with [`ReadError::Unreadable`].
pub fn read_audio(path: &Path) -> Result<AudioRead, ReadError> {
    guarded(|| {
        require_file(path)?;
        let with_art = ParseOptions::new()
            .parsing_mode(ParsingMode::Relaxed)
            .read_cover_art(true);
        // A malformed picture shouldn't cost the whole read: the apps read tags with cover art
        // off, so fall back to that and report no art.
        let parts = match read_parts(path, with_art) {
            Ok(parts) => parts,
            Err(ReadError::Unreadable(_)) => read_parts(path, tag_options())?,
            Err(error) => return Err(error),
        };
        Ok(parts.into_read())
    })
}

/// Reads the first embedded picture, with its MIME type, or `None` when the file has none.
pub fn read_audio_art(path: &Path) -> Result<Option<Picture>, ReadError> {
    guarded(|| {
        require_file(path)?;
        let tagged_file = read_sniffed(path, None)?;
        let Some(tag) = tagged_file.tag(effective_tag_type(&tagged_file)) else {
            return Ok(None);
        };
        Ok(tag.pictures().first().map(|picture| {
            let data = picture.data().to_vec();
            let mime = picture_mime(&data, picture.mime_type().map(|mime| mime.as_str()));
            Picture { data, mime }
        }))
    })
}

/// Reads the embedded lyrics text, or `None` when the file has none.
pub fn read_audio_lyrics(path: &Path) -> Result<Option<String>, ReadError> {
    guarded(|| {
        require_file(path)?;
        let parts = read_parts(path, tag_options())?;
        Ok(lyrics(&parts.tag))
    })
}

/// The options the apps' `read_tag` reads with.
fn tag_options() -> ParseOptions {
    ParseOptions::new()
        .parsing_mode(ParsingMode::Relaxed)
        .read_cover_art(false)
}

/// Runs a read, turning a panic inside lofty into an unreadable file.
///
/// A malformed file can panic lofty; that must cost one file, not a server thread. This relies
/// on the unwind panic strategy.
fn guarded<T>(body: impl FnOnce() -> Result<T, ReadError>) -> Result<T, ReadError> {
    match panic::catch_unwind(AssertUnwindSafe(|| {
        apply_allocation_limit();
        body()
    })) {
        Ok(result) => result,
        Err(_) => Err(ReadError::Unreadable("the tag reader panicked".to_string())),
    }
}

/// Fails unless `path` is a regular file, classifying the failure.
fn require_file(path: &Path) -> Result<(), ReadError> {
    let metadata = fs::metadata(path).map_err(|error| ReadError::from_io(&error))?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(ReadError::Unreadable("not a regular file".to_string()))
    }
}

/// Raises the per-item allocation limit for tag work on the calling thread.
///
/// Lofty's global options are thread-local, so every entry point has to set this for itself.
/// A path that skipped it would inherit whatever limit the pool thread it happened to land on
/// last set, making large embedded cover art succeed or fail at random.
fn apply_allocation_limit() {
    apply_global_options(
        GlobalOptions::new().allocation_limit(LOFTY_SIZE_LIMIT.saturating_mul(1024 * 1024)),
    );
}

/// Classifies a lofty error: an I/O error anywhere in its chain is worth retrying, anything
/// else means lofty can't parse the file.
///
/// The exception is `InvalidInput`, which lofty gets from a seek to an offset the file's own
/// headers produced (a negative position, say): that's a malformed file, not a failing disk.
fn classify(error: &(dyn std::error::Error + 'static)) -> ReadError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::InvalidInput {
                return ReadError::Unreadable(io.to_string());
            }
            return ReadError::from_io(io);
        }
        source = current.source();
    }
    ReadError::Unreadable(error.to_string())
}

// MARK: - Reading (copied from minimoon_tags tag.rs)

/// A file read for tag work, with MP4 files kept in lofty's native MP4 form.
///
/// lofty's generic tag turns an MP4 `rate` atom into a five-star popularimeter on the way in, so
/// a rating of 50 reads as nothing. The rating is read from the native `ilst` instead.
///
/// Other files keep their native text tag, and the rating from their ID3v2 `POPM` frames, which
/// lofty's generic tag cannot carry exactly.
enum SniffedFile {
    Mp4(Mp4File),
    Other(TaggedFile, Option<NativeTextTag>, Option<u32>),
}

/// Reads a file, choosing the parser from its content rather than from its extension.
///
/// `Probe::open` picks a parser from the file extension alone, which mislabels any container
/// whose extension does not name the codec inside it: an Ogg Opus stream in a `.ogg` file goes
/// to the Vorbis parser and fails to read at all. Sniffing the leading bytes picks the real
/// type, and lofty falls back to the extension's guess when the content is unrecognizable.
fn read_sniffed(path: &Path, options: Option<ParseOptions>) -> Result<TaggedFile, ReadError> {
    match read_sniffed_native(path, options)? {
        SniffedFile::Mp4(file) => Ok(file.into()),
        SniffedFile::Other(file, _, _) => Ok(file),
    }
}

/// Reads a file the way `read_sniffed` does, keeping an MP4 file in its native form.
fn read_sniffed_native(
    path: &Path,
    options: Option<ParseOptions>,
) -> Result<SniffedFile, ReadError> {
    let options = options.unwrap_or_default();
    let probe = Probe::open(path)
        .map_err(|error| classify(&error))?
        .options(options)
        .guess_file_type()
        .map_err(|error| ReadError::from_io(&error))?;
    if probe.file_type() == Some(FileType::Mp4) {
        let mut reader = probe.into_inner();
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| ReadError::from_io(&error))?;
        return Mp4File::read_from(&mut reader, options)
            .map(SniffedFile::Mp4)
            .map_err(|error| classify(&error));
    }
    if let Some(
        kind @ (FileType::Flac
        | FileType::Vorbis
        | FileType::Opus
        | FileType::Speex
        | FileType::Ape
        | FileType::WavPack
        | FileType::Mpc),
    ) = probe.file_type()
    {
        let mut reader = probe.into_inner();
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| ReadError::from_io(&error))?;
        return read_text_file(&mut reader, kind, options)
            .map(|(file, native)| SniffedFile::Other(file, native, None));
    }
    if let Some(kind @ (FileType::Mpeg | FileType::Wav | FileType::Aiff | FileType::Aac)) =
        probe.file_type()
    {
        let mut reader = probe.into_inner();
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| ReadError::from_io(&error))?;
        return read_id3_file(&mut reader, kind, options)
            .map(|(file, native, rating)| SniffedFile::Other(file, native, rating));
    }
    probe
        .read()
        .map(|file| SniffedFile::Other(file, None, None))
        .map_err(|error| classify(&error))
}

/// The rating stored in an MP4 `rate` atom, read as the exact number it holds.
fn mp4_rating(ilst: &Ilst) -> Option<u32> {
    ilst.get(&MP4_RATING)?.data().find_map(|data| match data {
        AtomData::UTF8(text) | AtomData::UTF16(text) => text.trim().parse().ok(),
        AtomData::UnsignedInteger(value) => Some(*value),
        AtomData::SignedInteger(value) => u32::try_from(*value).ok(),
        _ => None,
    })
}

/// The one tag type that reads and writes both agree on for a file.
///
/// The apps write into this tag, so reading any other would show stale values: the format's
/// primary type when the file has such a tag, otherwise the type it is actually tagged with.
fn effective_tag_type(tagged_file: &TaggedFile) -> TagType {
    let primary_type = tagged_file.file_type().primary_tag_type();
    if tagged_file.tag(primary_type).is_some() {
        return primary_type;
    }
    tagged_file
        .first_tag()
        .map_or(primary_type, |tag| tag.tag_type())
}

fn string_default(s: Option<Cow<'_, str>>) -> Option<String> {
    match s.map(|f| f.trim().to_string()) {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s),
    }
}

fn get_tag_number(tag: &LoftyTag, key: ItemKey) -> Option<u32> {
    get_tag_string(tag, key)?.parse::<u32>().ok()
}

fn get_tag_string(tag: &LoftyTag, key: ItemKey) -> Option<String> {
    match tag.get_string(key).map(|f| f.trim().to_string()) {
        None => None,
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s),
    }
}

fn get_tag_flag(tag: &LoftyTag, key: ItemKey) -> bool {
    get_tag_number(tag, key).is_some_and(|value| value != 0)
}

/// One parse of a file: its chosen tag, audio properties, rating and native text fields.
struct Parts {
    tag: LoftyTag,
    properties: FileProperties,
    rating: Option<u32>,
    native: Option<NativeTextTag>,
}

/// Parses a file the way the apps' `read_tag` does.
///
/// An untagged file still has usable audio properties, so it gets an empty tag rather than
/// failing the whole read.
fn read_parts(path: &Path, options: ParseOptions) -> Result<Parts, ReadError> {
    Ok(match read_sniffed_native(path, Some(options))? {
        SniffedFile::Mp4(mut file) => {
            let properties = FileProperties::from(file.properties().clone());
            let ilst = file.remove_ilst();
            let rating = ilst.as_ref().and_then(mp4_rating);
            let tag = ilst.map_or_else(|| LoftyTag::new(TagType::Mp4Ilst), LoftyTag::from);
            Parts {
                tag,
                properties,
                rating,
                native: None,
            }
        }
        SniffedFile::Other(mut tagged_file, native, id3_rating) => {
            let properties = tagged_file.properties().clone();
            let tag = tagged_file
                .remove(effective_tag_type(&tagged_file))
                .unwrap_or_else(|| LoftyTag::new(tagged_file.file_type().primary_tag_type()));
            let rating = if tag.tag_type() == TagType::Id3v2 {
                id3_rating
            } else {
                get_tag_number(&tag, ItemKey::Popularimeter)
            };
            Parts {
                tag,
                properties,
                rating,
                native,
            }
        }
    })
}

impl Parts {
    /// Maps one parse onto MMP's audio schema (spec §5, audio `tags`).
    fn into_read(self) -> AudioRead {
        let tag = &self.tag;
        let [sort_artist, sort_album_artist, sort_album, _sort_title, _sort_composer] =
            read_sort_tags(tag, self.native.as_ref());
        let properties = &self.properties;
        AudioRead {
            audio: AudioInfo {
                duration_ms: positive(properties.duration().as_millis() as u64),
                sample_rate: properties.sample_rate().and_then(positive),
                bit_depth: properties.bit_depth().map(u32::from).and_then(positive),
                bitrate: properties.audio_bitrate().and_then(positive),
                channels: properties.channels().map(u32::from).and_then(positive),
            },
            tags: AudioTags {
                title: get_tag_string(tag, ItemKey::TrackTitle),
                artist: get_tag_string(tag, ItemKey::TrackArtist),
                album_artist: get_tag_string(tag, ItemKey::AlbumArtist),
                album: get_tag_string(tag, ItemKey::AlbumTitle),
                genre: string_default(tag.genre()),
                work: get_tag_string(tag, ItemKey::Work),
                sort_artist,
                sort_album_artist,
                sort_album,
                replaygain_track_gain: get_tag_string(tag, ItemKey::ReplayGainTrackGain),
                replaygain_album_gain: get_tag_string(tag, ItemKey::ReplayGainAlbumGain),
                track_number: tag.track(),
                track_number_total: tag.track_total(),
                disk_number: tag.disk(),
                disk_number_total: tag.disk_total(),
                year: get_tag_number(tag, ItemKey::Year),
                date_year: tag.date().map(|date| u32::from(date.year)),
                rating: self.rating,
                compilation: get_tag_flag(tag, ItemKey::FlagCompilation),
                has_lyrics: lyrics(tag).is_some(),
                has_embedded_art: !tag.pictures().is_empty(),
            },
        }
    }
}

/// A tag's lyrics. lofty reads an ID3v2 `USLT` frame, and Vorbis `UNSYNCEDLYRICS`, as
/// `ItemKey::UnsyncLyrics` rather than `ItemKey::Lyrics`.
fn lyrics(tag: &LoftyTag) -> Option<String> {
    get_tag_string(tag, ItemKey::Lyrics).or_else(|| get_tag_string(tag, ItemKey::UnsyncLyrics))
}

/// `None` for zero, which lofty reports when it doesn't know a property.
fn positive<T: Default + PartialEq>(value: T) -> Option<T> {
    (value != T::default()).then_some(value)
}

/// The MIME type to serve a picture with.
///
/// The bytes decide when they're a recognizable image, because tags often carry a wrong or
/// non-standard type (`image/jpg`); otherwise the tag's own type, otherwise a generic one.
fn picture_mime(data: &[u8], tagged: Option<&str>) -> String {
    if let Some(sniffed) = sniff_image(data) {
        return sniffed.to_string();
    }
    match tagged.map(str::trim) {
        Some(mime) if !mime.is_empty() => mime.to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// The image type named by a picture's leading bytes.
fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if data.starts_with(b"BM") {
        Some("image/bmp")
    } else if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        Some("image/tiff")
    } else {
        None
    }
}

// MARK: - Native text fields (copied from minimoon_tags native_text.rs, reading side)

/// Retains unmapped text fields that Lofty's generic Vorbis/APE conversion discards.
enum NativeTextTag {
    Vorbis(VorbisComments),
    Ape(ApeTag),
}

impl NativeTextTag {
    fn tag_type(&self) -> TagType {
        match self {
            Self::Vorbis(_) => TagType::VorbisComments,
            Self::Ape(_) => TagType::Ape,
        }
    }

    fn values(&self) -> Vec<(String, String)> {
        match self {
            Self::Vorbis(tag) => tag
                .items()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            Self::Ape(tag) => tag
                .into_iter()
                .filter_map(|item| {
                    if let ItemValue::Text(value) = item.value() {
                        Some((item.key().to_owned(), value.clone()))
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }
}

/// Reads text-tag formats natively before generic conversion loses their custom fields.
fn read_text_file<R: Read + Seek>(
    reader: &mut R,
    kind: FileType,
    options: ParseOptions,
) -> Result<(TaggedFile, Option<NativeTextTag>), ReadError> {
    let parse_error = |error: lofty::error::FileParseError| classify(&error);
    match kind {
        FileType::Flac => {
            let file = lofty::flac::FlacFile::read_from(reader, options).map_err(parse_error)?;
            let native = file.vorbis_comments().cloned().map(NativeTextTag::Vorbis);
            Ok((file.into(), native))
        }
        FileType::Vorbis => {
            let file = lofty::ogg::VorbisFile::read_from(reader, options).map_err(parse_error)?;
            let native = NativeTextTag::Vorbis(file.vorbis_comments().clone());
            Ok((file.into(), Some(native)))
        }
        FileType::Opus => {
            let file = lofty::ogg::OpusFile::read_from(reader, options).map_err(parse_error)?;
            let native = NativeTextTag::Vorbis(file.vorbis_comments().clone());
            Ok((file.into(), Some(native)))
        }
        FileType::Speex => {
            let file = lofty::ogg::SpeexFile::read_from(reader, options).map_err(parse_error)?;
            let native = NativeTextTag::Vorbis(file.vorbis_comments().clone());
            Ok((file.into(), Some(native)))
        }
        FileType::Ape => {
            let file = lofty::ape::ApeFile::read_from(reader, options).map_err(parse_error)?;
            let native = file.ape().cloned().map(NativeTextTag::Ape);
            Ok((file.into(), native))
        }
        FileType::WavPack => {
            let file =
                lofty::wavpack::WavPackFile::read_from(reader, options).map_err(parse_error)?;
            let native = file.ape().cloned().map(NativeTextTag::Ape);
            Ok((file.into(), native))
        }
        FileType::Mpc => {
            let file = lofty::musepack::MpcFile::read_from(reader, options).map_err(parse_error)?;
            let native = file.ape().cloned().map(NativeTextTag::Ape);
            Ok((file.into(), native))
        }
        _ => Err(ReadError::Unreadable(format!(
            "{kind:?} has no native text reader"
        ))),
    }
}

// MARK: - ID3v2 (copied from minimoon_tags id3.rs)

/// The rating held in an ID3v2 tag's `POPM` frames, on the apps' 0–255 scale.
///
/// lofty's generic tag turns a `POPM` frame into `email|stars|counter` text, which the rating's
/// number parse rejects, and which reads a byte of 0 ("unrated") the same as one star. So the
/// rating comes from the native frame instead.
///
/// Taggers disagree on what byte a star count gets: Windows Media Player and MusicBee write 1,
/// 64, 128, 196 and 255, while Picard and MiniMoon's own scale use multiples of 51. Rounding the
/// byte up to the next multiple of 51 gives every one of those the star count its tagger meant.
/// The first frame with a rating wins.
fn popm_rating(tag: &Id3v2Tag) -> Option<u32> {
    tag.into_iter().find_map(|frame| match frame {
        Frame::Popularimeter(popm) if popm.rating > 0 => {
            Some(u32::from(popm.rating).div_ceil(RATING_POINTS_PER_STAR) * RATING_POINTS_PER_STAR)
        }
        _ => None,
    })
}

/// Reads a file that can carry an ID3v2 tag, keeping what its generic conversion loses.
///
/// Returns the file, its native APE tag where the format can carry one, and the rating from its
/// ID3v2 `POPM` frames.
fn read_id3_file<R: Read + Seek>(
    reader: &mut R,
    kind: FileType,
    options: ParseOptions,
) -> Result<(TaggedFile, Option<NativeTextTag>, Option<u32>), ReadError> {
    let parse_error = |error: lofty::error::FileParseError| classify(&error);
    match kind {
        FileType::Mpeg => {
            let file = lofty::mpeg::MpegFile::read_from(reader, options).map_err(parse_error)?;
            let native = file.ape().cloned().map(NativeTextTag::Ape);
            let rating = file.id3v2().and_then(popm_rating);
            Ok((file.into(), native, rating))
        }
        FileType::Wav => {
            let file = lofty::iff::wav::WavFile::read_from(reader, options).map_err(parse_error)?;
            let rating = file.id3v2().and_then(popm_rating);
            Ok((file.into(), None, rating))
        }
        FileType::Aiff => {
            let file =
                lofty::iff::aiff::AiffFile::read_from(reader, options).map_err(parse_error)?;
            let rating = file.id3v2().and_then(popm_rating);
            Ok((file.into(), None, rating))
        }
        FileType::Aac => {
            let file = lofty::aac::AacFile::read_from(reader, options).map_err(parse_error)?;
            let rating = file.id3v2().and_then(popm_rating);
            Ok((file.into(), None, rating))
        }
        _ => Err(ReadError::Unreadable(format!(
            "{kind:?} has no ID3v2 reader"
        ))),
    }
}

// MARK: - Sort tags (copied from minimoon_tags sort.rs, reading side)

/// A sort override, kept as one string regardless of punctuation or credit splitting.
#[derive(Clone, Copy)]
enum SortField {
    Artist,
    AlbumArtist,
    Album,
    Title,
    Composer,
}

impl SortField {
    const ALL: [Self; 5] = [
        Self::Artist,
        Self::AlbumArtist,
        Self::Album,
        Self::Title,
        Self::Composer,
    ];

    fn key(self) -> ItemKey {
        match self {
            Self::Artist => ItemKey::TrackArtistSortOrder,
            Self::AlbumArtist => ItemKey::AlbumArtistSortOrder,
            Self::Album => ItemKey::AlbumTitleSortOrder,
            Self::Title => ItemKey::TrackTitleSortOrder,
            Self::Composer => ItemKey::ComposerSortOrder,
        }
    }

    fn names(self) -> [&'static str; 2] {
        match self {
            Self::Artist => ["ARTISTSORT", "SORTARTIST"],
            Self::AlbumArtist => ["ALBUMARTISTSORT", "SORTALBUMARTIST"],
            Self::Album => ["ALBUMSORT", "SORTALBUM"],
            Self::Title => ["TITLESORT", "SORTTITLE"],
            Self::Composer => ["COMPOSERSORT", "SORTCOMPOSER"],
        }
    }
}

fn nonblank(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Reads native fields before canonical user text and aliases, including unmapped companion items.
fn read_sort_tags(tag: &LoftyTag, native: Option<&NativeTextTag>) -> [Option<String>; 5] {
    let text: Vec<(String, String)> = match tag.tag_type() {
        TagType::VorbisComments | TagType::Ape => native
            .filter(|n| n.tag_type() == tag.tag_type())
            .map(NativeTextTag::values)
            .unwrap_or_default(),
        TagType::Id3v2 => (&Id3v2Tag::from(tag.clone()))
            .into_iter()
            .filter_map(|frame| {
                if let Frame::UserText(text) = frame {
                    Some((text.description.to_string(), text.content.to_string()))
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    SortField::ALL.map(|field| {
        tag.get_strings(field.key()).find_map(nonblank).or_else(|| {
            field.names().iter().find_map(|name| {
                text.iter()
                    .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                    .find_map(|(_, value)| nonblank(value))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::config::WriteOptions;
    use lofty::id3::v2::UnsynchronizedTextFrame;
    use lofty::mp4::Atom;
    use lofty::picture::{MimeType, PictureType};
    use lofty::tag::TagExt;
    use lofty::TextEncoding;
    use std::path::PathBuf;

    /// A temporary directory removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("mmp-audio-{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// One second of silent 16-bit stereo PCM at 44.1 kHz, as a WAV file.
    fn silent_wav() -> Vec<u8> {
        let sample_rate: u32 = 44_100;
        let channels: u16 = 2;
        let bits: u16 = 16;
        let data_len = sample_rate * u32::from(channels) * u32::from(bits / 8);
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(
            &(sample_rate * u32::from(channels) * u32::from(bits / 8)).to_le_bytes(),
        );
        wav.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.resize(wav.len() + data_len as usize, 0);
        wav
    }

    const PNG_PIXEL: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\x0f\0\0\x01\x01\0\x05\x18\xd8N\0\0\0\0IEND\xaeB`\x82";

    #[test]
    fn reads_properties_and_id3_tags_from_a_wav() {
        let dir = TempDir::new("wav");
        let path = dir.0.join("song.wav");
        fs::write(&path, silent_wav()).unwrap();

        let untagged = read_audio(&path).unwrap();
        assert_eq!(untagged.audio.sample_rate, Some(44_100));
        assert_eq!(untagged.audio.channels, Some(2));
        assert_eq!(untagged.audio.bit_depth, Some(16));
        assert_eq!(untagged.audio.duration_ms, Some(1000));
        assert_eq!(untagged.tags, AudioTags::default());

        let mut id3 = Id3v2Tag::new();
        id3.set_title("  So What  ".to_string());
        id3.set_artist("Miles Davis".to_string());
        id3.set_album("Kind of Blue".to_string());
        id3.set_track(1);
        id3.set_track_total(5);
        id3.insert_user_text("ALBUMARTISTSORT".to_string(), "Davis, Miles".to_string());
        let mut tag: LoftyTag = id3.into();
        // ID3v2.4 has no year frame of its own, so the year arrives as the recording date's.
        tag.insert_text(ItemKey::RecordingDate, "1959-03-02".to_string());
        tag.push_picture(
            lofty::picture::Picture::unchecked(PNG_PIXEL.to_vec())
                .pic_type(PictureType::CoverFront)
                .build(),
        );
        // Lyrics need a language, which only the native frame can carry. lofty reads the `USLT`
        // frame as `ItemKey::UnsyncLyrics`.
        let mut id3 = Id3v2Tag::from(tag);
        id3.insert(Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
            TextEncoding::UTF8,
            *b"eng",
            "",
            "So what",
        )));
        id3.save_to_path(&path, WriteOptions::new()).unwrap();

        let read = read_audio(&path).unwrap();
        assert_eq!(read.tags.title.as_deref(), Some("So What"));
        assert_eq!(read.tags.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(read.tags.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(read.tags.track_number, Some(1));
        assert_eq!(read.tags.track_number_total, Some(5));
        assert_eq!(read.tags.date_year, Some(1959));
        assert_eq!(read.tags.sort_album_artist.as_deref(), Some("Davis, Miles"));
        assert!(read.tags.has_lyrics);
        assert!(read.tags.has_embedded_art);
        assert!(!read.tags.compilation);

        assert_eq!(
            read_audio_lyrics(&path).unwrap().as_deref(),
            Some("So what")
        );
        let art = read_audio_art(&path).unwrap().unwrap();
        assert_eq!(art.data, PNG_PIXEL);
        assert_eq!(art.mime, "image/png");
    }

    /// A copy of a fixture from `tests/fixtures`, in `dir`.
    fn fixture(dir: &TempDir, name: &str) -> PathBuf {
        let path = dir.0.join(name);
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
            &path,
        )
        .unwrap();
        path
    }

    fn save_id3(path: &Path, frames: Vec<Frame<'static>>) {
        let mut id3 = Id3v2Tag::new();
        for frame in frames {
            id3.insert(frame);
        }
        id3.save_to_path(path, WriteOptions::new()).unwrap();
    }

    fn popm(email: &str, rating: u8, counter: u64) -> Frame<'static> {
        Frame::Popularimeter(lofty::id3::v2::PopularimeterFrame::new(
            email.to_owned(),
            rating,
            counter,
        ))
    }

    /// lofty shows a `POPM` frame to the generic tag as `email|stars|counter` text, which the
    /// number parse rejected, so no MP3 or WAV rating ever read. The byte comes through on the
    /// apps' 0–255 scale, as the star count its tagger meant, with 0 meaning unrated.
    #[test]
    fn id3_popm_ratings_read_on_the_apps_scale() {
        let dir = TempDir::new("popm");
        let wav = dir.0.join("song.wav");
        fs::write(&wav, silent_wav()).unwrap();
        for path in [fixture(&dir, "untagged.mp3"), wav] {
            for (email, byte, expected) in [
                ("Windows Media Player 9 Series", 196, Some(204)),
                ("Windows Media Player 9 Series", 1, Some(51)),
                ("users@musicbrainz.org", 102, Some(102)),
                ("MusicBee", 255, Some(255)),
                ("Windows Media Player 9 Series", 0, None),
            ] {
                save_id3(&path, vec![popm(email, byte, 7)]);
                assert_eq!(
                    read_audio(&path).unwrap().tags.rating,
                    expected,
                    "{} {email} {byte}",
                    path.display()
                );
            }
            // The first frame that holds a rating wins.
            save_id3(
                &path,
                vec![popm("a@example.com", 0, 3), popm("b@example.com", 128, 0)],
            );
            assert_eq!(read_audio(&path).unwrap().tags.rating, Some(153));
        }
    }

    /// Lyrics stored as unsynced lyrics are found: an MP3's `USLT` frame and a FLAC's
    /// `UNSYNCEDLYRICS` comment.
    #[test]
    fn unsynced_lyrics_are_found() {
        let dir = TempDir::new("lyrics");
        let mp3 = fixture(&dir, "untagged.mp3");
        save_id3(
            &mp3,
            vec![Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
                TextEncoding::UTF8,
                *b"eng",
                "",
                "Line one\nLine two".to_owned(),
            ))],
        );
        let flac = fixture(&dir, "sinewave.flac");
        let mut vorbis = VorbisComments::default();
        vorbis.insert("UNSYNCEDLYRICS".into(), "Line one\nLine two".into());
        vorbis.save_to_path(&flac, WriteOptions::new()).unwrap();
        for path in [mp3, flac] {
            assert!(
                read_audio(&path).unwrap().tags.has_lyrics,
                "{}",
                path.display()
            );
            assert_eq!(
                read_audio_lyrics(&path).unwrap().as_deref(),
                Some("Line one\nLine two"),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn missing_file_is_temporary_and_garbage_is_unreadable() {
        let dir = TempDir::new("errors");
        assert!(matches!(
            read_audio(&dir.0.join("missing.mp3")),
            Err(ReadError::Temporary(_))
        ));
        let garbage = dir.0.join("garbage.flac");
        fs::write(&garbage, b"this is not a flac file at all").unwrap();
        assert!(matches!(
            read_audio(&garbage),
            Err(ReadError::Unreadable(_))
        ));
        assert!(matches!(read_audio(&dir.0), Err(ReadError::Unreadable(_))));
    }

    #[test]
    fn io_errors_classify_by_kind() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(classify(&denied), ReadError::Temporary(_)));
        let bad_seek = std::io::Error::from(std::io::ErrorKind::InvalidInput);
        assert!(matches!(classify(&bad_seek), ReadError::Unreadable(_)));
        let eof = std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        assert!(matches!(classify(&eof), ReadError::Unreadable(_)));
    }

    #[test]
    fn mp4_rating_reads_the_raw_number() {
        let mut ilst = Ilst::default();
        ilst.replace_atom(Atom::new(MP4_RATING, AtomData::UTF8(" 50 ".to_string())));
        assert_eq!(mp4_rating(&ilst), Some(50));
        ilst.replace_atom(Atom::new(MP4_RATING, AtomData::UnsignedInteger(80)));
        assert_eq!(mp4_rating(&ilst), Some(80));
        assert_eq!(mp4_rating(&Ilst::default()), None);
    }

    #[test]
    fn blank_and_non_numeric_values_are_absent() {
        let mut tag = LoftyTag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::TrackTitle, "   ".to_string());
        tag.insert_text(ItemKey::Year, "19xx".to_string());
        tag.insert_text(ItemKey::FlagCompilation, "1".to_string());
        assert_eq!(get_tag_string(&tag, ItemKey::TrackTitle), None);
        assert_eq!(get_tag_number(&tag, ItemKey::Year), None);
        assert!(get_tag_flag(&tag, ItemKey::FlagCompilation));
        assert_eq!(
            string_default(Some(Cow::Borrowed("  Jazz "))),
            Some("Jazz".to_string())
        );
        assert_eq!(string_default(Some(Cow::Borrowed(" "))), None);
    }

    #[test]
    fn sort_tags_prefer_native_then_canonical_then_alias() {
        let mut vorbis = VorbisComments::default();
        vorbis.insert("SORTALBUM".to_string(), "Alias".to_string());
        vorbis.insert("ALBUMSORT".to_string(), "Canonical".to_string());
        let tag: LoftyTag = vorbis.clone().into();
        let sorts = read_sort_tags(&tag, Some(&NativeTextTag::Vorbis(vorbis)));
        assert_eq!(sorts[2].as_deref(), Some("Canonical"));

        let mut id3 = Id3v2Tag::new();
        id3.insert_user_text("sortartist".to_string(), "Alias".to_string());
        let tag: LoftyTag = id3.into();
        assert_eq!(read_sort_tags(&tag, None)[0].as_deref(), Some("Alias"));
    }

    #[test]
    fn picture_mime_trusts_bytes_over_tags() {
        assert_eq!(picture_mime(PNG_PIXEL, Some("image/jpg")), "image/png");
        assert_eq!(picture_mime(&[0xFF, 0xD8, 0xFF, 0xE0], None), "image/jpeg");
        assert_eq!(picture_mime(b"????", Some("image/x-odd")), "image/x-odd");
        assert_eq!(picture_mime(b"????", Some(" ")), "application/octet-stream");
        assert_eq!(MimeType::Png.as_str(), "image/png");
    }
}
