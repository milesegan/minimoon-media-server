//! Matroska (and WebM): stream facts through `matroska-demuxer`, plus hand-written EBML reading
//! for what it doesn't expose: Dolby Vision `BlockAdditionMapping`s and attachment headers.

use super::codec::{
    aac_channels, av1_transfer, avc_transfer, has_hdr10_plus, hevc_config, image_mime,
    leading_year, nonblank, round_frame_rate, snap_frame_rate, transfer_name, DolbyVision,
};
use super::{bad, io_error, MAX_STRUCTURE_BYTES};
use crate::model::{Picture, ReadError, Stream, VideoInfo, VideoRead, VideoTags};
use matroska_demuxer::{
    ContentCompAlgo, ContentEncodingValue, MatroskaFile, TrackEntry, TrackType,
    TransferCharacteristics,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};

const ID_EBML: u64 = 0x1A45_DFA3;
const ID_SEGMENT: u64 = 0x1853_8067;
const ID_CLUSTER: u64 = 0x1F43_B675;
const ID_SEEK_HEAD: u64 = 0x114D_9B74;
const ID_SEEK: u64 = 0x4DBB;
const ID_SEEK_ID: u64 = 0x53AB;
const ID_SEEK_POSITION: u64 = 0x53AC;
const ID_TRACKS: u64 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u64 = 0xAE;
const ID_TRACK_NUMBER: u64 = 0xD7;
const ID_BLOCK_ADDITION_MAPPING: u64 = 0x41E4;
const ID_BLOCK_ADD_ID_TYPE: u64 = 0x41E7;
const ID_BLOCK_ADD_ID_EXTRA_DATA: u64 = 0x41ED;
const ID_ATTACHMENTS: u64 = 0x1941_A469;
const ID_ATTACHED_FILE: u64 = 0x61A7;
const ID_FILE_NAME: u64 = 0x466E;
const ID_FILE_MEDIA_TYPE: u64 = 0x4660;
const ID_FILE_DATA: u64 = 0x465C;

/// The most frames read looking for the first video frame and frame-rate timing.
const MAX_FRAMES_SCANNED: usize = 400;
/// Frames timed when a video track has no `DefaultDuration`.
const FRAMES_TIMED: usize = 25;

/// Reads a Matroska file's streams and tags.
pub(super) fn read(path: &std::path::Path) -> Result<VideoRead, ReadError> {
    let mut file = open(path)?;
    let layout = Layout::read(&mut file)?;
    let dolby_vision = match layout.tracks {
        Some(tracks) => dolby_vision_by_track(&tracks.read_body(&mut file)?),
        None => HashMap::new(),
    };

    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut matroska = MatroskaFile::open(file).map_err(demux_error)?;
    let timestamp_scale = matroska.info().timestamp_scale().get() as f64;
    let duration_ms = matroska
        .info()
        .duration()
        .filter(|duration| duration.is_finite() && *duration >= 0.0)
        .map(|duration| (duration * timestamp_scale / 1e6).round() as u64);

    let mut streams = Vec::new();
    let mut hevc_track: Option<(u64, usize, usize)> = None; // (track, nal size, stream index)
    let mut untimed_track: Option<u64> = None;
    let mut header_stripping: HashMap<u64, Vec<u8>> = HashMap::new();
    for track in matroska.tracks() {
        let number = track.track_number().get();
        let mut stream = Stream::new(track_kind(track.track_type()));
        stream.track_number = Some(number);
        stream.codec = Some(codec_name(track.codec_id()).to_string());
        // An absent Language element means "eng" (Matroska spec); LanguageBCP47 wins over it.
        stream.language = Some(
            track
                .language_bcp47()
                .or(track.language())
                .unwrap_or("eng")
                .to_string(),
        );
        stream.title = track.name().and_then(nonblank);
        stream.default = Some(track.flag_default());
        stream.forced = Some(track.flag_forced());
        if let Some(prefix) = stripped_header(track) {
            header_stripping.insert(number, prefix);
        }
        match track.track_type() {
            TrackType::Video => {
                let nal_length_size =
                    read_video_track(track, dolby_vision.get(&number), &mut stream);
                if let (None, Some(size)) = (hevc_track, nal_length_size) {
                    hevc_track = Some((number, size, streams.len()));
                }
                if stream.frame_rate.is_none() && untimed_track.is_none() {
                    untimed_track = Some(number);
                }
            }
            TrackType::Audio => {
                let container_channels = track.audio().map(|audio| audio.channels().get() as u32);
                stream.channels = if track.codec_id().starts_with("A_AAC") {
                    aac_channels(
                        track.codec_private(),
                        container_channels,
                        track.codec_id().contains("SBR"),
                    )
                } else {
                    container_channels
                };
            }
            _ => {}
        }
        streams.push(stream);
    }

    // The first frames: HDR10+ lives only in the first HEVC frame's SEI, and a track without a
    // DefaultDuration is timed from its first frames.
    if hevc_track.is_some() || untimed_track.is_some() {
        let first = scan_frames(
            &mut matroska,
            hevc_track.map(|(track, ..)| track),
            untimed_track,
            &header_stripping,
            timestamp_scale,
        );
        if let (Some((_, nal_length_size, index)), Some(frame)) = (hevc_track, first.hevc_frame) {
            if has_hdr10_plus(&frame, nal_length_size) {
                streams[index].hdr10_plus = Some(true);
            }
        }
        if let (Some(track), Some(rate)) = (untimed_track, first.frame_rate) {
            if let Some(stream) = streams
                .iter_mut()
                .find(|stream| stream.track_number == Some(track))
            {
                stream.frame_rate = Some(rate);
            }
        }
    }

    Ok(VideoRead {
        video: VideoInfo {
            duration_ms,
            container: "matroska".to_string(),
            streams,
        },
        tags: tags(&matroska),
    })
}

