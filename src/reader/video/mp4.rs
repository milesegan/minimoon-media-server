//! MP4 and QuickTime: the `moov` box through `mp4-atom`, the raw boxes it drops (Dolby Vision
//! records, codec configuration records, `colr`, `elng`, track names) by hand, and the
//! iTunes-style tags and cover through `lofty`.

use super::codec::{
    aac_channels, av1_transfer, avc_transfer, has_hdr10_plus, hevc_config, image_mime,
    leading_year, nonblank, round_frame_rate, snap_frame_rate, transfer_name, DolbyVision,
    DOLBY_VISION_BOXES,
};
use super::{bad, io_error, MAX_STRUCTURE_BYTES};
use crate::model::{Picture, ReadError, Stream, VideoInfo, VideoRead, VideoTags};
use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4File};
use mp4_atom::{Codec, ReadFrom};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// The DASH role scheme MP4 `kind` boxes use; FFmpeg writes forced subtitles with it.
const DASH_ROLE_SCHEME: &str = "urn:mpeg:dash:role:2011";

/// Reads an MP4 file's streams and tags.
pub(super) fn read(path: &Path) -> Result<VideoRead, ReadError> {
    let mut file = File::open(path)
        .map(|file| BufReader::with_capacity(64 * 1024, file))
        .map_err(io_error)?;
    let moov_bytes = read_moov(&mut file)?;
    let moov = mp4_atom::Moov::read_from(&mut Cursor::new(&moov_bytes))
        .map_err(|error| bad(format!("mp4: {error}")))?;
    let moov_body = box_body(&moov_bytes).ok_or_else(|| bad("malformed moov"))?;
    let raw_traks: Vec<&[u8]> = boxes(moov_body)
        .filter(|(kind, _)| kind == b"trak")
        .map(|(_, body)| body)
        .collect();

    // Chapter tracks are text tracks other tracks point at with `tref`/`chap`; they aren't
    // subtitles.
    let chapter_tracks: HashSet<u32> = raw_traks
        .iter()
        .filter_map(|trak| path_in(trak, &[b"tref", b"chap"]))
        .flat_map(|chap| {
            chap.as_chunks::<4>()
                .0
                .iter()
                .map(|id| u32::from_be_bytes(*id))
        })
        .collect();

    let duration_ms = (moov.mvhd.timescale > 0)
        .then(|| moov.mvhd.duration.saturating_mul(1000) / moov.mvhd.timescale as u64);

    let mut streams = Vec::new();
    for (index, trak) in moov.trak.iter().enumerate() {
        let raw = raw_traks.get(index).copied().unwrap_or(&[]);
        if chapter_tracks.contains(&trak.tkhd.track_id) {
            continue;
        }
        let Some(entry) = sample_entry(raw) else {
            continue;
        };
        let handler = trak.mdia.hdlr.handler.to_string();
        let Some((kind, codec)) = classify(&handler, entry.kind, entry.object_type()) else {
            continue; // a cover image stored as a one-frame track
        };
        let mut stream = Stream::new(kind);
        stream.track_number = Some(trak.tkhd.track_id as u64);
        stream.codec = Some(codec);
        stream.language = path_in(raw, &[b"mdia", b"elng"])
            .and_then(|elng| elng.get(4..))
            .and_then(|text| nonblank(&String::from_utf8_lossy(text)))
            .or_else(|| {
                let language = trak.mdia.mdhd.language.trim();
                (!language.is_empty() && language != "und").then(|| language.to_string())
            });
        stream.title = path_in(raw, &[b"udta", b"name"])
            .and_then(|name| nonblank(&String::from_utf8_lossy(name)));
        stream.default = Some(trak.tkhd.enabled);
        stream.forced = Some(trak.udta.as_ref().is_some_and(|udta| {
            udta.kind
                .iter()
                .any(|kind| kind.scheme_uri == DASH_ROLE_SCHEME && kind.value == "forced-subtitle")
        }));

        match kind {
            "video" => read_video_track(&mut file, trak, &entry, &mut stream),
            "audio" => stream.channels = audio_channels(trak, &entry),
            _ => {}
        }
        streams.push(stream);
    }

    Ok(VideoRead {
        video: VideoInfo {
            duration_ms,
            container: "mp4".to_string(),
            streams,
        },
        tags: read_tags(path)?,
    })
}