/// The first cover attachment: one named `cover…` if there is one, else the first image.
pub(super) fn read_art(path: &std::path::Path) -> Result<Option<Picture>, ReadError> {
    let mut file = open(path)?;
    let layout = Layout::read(&mut file)?;
    let Some(attachments) = layout.attachments else {
        return Ok(None);
    };
    let headers = attachment_headers(&mut file, attachments)?;
    let is_image = |header: &&AttachmentHeader| header.mime.starts_with("image/");
    let chosen = headers
        .iter()
        .filter(is_image)
        .find(|header| header.name.to_ascii_lowercase().starts_with("cover"))
        .or_else(|| headers.iter().find(is_image));
    let Some(header) = chosen else {
        return Ok(None);
    };
    let data = header.data.read_body(&mut file)?;
    let mime = image_mime(&data)
        .map(str::to_string)
        .unwrap_or_else(|| header.mime.clone());
    Ok(Some(Picture { data, mime }))
}

fn open(path: &std::path::Path) -> Result<BufReader<File>, ReadError> {
    File::open(path)
        .map(|file| BufReader::with_capacity(64 * 1024, file))
        .map_err(io_error)
}

fn demux_error(error: matroska_demuxer::DemuxError) -> ReadError {
    match error {
        matroska_demuxer::DemuxError::IoError(error) => io_error(error),
        other => bad(format!("matroska: {other}")),
    }
}