/// The `covr` picture, read through `lofty`.
pub(super) fn read_art(path: &Path) -> Result<Option<Picture>, ReadError> {
    let Some(ilst) = read_ilst(path, true)? else {
        return Ok(None);
    };
    let Some(picture) = ilst.pictures().and_then(|mut pictures| pictures.next()) else {
        return Ok(None);
    };
    let data = picture.data().to_vec();
    let mime = image_mime(&data)
        .map(str::to_string)
        .or_else(|| picture.mime_type().map(|mime| mime.as_str().to_string()))
        .unwrap_or_else(|| "image/jpeg".to_string());
    Ok(Some(Picture { data, mime }))
}

/// Finds the top-level `moov` box and reads it whole, header included.
fn read_moov<R: Read + Seek>(file: &mut R) -> Result<Vec<u8>, ReadError> {
    let length = file.seek(SeekFrom::End(0)).map_err(io_error)?;
    let mut position = 0u64;
    while position + 8 <= length {
        file.seek(SeekFrom::Start(position)).map_err(io_error)?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header).map_err(io_error)?;
        let mut size = u32::from_be_bytes(header[0..4].try_into().unwrap()) as u64;
        if size == 1 {
            let mut large = [0u8; 8];
            file.read_exact(&mut large).map_err(io_error)?;
            size = u64::from_be_bytes(large);
        } else if size == 0 {
            size = length - position;
        }
        if size < 8 {
            return Err(bad("malformed MP4 box size"));
        }
        if &header[4..8] == b"moov" {
            if size > MAX_STRUCTURE_BYTES as u64 || position + size > length {
                return Err(bad(format!(
                    "a {size} byte moov exceeds the reader's limit"
                )));
            }
            file.seek(SeekFrom::Start(position)).map_err(io_error)?;
            let mut moov = vec![0u8; size as usize];
            file.read_exact(&mut moov).map_err(io_error)?;
            return Ok(moov);
        }
        position = position.saturating_add(size);
    }
    Err(bad("no moov box"))
}

/// The body of a box given its bytes from the header on.
fn box_body(bytes: &[u8]) -> Option<&[u8]> {
    let size = u32::from_be_bytes(bytes.get(0..4)?.try_into().ok()?);
    let header = if size == 1 { 16 } else { 8 };
    bytes.get(header..)
}

/// Iterates the child boxes in a container body: (type, body).
fn boxes(body: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut position = 0usize;
    std::iter::from_fn(move || {
        let header = body.get(position..position + 8)?;
        let mut size = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = header[4..8].try_into().unwrap();
        let mut header_length = 8;
        if size == 1 {
            size = u64::from_be_bytes(body.get(position + 8..position + 16)?.try_into().unwrap())
                .try_into()
                .ok()?;
            header_length = 16;
        } else if size == 0 {
            size = body.len() - position;
        }
        if size < header_length || position + size > body.len() {
            return None;
        }
        let child = &body[position + header_length..position + size];
        position += size;
        Some((kind, child))
    })
}

fn child<'a>(body: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(body).find(|(k, _)| k == kind).map(|(_, body)| body)
}

/// Follows a path of child boxes down from a container body.
fn path_in<'a>(body: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    path.iter().try_fold(body, |body, kind| child(body, kind))
}

/// A track's first sample entry, from the raw `stsd`.
struct SampleEntry<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

impl<'a> SampleEntry<'a> {
    /// The child boxes of a visual sample entry, after its 78 fixed bytes.
    fn visual_children(&self) -> impl Iterator<Item = ([u8; 4], &'a [u8])> {
        boxes(self.body.get(78..).unwrap_or(&[]))
    }

    fn visual_child(&self, kind: &[u8; 4]) -> Option<&'a [u8]> {
        self.visual_children()
            .find(|(k, _)| k == kind)
            .map(|(_, body)| body)
    }

    fn visual_size(&self) -> Option<(u32, u32)> {
        let width = u16::from_be_bytes(self.body.get(24..26)?.try_into().ok()?);
        let height = u16::from_be_bytes(self.body.get(26..28)?.try_into().ok()?);
        Some((width as u32, height as u32))
    }

    /// The channel count in an audio sample entry (QuickTime versions 0, 1 and 2).
    fn audio_channels(&self) -> Option<u32> {
        let version = u16::from_be_bytes(self.body.get(8..10)?.try_into().ok()?);
        if version == 2 {
            let count = u32::from_be_bytes(self.body.get(40..44)?.try_into().ok()?);
            return (count > 0).then_some(count);
        }
        let count = u16::from_be_bytes(self.body.get(16..18)?.try_into().ok()?);
        (count > 0).then_some(count as u32)
    }

    /// The child boxes of an audio sample entry, after its fixed fields (28 bytes, plus 16 for
    /// a QuickTime version 1 entry or 36 for version 2).
    fn audio_children(&self) -> impl Iterator<Item = ([u8; 4], &'a [u8])> {
        let version = self
            .body
            .get(8..10)
            .map_or(0, |v| u16::from_be_bytes([v[0], v[1]]));
        let offset = match version {
            1 => 44,
            2 => 64,
            _ => 28,
        };
        boxes(self.body.get(offset..).unwrap_or(&[]))
    }

    /// The MPEG-4 `objectTypeIndication` in an `mp4v` or `mp4a` entry's `esds`, which says
    /// what an `mp4v` track really holds (a JPEG cover, MPEG-4 Part 2, MPEG-2) and whether an
    /// `mp4a` track is AAC or MP3.
    fn object_type(&self) -> Option<u8> {
        let esds = match &self.kind {
            b"mp4v" => self.visual_child(b"esds"),
            b"mp4a" => self
                .audio_children()
                .find(|(kind, _)| kind == b"esds")
                .map(|(_, body)| body),
            _ => None,
        }?;
        esds_object_type(esds)
    }
}

/// Reads `objectTypeIndication` from an `esds` body: the full-box header, then an
/// ES_Descriptor (tag 3) whose first child is a DecoderConfigDescriptor (tag 4).
fn esds_object_type(esds: &[u8]) -> Option<u8> {
    fn descriptor(data: &[u8], tag: u8) -> Option<&[u8]> {
        if *data.first()? != tag {
            return None;
        }
        let mut size = 0usize;
        for index in 1..=4 {
            let byte = *data.get(index)?;
            size = (size << 7) | (byte & 0x7f) as usize;
            if byte & 0x80 == 0 {
                return data.get(index + 1..(index + 1 + size).min(data.len()));
            }
        }
        None
    }
    let es = descriptor(esds.get(4..)?, 3)?;
    let flags = *es.get(2)?;
    let mut position = 3;
    if flags & 0x80 != 0 {
        position += 2; // dependsOn_ES_ID
    }
    if flags & 0x40 != 0 {
        position += 1 + *es.get(position)? as usize; // URL
    }
    if flags & 0x20 != 0 {
        position += 2; // OCR_ES_Id
    }
    descriptor(es.get(position..)?, 4)?.first().copied()
}

fn sample_entry(trak: &[u8]) -> Option<SampleEntry<'_>> {
    let stsd = path_in(trak, &[b"mdia", b"minf", b"stbl", b"stsd"])?;
    // Full box header (4 bytes), then the entry count (4 bytes), then the entries.
    let (kind, body) = boxes(stsd.get(8..)?).next()?;
    Some(SampleEntry { kind, body })
}

/// The stream type and codec name for a track, or `None` for a cover-image track.
fn classify(
    handler: &str,
    entry: [u8; 4],
    object_type: Option<u8>,
) -> Option<(&'static str, String)> {
    let codec = match (&entry, object_type) {
        // JPEG, PNG and BMP in an `mp4v` entry: a cover stored as a track.
        (b"mp4v", Some(0x6C..=0x6E)) => return None,
        (b"mp4v", Some(0x60..=0x65)) => "mpeg2video",
        (b"mp4v", Some(0x6A)) => "mpeg1video",
        (b"mp4a", Some(0x69 | 0x6B)) => "mp3",
        (b"mp4a", Some(0xA5)) => "ac3",
        (b"mp4a", Some(0xA6)) => "eac3",
        (b"mp4a", Some(0xA9)) => "dts",
        (b"mp4a", Some(0xAD)) => "opus",
        _ => "",
    };
    if !codec.is_empty() {
        let kind = if entry == *b"mp4v" { "video" } else { "audio" };
        return Some((kind, codec.to_string()));
    }
    let codec = match &entry {
        b"avc1" | b"avc3" | b"dva1" | b"dvav" => "h264",
        b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => "hevc",
        b"av01" | b"dav1" => "av1",
        b"vp09" => "vp9",
        b"vp08" => "vp8",
        b"mp4v" => "mpeg4",
        b"jpeg" | b"mjpa" | b"mjpb" | b"png " => return None,
        b"mp4a" => "aac",
        b"ac-3" => "ac3",
        b"ec-3" => "eac3",
        b"Opus" => "opus",
        b"fLaC" => "flac",
        b".mp3" => "mp3",
        b"alac" => "alac",
        b"lpcm" | b"sowt" | b"twos" | b"ipcm" | b"fpcm" => "pcm",
        b"tx3g" | b"text" => "mov_text",
        b"wvtt" => "webvtt",
        b"stpp" => "ttml",
        b"c608" => "eia_608",
        other => {
            return Some((
                handler_kind(handler),
                String::from_utf8_lossy(other).trim().to_string(),
            ))
        }
    };
    let kind = match codec {
        "h264" | "hevc" | "av1" | "vp9" | "vp8" | "mpeg4" => "video",
        "mov_text" | "webvtt" | "ttml" | "eia_608" => "subtitle",
        _ => "audio",
    };
    Some((kind, codec.to_string()))
}