fn track_kind(track_type: TrackType) -> String {
    match track_type {
        TrackType::Video => "video".into(),
        TrackType::Audio => "audio".into(),
        TrackType::Subtitle => "subtitle".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Matroska codec IDs to the names the wire schema uses (FFmpeg's codec names).
fn codec_name(codec_id: &str) -> &str {
    match codec_id {
        "V_MPEGH/ISO/HEVC" => "hevc",
        "V_MPEG4/ISO/AVC" => "h264",
        "V_AV1" => "av1",
        "V_VP9" => "vp9",
        "V_VP8" => "vp8",
        "V_MPEG2" => "mpeg2video",
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" => "mpeg4",
        "A_EAC3" => "eac3",
        "A_AC3" => "ac3",
        "A_OPUS" => "opus",
        "A_FLAC" => "flac",
        "A_VORBIS" => "vorbis",
        "A_DTS" => "dts",
        "A_TRUEHD" => "truehd",
        "A_MPEG/L3" => "mp3",
        "A_MPEG/L2" => "mp2",
        "S_TEXT/UTF8" => "subrip",
        "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA" => "ass",
        "S_TEXT/WEBVTT" => "webvtt",
        "S_HDMV/PGS" => "hdmv_pgs_subtitle",
        "S_VOBSUB" => "dvd_subtitle",
        id if id.starts_with("A_AAC") => "aac",
        id if id.starts_with("A_PCM") => "pcm",
        id => id,
    }
}

/// Fills a video stream's size, frame rate and colour facts. Returns the NAL length size when
/// the track is HEVC, for the HDR10+ check.
fn read_video_track(
    track: &TrackEntry,
    dolby_vision: Option<&DolbyVision>,
    stream: &mut Stream,
) -> Option<usize> {
    if let Some(video) = track.video() {
        stream.width = u32::try_from(video.pixel_width().get()).ok();
        stream.height = u32::try_from(video.pixel_height().get()).ok();
    }
    stream.frame_rate = track
        .default_duration()
        .and_then(|duration| round_frame_rate(1e9 / duration.get() as f64));

    let private = track.codec_private().unwrap_or(&[]);
    let (transfer, nal_length_size) = match track.codec_id() {
        "V_MPEGH/ISO/HEVC" => {
            let (transfer, size) = hevc_config(private);
            (transfer, Some(size))
        }
        "V_MPEG4/ISO/AVC" => (avc_transfer(private), None),
        "V_AV1" => (av1_transfer(private), None),
        _ => (None, None),
    };
    // Codec headers first; a Matroska Colour element only when they say nothing (none of 106
    // real files had one).
    stream.transfer = transfer
        .and_then(transfer_name)
        .or_else(|| colour_transfer(track))
        .map(str::to_string);
    if let Some(dolby_vision) = dolby_vision {
        stream.dolby_vision_profile = Some(dolby_vision.profile);
        if stream.transfer.is_none() && dolby_vision.implies_pq() {
            stream.transfer = Some("pq".to_string());
        }
    }
    nal_length_size
}

fn colour_transfer(track: &TrackEntry) -> Option<&'static str> {
    match track.video()?.colour()?.transfer_characteristics()? {
        TransferCharacteristics::Unknown => None,
        TransferCharacteristics::Bt2100 => Some("pq"),
        TransferCharacteristics::Hlg => Some("hlg"),
        _ => Some("sdr"),
    }
}

/// The bytes header-stripping compression removed from every frame of a track.
fn stripped_header(track: &TrackEntry) -> Option<Vec<u8>> {
    track
        .content_encodings()?
        .iter()
        .find_map(|encoding| match encoding.encoding() {
            ContentEncodingValue::Compression(compression)
                if matches!(compression.algo(), ContentCompAlgo::Stripping) =>
            {
                compression.settings().map(<[u8]>::to_vec)
            }
            _ => None,
        })
}

/// What the first frames showed.
struct FirstFrames {
    hevc_frame: Option<Vec<u8>>,
    frame_rate: Option<f64>,
}

fn scan_frames<R: Read + Seek>(
    matroska: &mut MatroskaFile<R>,
    hevc_track: Option<u64>,
    untimed_track: Option<u64>,
    header_stripping: &HashMap<u64, Vec<u8>>,
    timestamp_scale: f64,
) -> FirstFrames {
    let mut frame = matroska_demuxer::Frame::default();
    let mut hevc_frame = None;
    let mut timestamps = Vec::new();
    for _ in 0..MAX_FRAMES_SCANNED {
        let hevc_done = hevc_track.is_none() || hevc_frame.is_some();
        let timing_done = untimed_track.is_none() || timestamps.len() >= FRAMES_TIMED;
        if hevc_done && timing_done {
            break;
        }
        match matroska.next_frame(&mut frame) {
            Ok(true) => {}
            _ => break,
        }
        if Some(frame.track) == hevc_track && hevc_frame.is_none() {
            if frame.data.len() > MAX_STRUCTURE_BYTES {
                break;
            }
            let mut data = header_stripping
                .get(&frame.track)
                .cloned()
                .unwrap_or_default();
            data.extend_from_slice(&frame.data);
            hevc_frame = Some(data);
        }
        if Some(frame.track) == untimed_track && timestamps.len() < FRAMES_TIMED {
            timestamps.push(frame.timestamp);
        }
    }
    timestamps.sort_unstable();
    let frame_rate = match (timestamps.first(), timestamps.last()) {
        (Some(first), Some(last)) if timestamps.len() >= 2 && last > first => {
            let seconds = (last - first) as f64 * timestamp_scale / 1e9;
            round_frame_rate(snap_frame_rate((timestamps.len() - 1) as f64 / seconds))
        }
        _ => None,
    };
    FirstFrames {
        hevc_frame,
        frame_rate,
    }
}

/// Video tags: the segment title, then the global tags (those not aimed at one track).
///
/// TV metadata follows the Matroska tagging conventions: `TITLE` at target type 70
/// (collection) is the show, `PART_NUMBER` at 60 (season) the season and at 50 (episode) the
/// episode. Tags without a target type count as 50.
fn tags<R: Read + Seek>(matroska: &MatroskaFile<R>) -> VideoTags {
    let mut tags = VideoTags {
        title: matroska.info().title().and_then(nonblank),
        ..VideoTags::default()
    };
    let mut released = None;
    let mut recorded = None;
    for tag in matroska.tags().unwrap_or(&[]) {
        let targets = tag.targets();
        if targets.and_then(|t| t.tag_track_uid()).unwrap_or(0) != 0 {
            continue; // per-track tags, such as mkvmerge's statistics
        }
        let target = targets.and_then(|t| t.target_type_value()).unwrap_or(50);
        for simple in tag.simple_tags() {
            let Some(value) = simple.string().and_then(nonblank) else {
                continue;
            };
            let number = || value.parse::<u32>().ok();
            match (simple.name().to_ascii_uppercase().as_str(), target) {
                ("TITLE", 70) => {
                    tags.show.get_or_insert(value);
                }
                ("TITLE", 50) => {
                    tags.title.get_or_insert(value);
                }
                ("PART_NUMBER", 60) => tags.season = tags.season.or_else(number),
                ("PART_NUMBER", 50) => tags.episode = tags.episode.or_else(number),
                ("DATE_RELEASED", _) => released = released.or_else(|| leading_year(&value)),
                ("DATE_RECORDED", _) => recorded = recorded.or_else(|| leading_year(&value)),
                ("DESCRIPTION" | "SYNOPSIS" | "SUMMARY", _) => {
                    tags.description.get_or_insert(value);
                }
                _ => {}
            }
        }
    }
    tags.year = released.or(recorded);
    tags
}

/// An element's body: where it starts and how long it is.
#[derive(Clone, Copy, Debug)]
struct Element {
    start: u64,
    size: u64,
}

impl Element {
    fn read_body<R: Read + Seek>(&self, reader: &mut R) -> Result<Vec<u8>, ReadError> {
        if self.size > MAX_STRUCTURE_BYTES as u64 {
            return Err(bad(format!(
                "a {} byte Matroska element exceeds the reader's limit",
                self.size
            )));
        }
        reader.seek(SeekFrom::Start(self.start)).map_err(io_error)?;
        let mut body = vec![0u8; self.size as usize];
        reader.read_exact(&mut body).map_err(io_error)?;
        Ok(body)
    }
}

/// Where the top-level elements we read by hand are.
#[derive(Debug, Default)]
struct Layout {
    tracks: Option<Element>,
    attachments: Option<Element>,
}

impl Layout {
    /// Walks the top level of the segment up to the first Cluster, then follows the SeekHead to
    /// Tracks or Attachments if they come later in the file.
    fn read<R: Read + Seek>(reader: &mut R) -> Result<Self, ReadError> {
        let file_length = reader.seek(SeekFrom::End(0)).map_err(io_error)?;
        reader.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let (id, _) = read_vint(reader, true).map_err(io_error)?;
        if id != ID_EBML {
            return Err(bad("not an EBML file"));
        }
        let (header_size, _) = read_vint(reader, false).map_err(io_error)?;
        if header_size == u64::MAX {
            return Err(bad("EBML header of unknown size"));
        }
        reader
            .seek(SeekFrom::Current(header_size as i64))
            .map_err(io_error)?;
        let (id, _) = read_vint(reader, true).map_err(io_error)?;
        if id != ID_SEGMENT {
            return Err(bad("no Matroska Segment"));
        }
        read_vint(reader, false).map_err(io_error)?;
        let segment_start = reader.stream_position().map_err(io_error)?;

        let mut layout = Self::default();
        let mut sought: Vec<(u64, u64)> = Vec::new(); // (element id, absolute position)
        while let (Ok((id, _)), Ok((size, _))) = (read_vint(reader, true), read_vint(reader, false))
        {
            if id == ID_CLUSTER || size == u64::MAX {
                break;
            }
            let start = reader.stream_position().map_err(io_error)?;
            if start.saturating_add(size) > file_length {
                break;
            }
            let element = Element { start, size };
            match id {
                ID_TRACKS => layout.tracks = Some(element),
                ID_ATTACHMENTS => layout.attachments = Some(element),
                ID_SEEK_HEAD => {
                    for (target, position) in seek_entries(&element.read_body(reader)?) {
                        sought.push((target, segment_start.saturating_add(position)));
                    }
                }
                _ => {}
            }
            reader
                .seek(SeekFrom::Start(start + size))
                .map_err(io_error)?;
        }

        for (target, position) in sought {
            let slot = match target {
                ID_TRACKS if layout.tracks.is_none() => &mut layout.tracks,
                ID_ATTACHMENTS if layout.attachments.is_none() => &mut layout.attachments,
                _ => continue,
            };
            if position >= file_length {
                continue;
            }
            reader.seek(SeekFrom::Start(position)).map_err(io_error)?;
            let (Ok((id, _)), Ok((size, _))) = (read_vint(reader, true), read_vint(reader, false))
            else {
                continue;
            };
            let start = reader.stream_position().map_err(io_error)?;
            if id == target && size != u64::MAX && start.saturating_add(size) <= file_length {
                *slot = Some(Element { start, size });
            }
        }
        Ok(layout)
    }
}

/// The (element id, segment-relative position) pairs in a SeekHead body.
fn seek_entries(body: &[u8]) -> Vec<(u64, u64)> {
    children(body)
        .into_iter()
        .filter(|(id, _)| *id == ID_SEEK)
        .filter_map(|(_, seek)| {
            let mut target = None;
            let mut position = None;
            for (id, value) in children(seek) {
                match id {
                    ID_SEEK_ID => target = Some(uint(value)),
                    ID_SEEK_POSITION => position = Some(uint(value)),
                    _ => {}
                }
            }
            Some((target?, position?))
        })
        .collect()
}

/// Each track's Dolby Vision configuration, from its `BlockAdditionMapping` elements.
fn dolby_vision_by_track(tracks: &[u8]) -> HashMap<u64, DolbyVision> {
    let mut found = HashMap::new();
    for (id, entry) in children(tracks) {
        if id != ID_TRACK_ENTRY {
            continue;
        }
        let mut number = None;
        let mut dolby_vision = None;
        for (id, value) in children(entry) {
            match id {
                ID_TRACK_NUMBER => number = Some(uint(value)),
                ID_BLOCK_ADDITION_MAPPING => {
                    let mut kind = 0;
                    let mut extra: &[u8] = &[];
                    for (id, value) in children(value) {
                        match id {
                            ID_BLOCK_ADD_ID_TYPE => kind = uint(value),
                            ID_BLOCK_ADD_ID_EXTRA_DATA => extra = value,
                            _ => {}
                        }
                    }
                    let is_dolby_vision = super::codec::DOLBY_VISION_BOXES
                        .iter()
                        .any(|name| kind == u32::from_be_bytes(**name) as u64);
                    if is_dolby_vision {
                        dolby_vision = dolby_vision.or_else(|| DolbyVision::parse(extra));
                    }
                }
                _ => {}
            }
        }
        if let (Some(number), Some(dolby_vision)) = (number, dolby_vision) {
            found.insert(number, dolby_vision);
        }
    }
    found
}

/// An attachment's name and MIME type, and where its data is, read without the data itself
/// (font attachments are often megabytes each).
struct AttachmentHeader {
    name: String,
    mime: String,
    data: Element,
}

fn attachment_headers<R: Read + Seek>(
    reader: &mut R,
    attachments: Element,
) -> Result<Vec<AttachmentHeader>, ReadError> {
    let end = attachments.start.saturating_add(attachments.size);
    let mut headers = Vec::new();
    let mut position = attachments.start;
    while position < end {
        reader.seek(SeekFrom::Start(position)).map_err(io_error)?;
        let (id, _) = read_vint(reader, true).map_err(io_error)?;
        let (size, _) = read_vint(reader, false).map_err(io_error)?;
        let start = reader.stream_position().map_err(io_error)?;
        if size == u64::MAX || start.saturating_add(size) > end {
            break;
        }
        if id == ID_ATTACHED_FILE {
            let mut name = String::new();
            let mut mime = String::new();
            let mut data = None;
            let file_end = start + size;
            let mut child = start;
            while child < file_end {
                reader.seek(SeekFrom::Start(child)).map_err(io_error)?;
                let (child_id, _) = read_vint(reader, true).map_err(io_error)?;
                let (child_size, _) = read_vint(reader, false).map_err(io_error)?;
                let child_start = reader.stream_position().map_err(io_error)?;
                if child_size == u64::MAX || child_start.saturating_add(child_size) > file_end {
                    break;
                }
                let element = Element {
                    start: child_start,
                    size: child_size,
                };
                match child_id {
                    ID_FILE_NAME | ID_FILE_MEDIA_TYPE if child_size <= 4096 => {
                        let text = String::from_utf8_lossy(&element.read_body(reader)?)
                            .trim_end_matches('\0')
                            .to_string();
                        if child_id == ID_FILE_NAME {
                            name = text;
                        } else {
                            mime = text.to_ascii_lowercase();
                        }
                    }
                    ID_FILE_DATA => data = Some(element),
                    _ => {}
                }
                child = child_start + child_size;
            }
            if let Some(data) = data {
                headers.push(AttachmentHeader { name, mime, data });
            }
        }
        position = start + size;
    }
    Ok(headers)
}

/// Reads an EBML variable-length integer. With `keep_marker` it's an element ID; without, a
/// size, where all value bits set means "unknown size" (returned as `u64::MAX`).
fn read_vint<R: Read>(reader: &mut R, keep_marker: bool) -> io::Result<(u64, usize)> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first)?;
    let length = first[0].leading_zeros() as usize + 1;
    if length > 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad EBML integer",
        ));
    }
    let marker_mask = if length == 8 {
        0
    } else {
        (1u64 << (8 - length)) - 1
    };
    let mut value = if keep_marker {
        first[0] as u64
    } else {
        first[0] as u64 & marker_mask
    };
    let mut all_ones = value == marker_mask;
    for _ in 1..length {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        all_ones &= byte[0] == 0xff;
        value = (value << 8) | byte[0] as u64;
    }
    if !keep_marker && all_ones {
        return Ok((u64::MAX, length));
    }
    Ok((value, length))
}