fn handler_kind(handler: &str) -> &'static str {
    match handler {
        "vide" => "video",
        "soun" => "audio",
        "sbtl" | "subt" | "text" | "clcp" => "subtitle",
        _ => "data",
    }
}

fn read_video_track<R: Read + Seek>(
    file: &mut R,
    trak: &mp4_atom::Trak,
    entry: &SampleEntry,
    stream: &mut Stream,
) {
    if let Some((width, height)) = entry.visual_size() {
        stream.width = Some(width);
        stream.height = Some(height);
    }

    let mut nal_length_size = None;
    let codec_transfer = if let Some(hvcc) = entry.visual_child(b"hvcC") {
        let (transfer, size) = hevc_config(hvcc);
        nal_length_size = Some(size);
        transfer
    } else if let Some(avcc) = entry.visual_child(b"avcC") {
        avc_transfer(avcc)
    } else if let Some(av1c) = entry.visual_child(b"av1C") {
        av1_transfer(av1c)
    } else {
        None
    };
    // Codec headers first, then an `nclx` colour box.
    stream.transfer = codec_transfer
        .and_then(transfer_name)
        .or_else(|| colr_transfer(entry).and_then(transfer_name))
        .map(str::to_string);

    // mp4-atom drops these unknown children when it decodes a sample entry, so they're read
    // from the raw entry.
    let dolby_vision = DOLBY_VISION_BOXES
        .iter()
        .find_map(|kind| entry.visual_child(kind))
        .and_then(DolbyVision::parse);
    if let Some(dolby_vision) = dolby_vision {
        stream.dolby_vision_profile = Some(dolby_vision.profile);
        if stream.transfer.is_none() && dolby_vision.implies_pq() {
            stream.transfer = Some("pq".to_string());
        }
    }

    stream.frame_rate = nominal_frame_rate(trak);

    if let Some(nal_length_size) = nal_length_size {
        if let Some(sample) = first_sample(file, trak) {
            if has_hdr10_plus(&sample, nal_length_size) {
                stream.hdr10_plus = Some(true);
            }
        }
    }
}

/// The transfer code in an `nclx` (ISO) or `nclc` (QuickTime) `colr` box: colour type, then
/// primaries, transfer and matrix as 16-bit values.
fn colr_transfer(entry: &SampleEntry) -> Option<u8> {
    entry
        .visual_children()
        .filter(|(kind, _)| kind == b"colr")
        .find_map(|(_, body)| {
            if !matches!(body.get(0..4)?, b"nclx" | b"nclc") {
                return None;
            }
            let transfer = u16::from_be_bytes(body.get(6..8)?.try_into().ok()?);
            u8::try_from(transfer).ok()
        })
}

/// The nominal frame rate, from the sample durations.
///
/// MP4 has no nominal-rate field, and a muxer whose timescale doesn't divide evenly writes
/// alternating durations (656, 672, 672 ticks for 24 fps at 16 kHz). So the rate is the
/// average over every sample but a lone last one (whose duration is often arbitrary), snapped
/// to a standard rate when it's within 0.2% of one.
fn nominal_frame_rate(trak: &mp4_atom::Trak) -> Option<f64> {
    let mut entries = trak.mdia.minf.stbl.stts.entries.as_slice();
    if let [rest @ .., last] = entries {
        if last.sample_count == 1 && !rest.is_empty() {
            entries = rest;
        }
    }
    let samples: u64 = entries.iter().map(|e| e.sample_count as u64).sum();
    let ticks: u64 = entries
        .iter()
        .map(|e| e.sample_count as u64 * e.sample_delta as u64)
        .sum();
    if samples == 0 || ticks == 0 {
        return None;
    }
    let rate = samples as f64 * trak.mdia.mdhd.timescale as f64 / ticks as f64;
    round_frame_rate(snap_frame_rate(rate))
}

/// The bytes of a track's first sample.
fn first_sample<R: Read + Seek>(file: &mut R, trak: &mp4_atom::Trak) -> Option<Vec<u8>> {
    let stbl = &trak.mdia.minf.stbl;
    let offset = stbl
        .stco
        .as_ref()
        .and_then(|stco| stco.entries.first().map(|offset| *offset as u64))
        .or_else(|| {
            stbl.co64
                .as_ref()
                .and_then(|co64| co64.entries.first().copied())
        })?;
    let size = match &stbl.stsz.samples {
        mp4_atom::StszSamples::Identical { size, .. } => *size as u64,
        mp4_atom::StszSamples::Different { sizes } => *sizes.first()? as u64,
    };
    if size == 0 || size > MAX_STRUCTURE_BYTES as u64 {
        return None;
    }
    let mut sample = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut sample).ok()?;
    Some(sample)
}

/// The decoded channel count: from the codec's own configuration where it has one, otherwise
/// the sample entry's count.
fn audio_channels(trak: &mp4_atom::Trak, entry: &SampleEntry) -> Option<u32> {
    let container = entry.audio_channels();
    match trak.mdia.minf.stbl.stsd.codecs.first() {
        Some(Codec::Mp4a(mp4a)) if entry.object_type().is_none_or(is_aac_object_type) => {
            let config = mp4a
                .esds
                .es_desc
                .dec_config
                .dec_specific
                .as_ref()
                .map(|specific| specific.raw.as_slice());
            aac_channels(config, container, false)
        }
        Some(Codec::Ac3(ac3)) => Some(ac3_channels(ac3.dac3.acmod, ac3.dac3.lfeon)),
        Some(Codec::Eac3(eac3)) => eac3
            .dec3
            .substreams
            .first()
            .map(|substream| {
                ac3_channels(substream.acmod, substream.lfeon)
                    + substream.chan_loc.map_or(0, dependent_channels)
            })
            .or(container),
        _ => container,
    }
}

/// MPEG-4 audio (AAC and its relatives) and the MPEG-2 AAC profiles.
fn is_aac_object_type(object_type: u8) -> bool {
    matches!(object_type, 0x40 | 0x66..=0x68)
}

/// Channels for an AC-3 audio coding mode, plus the LFE channel.
fn ac3_channels(acmod: u8, lfe: bool) -> u32 {
    const FULL_BANDWIDTH: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
    FULL_BANDWIDTH[(acmod & 7) as usize] + lfe as u32
}

/// Extra channels an E-AC-3 dependent substream adds, from its 9-bit `chan_loc` (ETSI TS
/// 102 366 Table F.6.1, most significant bit first): Lc/Rc, Lrs/Rrs, Cs, Ts, Lsd/Rsd, Lw/Rw,
/// Lvh/Rvh, Cvh, LFE2. Pairs count as two.
fn dependent_channels(chan_loc: u16) -> u32 {
    const PAIR: [bool; 9] = [true, true, false, false, true, true, true, false, false];
    (0..9)
        .filter(|bit| chan_loc & (1 << (8 - bit)) != 0)
        .map(|bit| if PAIR[bit] { 2 } else { 1 })
        .sum()
}

/// Reads the iTunes-style `ilst` through lofty. A file lofty can't parse has no tags, but an
/// I/O error is still reported as one.
fn read_ilst(path: &Path, cover_art: bool) -> Result<Option<Ilst>, ReadError> {
    let mut file = File::open(path)
        .map(|file| BufReader::with_capacity(64 * 1024, file))
        .map_err(io_error)?;
    let options = ParseOptions::new()
        .read_properties(false)
        .read_cover_art(cover_art);
    match Mp4File::read_from(&mut file, options) {
        Ok(mut mp4) => Ok(mp4.remove_ilst()),
        Err(error) => match io_cause(&error) {
            Some(io) => Err(ReadError::from_io(io)),
            None => Ok(None),
        },
    }
}