/// The children of an in-memory EBML master element body.
fn children(body: &[u8]) -> Vec<(u64, &[u8])> {
    let mut found = Vec::new();
    let mut cursor = Cursor::new(body);
    while (cursor.position() as usize) < body.len() {
        let Ok((id, _)) = read_vint(&mut cursor, true) else {
            break;
        };
        let Ok((size, _)) = read_vint(&mut cursor, false) else {
            break;
        };
        let start = cursor.position() as usize;
        let end = if size == u64::MAX {
            body.len()
        } else {
            start.saturating_add(size as usize).min(body.len())
        };
        found.push((id, &body[start..end]));
        cursor.set_position(end as u64);
    }
    found
}

fn uint(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(8)
        .fold(0, |value, byte| (value << 8) | *byte as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes one EBML element (IDs are written with their marker bits already in place).
    fn element(id: u64, body: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = id
            .to_be_bytes()
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect();
        // 8-byte size: 0x01 marker then seven bytes of length.
        out.push(0x01);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn reads_vints_including_unknown_sizes() {
        assert_eq!(read_vint(&mut Cursor::new([0x81]), false).unwrap(), (1, 1));
        assert_eq!(
            read_vint(&mut Cursor::new([0x40, 0x02]), false).unwrap(),
            (2, 2)
        );
        assert_eq!(
            read_vint(&mut Cursor::new([0xFF]), false).unwrap().0,
            u64::MAX
        );
        assert_eq!(
            read_vint(&mut Cursor::new([0x1A, 0x45, 0xDF, 0xA3]), true)
                .unwrap()
                .0,
            ID_EBML
        );
        assert!(read_vint(&mut Cursor::new([0x00]), false).is_err());
    }

    #[test]
    fn finds_dolby_vision_in_a_block_addition_mapping() {
        let record = [1, 0, 0x10, 0x35, 0x10];
        let mapping = [
            element(
                ID_BLOCK_ADD_ID_TYPE,
                &u32::from_be_bytes(*b"dvvC").to_be_bytes(),
            ),
            element(ID_BLOCK_ADD_ID_EXTRA_DATA, &record),
        ]
        .concat();
        let entry = [
            element(ID_TRACK_NUMBER, &[3]),
            element(ID_BLOCK_ADDITION_MAPPING, &mapping),
        ]
        .concat();
        let tracks = element(ID_TRACK_ENTRY, &entry);
        let found = dolby_vision_by_track(&tracks);
        assert_eq!(found.get(&3).map(|dv| dv.profile), Some(8));
    }

    #[test]
    fn maps_codec_ids() {
        assert_eq!(codec_name("V_MPEGH/ISO/HEVC"), "hevc");
        assert_eq!(codec_name("A_AAC/MPEG4/LC/SBR"), "aac");
        assert_eq!(codec_name("S_HDMV/PGS"), "hdmv_pgs_subtitle");
        assert_eq!(codec_name("V_UNKNOWN"), "V_UNKNOWN");
    }
}