/// The I/O error behind a lofty error, if that's what it was.
fn io_cause<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a std::io::Error> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return Some(io);
        }
        current = error.source();
    }
    None
}

/// Video tags from the iTunes atoms: `©nam`, `tvsh`, `tvsn`, `tves`, `©day`, and `desc` (or
/// the long description `ldes`).
fn read_tags(path: &Path) -> Result<VideoTags, ReadError> {
    let Some(ilst) = read_ilst(path, false)? else {
        return Ok(VideoTags::default());
    };
    let text = |name: &[u8; 4]| -> Option<String> {
        ilst.get(&AtomIdent::Fourcc(*name))?
            .data()
            .find_map(|data| match data {
                AtomData::UTF8(text) | AtomData::UTF16(text) => nonblank(text),
                _ => None,
            })
    };
    let number = |name: &[u8; 4]| -> Option<u32> {
        ilst.get(&AtomIdent::Fourcc(*name))?
            .data()
            .find_map(|data| match data {
                AtomData::UnsignedInteger(value) => Some(*value),
                AtomData::SignedInteger(value) => u32::try_from(*value).ok(),
                AtomData::UTF8(text) | AtomData::UTF16(text) => text.trim().parse().ok(),
                _ => None,
            })
    };
    Ok(VideoTags {
        title: text(b"\xa9nam"),
        show: text(b"tvsh"),
        season: number(b"tvsn"),
        episode: number(b"tves"),
        year: text(b"\xa9day").as_deref().and_then(leading_year),
        description: text(b"desc").or_else(|| text(b"ldes")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn walks_nested_boxes() {
        let name = mp4_box(b"name", b"Commentary");
        let udta = mp4_box(b"udta", &name);
        let trak = [mp4_box(b"tkhd", &[0; 4]), udta].concat();
        assert_eq!(
            path_in(&trak, &[b"udta", b"name"]),
            Some(&b"Commentary"[..])
        );
        assert_eq!(path_in(&trak, &[b"mdia"]), None);
        // A child claiming more bytes than remain stops the walk instead of panicking.
        let broken = [0, 0, 0, 99, b'f', b'r', b'e', b'e'];
        assert_eq!(boxes(&broken).count(), 0);
    }

    #[test]
    fn counts_ac3_and_eac3_channels() {
        assert_eq!(ac3_channels(7, true), 6); // 3/2 + LFE
        assert_eq!(ac3_channels(2, false), 2);
        // A dependent substream adding Lrs/Rrs makes 7.1.
        assert_eq!(ac3_channels(7, true) + dependent_channels(0b0_1000_0000), 8);
    }

    #[test]
    fn classifies_sample_entries() {
        assert_eq!(
            classify("vide", *b"dvh1", None),
            Some(("video", "hevc".to_string()))
        );
        assert_eq!(classify("vide", *b"jpeg", None), None);
        assert_eq!(classify("vide", *b"mp4v", Some(0x6C)), None);
        assert_eq!(
            classify("vide", *b"mp4v", Some(0x20)),
            Some(("video", "mpeg4".to_string()))
        );
        assert_eq!(
            classify("soun", *b"mp4a", Some(0x6B)),
            Some(("audio", "mp3".to_string()))
        );
        assert_eq!(
            classify("sbtl", *b"tx3g", None),
            Some(("subtitle", "mov_text".to_string()))
        );
        assert_eq!(
            classify("soun", *b"xyz1", None),
            Some(("audio", "xyz1".to_string()))
        );
    }

    #[test]
    fn reads_the_object_type_from_an_esds() {
        // From a real cover track: ES_Descriptor (3) → DecoderConfigDescriptor (4), OTI 0x6C.
        let esds = [
            0, 0, 0, 0, 0x03, 0x80, 0x80, 0x80, 0x1B, 0x00, 0x07, 0x00, 0x04, 0x80, 0x80, 0x80,
            0x0D, 0x6C, 0x11, 0, 0, 0, 0, 0x03, 0x34, 0x50, 0, 0x03, 0x34, 0x50,
        ];
        assert_eq!(esds_object_type(&esds), Some(0x6C));
        assert_eq!(esds_object_type(&esds[..10]), None);
    }
}
