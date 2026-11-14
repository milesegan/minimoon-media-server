//! Codec-level facts the containers don't carry themselves (spec §5, video reading rules).
//!
//! Colour comes from the codec headers, Dolby Vision from its configuration record, HDR10+
//! from SEI messages in the first frame, and the decoded AAC channel count from the
//! AudioSpecificConfig. The container readers (`mkv`, `mp4`) find the bytes; this module
//! interprets them.

use std::io::Cursor;

/// Maps an H.273 transfer characteristics code to the wire value.
///
/// Unspecified (2) and reserved (0, 3) give nothing, so the field stays absent.
pub(super) fn transfer_name(code: u8) -> Option<&'static str> {
    match code {
        16 => Some("pq"),
        18 => Some("hlg"),
        0 | 2 | 3 => None,
        _ => Some("sdr"),
    }
}

/// The transfer code from the SPS in an HEVC configuration record (`hvcC` body or Matroska
/// `CodecPrivate`), and the NAL length size the record declares.
pub(super) fn hevc_config(hvcc: &[u8]) -> (Option<u8>, usize) {
    // Byte 21 ends in lengthSizeMinusOne; 4-byte lengths are the common default.
    let nal_length_size = hvcc.get(21).map_or(4, |b| (b & 3) as usize + 1);
    let Ok(record) = scuffle_h265::HEVCDecoderConfigurationRecord::demux(Cursor::new(hvcc)) else {
        return (None, nal_length_size);
    };
    let transfer = record
        .arrays
        .iter()
        .filter(|array| array.nal_unit_type == scuffle_h265::NALUnitType::SpsNut)
        .find_map(|array| array.nalus.first())
        .and_then(|nalu| scuffle_h265::SpsNALUnit::parse(Cursor::new(nalu.as_ref())).ok())
        .and_then(|sps| {
            sps.rbsp
                .vui_parameters
                .as_ref()
                .map(|vui| vui.video_signal_type.transfer_characteristics)
        });
    (transfer, nal_length_size)
}

/// The transfer code from the first SPS in an H.264 configuration record (`avcC`).
pub(super) fn avc_transfer(avcc: &[u8]) -> Option<u8> {
    use h264_reader::avcc::AvcDecoderConfigurationRecord;
    use h264_reader::nal::sps::SeqParameterSet;
    use h264_reader::rbsp::{decode_nal, BitReader};

    let record = AvcDecoderConfigurationRecord::try_from(avcc).ok()?;
    let sps = record.sequence_parameter_sets().find_map(Result::ok)?;
    let rbsp = decode_nal(sps).ok()?;
    let sps = SeqParameterSet::from_bits(BitReader::new(&rbsp[..])).ok()?;
    sps.vui_parameters?
        .video_signal_type?
        .colour_description
        .map(|colour| colour.transfer_characteristics)
}

/// The transfer code from the sequence header in an AV1 configuration record (`av1C`).
pub(super) fn av1_transfer(av1c: &[u8]) -> Option<u8> {
    let mut cursor = Cursor::new(bytes::Bytes::copy_from_slice(av1c));
    let record = scuffle_av1::AV1CodecConfigurationRecord::demux(&mut cursor).ok()?;
    if record.config_obu.is_empty() {
        return None;
    }
    let mut obu = Cursor::new(record.config_obu.as_ref());
    let header = scuffle_av1::ObuHeader::parse(&mut obu).ok()?;
    let sequence = scuffle_av1::seq::SequenceHeaderObu::parse(header, &mut obu).ok()?;
    Some(sequence.color_config.transfer_characteristics)
}

/// A Dolby Vision configuration record (`dvcC`, `dvvC` or `dvwC`), as far as MMP uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DolbyVision {
    pub profile: u8,
    /// `dv_bl_signal_compatibility_id`: 0 means the base layer has no standalone meaning.
    pub compatibility: u8,
}

impl DolbyVision {
    /// Parses a record body: version (2 bytes), then profile (7 bits), level (6 bits),
    /// rpu/el/bl present flags (3 bits), and the compatibility id (4 bits).
    pub fn parse(record: &[u8]) -> Option<Self> {
        if record.len() < 5 {
            return None;
        }
        Some(Self {
            profile: record[2] >> 1,
            compatibility: record[4] >> 4,
        })
    }

    /// Whether this stream is PQ even though its base layer may carry no transfer value.
    ///
    /// Profile 5 (and AV1 profile 10 with no compatible base, its counterpart) is always PQ;
    /// its base layer is in Dolby's own colour space and usually leaves transfer unspecified.
    pub fn implies_pq(&self) -> bool {
        self.profile == 5 || (self.profile == 10 && self.compatibility == 0)
    }
}

/// The four-character types of a Dolby Vision configuration record.
pub(super) const DOLBY_VISION_BOXES: [&[u8; 4]; 3] = [b"dvcC", b"dvvC", b"dvwC"];

/// Whether an HEVC access unit, in length-prefixed form, carries an HDR10+ SEI message:
/// ITU-T T.35 user data registered by Samsung (country 0xB5, provider 0x003C, oriented code
/// 0x0001) with application identifier 4.
pub(super) fn has_hdr10_plus(access_unit: &[u8], nal_length_size: usize) -> bool {
    if !(1..=4).contains(&nal_length_size) {
        return false;
    }
    let mut position = 0;
    while position + nal_length_size <= access_unit.len() {
        let length = access_unit[position..position + nal_length_size]
            .iter()
            .fold(0usize, |value, byte| (value << 8) | *byte as usize);
        position += nal_length_size;
        if length < 2 || position + length > access_unit.len() {
            break;
        }
        let nal = &access_unit[position..position + length];
        position += length;
        let nal_type = (nal[0] >> 1) & 0x3f;
        // Prefix SEI only; HDR10+ is never sent as a suffix SEI before the first slice.
        if nal_type == 39 && sei_has_hdr10_plus(&unescape(&nal[2..])) {
            return true;
        }
    }
    false
}

/// Removes emulation prevention bytes (00 00 03 → 00 00).
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        rbsp.push(byte);
    }
    rbsp
}

/// Walks the SEI messages in an SEI RBSP looking for the HDR10+ T.35 payload.
fn sei_has_hdr10_plus(rbsp: &[u8]) -> bool {
    let mut position = 0;
    let read_value = |position: &mut usize| -> Option<usize> {
        let mut value = 0usize;
        while *rbsp.get(*position)? == 0xff {
            value += 255;
            *position += 1;
        }
        value += *rbsp.get(*position)? as usize;
        *position += 1;
        Some(value)
    };
    // 0x80 is the rbsp_trailing_bits that ends the message list.
    while position < rbsp.len() && rbsp[position] != 0x80 {
        let Some(payload_type) = read_value(&mut position) else {
            return false;
        };
        let Some(payload_size) = read_value(&mut position) else {
            return false;
        };
        let Some(payload) = rbsp.get(position..position + payload_size) else {
            return false;
        };
        if payload_type == 4
            && payload.len() >= 6
            && payload[0] == 0xB5
            && payload[1..3] == [0x00, 0x3C]
            && payload[3..5] == [0x00, 0x01]
            && payload[5] == 4
        {
            return true;
        }
        position += payload_size;
    }
    false
}

/// What an AAC AudioSpecificConfig says about the decoded channel count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AacConfig {
    /// The core audio object type (2 for AAC LC, even under SBR or PS signalling).
    pub object_type: u8,
    /// `channelConfiguration`; 0 means a program config element defines the channels.
    pub channel_configuration: u8,
    /// SBR (HE-AAC) is signalled.
    pub sbr: bool,
    /// Parametric stereo: `Some(true)` when signalled, `Some(false)` when explicitly signalled
    /// absent, `None` when the config doesn't say.
    pub parametric_stereo: Option<bool>,
}

impl AacConfig {
    /// Parses an AudioSpecificConfig (ISO/IEC 14496-3 §1.6.2.1), including explicit
    /// hierarchical SBR/PS signalling (object types 5 and 29) and backward-compatible
    /// signalling (the 0x2B7 and 0x548 sync extensions after the GASpecificConfig).
    pub fn parse(config: &[u8]) -> Option<Self> {
        let mut bits = Bits::new(config);
        let mut object_type = read_object_type(&mut bits)?;
        read_frequency(&mut bits)?;
        let channel_configuration = bits.read(4)? as u8;
        let mut sbr = false;
        let mut parametric_stereo = None;
        let explicit_hierarchical = object_type == 5 || object_type == 29;
        if explicit_hierarchical {
            sbr = true;
            if object_type == 29 {
                parametric_stereo = Some(true);
            }
            read_frequency(&mut bits)?;
            object_type = read_object_type(&mut bits)?;
            if object_type == 22 {
                bits.read(4)?; // extensionChannelConfiguration
            }
        }
        let config = Self {
            object_type,
            channel_configuration,
            sbr,
            parametric_stereo,
        };
        if !matches!(object_type, 1..=4 | 6 | 7 | 17 | 19..=23) {
            // Not a GASpecificConfig: nothing more we can read.
            return Some(config);
        }
        if channel_configuration == 0 || !skip_ga_specific_config(&mut bits, object_type) {
            return Some(config);
        }
        if explicit_hierarchical {
            return Some(config);
        }
        Some(read_sync_extensions(&mut bits, config))
    }

    /// The channel count a decoder outputs, or `None` when only a program config element
    /// (or an unknown configuration) could say.
    ///
    /// A mono core with SBR is reported as stereo unless the config explicitly rules out
    /// parametric stereo: FFmpeg's decoder (and so VLC, which Matinee uses) outputs two
    /// channels for it, because PS can be present in the frames without being signalled.
    pub fn decoded_channels(&self) -> Option<u32> {
        let core = match self.channel_configuration {
            1..=6 => self.channel_configuration as u32,
            7 => 8,
            11 => 7,
            12 | 14 => 8,
            13 => 24,
            _ => return None,
        };
        let stereo_from_ps = core == 1
            && (self.parametric_stereo == Some(true)
                || (self.sbr && self.parametric_stereo != Some(false)));
        Some(if stereo_from_ps { 2 } else { core })
    }
}

/// The channel count for an AAC track: what its config says, else the container's count, with
/// the same SBR rule as [`AacConfig::decoded_channels`] when only a Matroska codec ID such as
/// `A_AAC/MPEG4/LC/SBR` says SBR is present.
pub(super) fn aac_channels(
    config: Option<&[u8]>,
    container_channels: Option<u32>,
    codec_id_says_sbr: bool,
) -> Option<u32> {
    if let Some(channels) = config
        .and_then(AacConfig::parse)
        .and_then(|config| config.decoded_channels())
    {
        return Some(channels);
    }
    match container_channels {
        Some(1) if codec_id_says_sbr => Some(2),
        other => other,
    }
}

fn read_object_type(bits: &mut Bits) -> Option<u8> {
    let object_type = bits.read(5)? as u8;
    if object_type == 31 {
        Some(32 + bits.read(6)? as u8)
    } else {
        Some(object_type)
    }
}

fn read_frequency(bits: &mut Bits) -> Option<()> {
    if bits.read(4)? == 15 {
        bits.read(24)?;
    }
    Some(())
}

/// Skips a GASpecificConfig whose channel configuration isn't 0. Returns false if it runs out.
fn skip_ga_specific_config(bits: &mut Bits, object_type: u8) -> bool {
    let mut read = || -> Option<()> {
        bits.read(1)?; // frameLengthFlag
        if bits.read(1)? == 1 {
            bits.read(14)?; // coreCoderDelay
        }
        let extension_flag = bits.read(1)?;
        if object_type == 6 || object_type == 20 {
            bits.read(3)?; // layerNr
        }
        if extension_flag == 1 {
            if object_type == 22 {
                bits.read(5 + 11)?; // numOfSubFrame, layer_length
            }
            if matches!(object_type, 17 | 19 | 20 | 23) {
                bits.read(3)?; // the three resilience flags
            }
            bits.read(1)?; // extensionFlag3
        }
        Some(())
    };
    read().is_some()
}

/// Reads backward-compatible SBR and PS signalling after the GASpecificConfig, if present.
fn read_sync_extensions(bits: &mut Bits, mut config: AacConfig) -> AacConfig {
    if bits.remaining() < 16 || bits.peek(11) != Some(0x2B7) {
        return config;
    }
    bits.read(11);
    let Some(extension_type) = read_object_type(bits) else {
        return config;
    };
    if extension_type == 5 {
        let Some(sbr_present) = bits.read(1) else {
            return config;
        };
        config.sbr = sbr_present == 1;
        if config.sbr {
            if read_frequency(bits).is_none() {
                return config;
            }
            if bits.remaining() >= 12 && bits.peek(11) == Some(0x548) {
                bits.read(11);
                if let Some(ps) = bits.read(1) {
                    config.parametric_stereo = Some(ps == 1);
                }
            }
        }
    }
    config
}

/// A big-endian bit reader over a byte slice.
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn remaining(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.position)
    }

    fn peek(&self, count: usize) -> Option<u32> {
        if count > 32 || self.remaining() < count {
            return None;
        }
        let mut value = 0u32;
        for offset in 0..count {
            let bit = self.position + offset;
            let byte = self.data[bit / 8];
            value = (value << 1) | ((byte >> (7 - bit % 8)) & 1) as u32;
        }
        Some(value)
    }

    fn read(&mut self, count: usize) -> Option<u32> {
        let value = self.peek(count)?;
        self.position += count;
        Some(value)
    }
}

/// The MIME type of an image, from its leading bytes.
pub(super) fn image_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if data.starts_with(b"GIF8") {
        Some("image/gif")
    } else if data.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

/// Snaps a measured frame rate to the nearest standard rate within 0.2% of it, if any.
pub(super) fn snap_frame_rate(rate: f64) -> f64 {
    const STANDARD: [f64; 15] = [
        24000.0 / 1001.0,
        24.0,
        25.0,
        30000.0 / 1001.0,
        30.0,
        48000.0 / 1001.0,
        48.0,
        50.0,
        60000.0 / 1001.0,
        60.0,
        100.0,
        120000.0 / 1001.0,
        120.0,
        144.0,
        240.0,
    ];
    let distance = |standard: &f64| ((rate - standard) / standard).abs();
    STANDARD
        .into_iter()
        .filter(|standard| distance(standard) < 0.002)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .unwrap_or(rate)
}

/// Rounds a frame rate to three decimals, as the wire schema shows it (23.976).
pub(super) fn round_frame_rate(rate: f64) -> Option<f64> {
    (rate.is_finite() && rate > 0.0).then(|| (rate * 1000.0).round() / 1000.0)
}

/// The leading four-digit year of a date string such as `2019` or `2019-05-01`.
pub(super) fn leading_year(text: &str) -> Option<u32> {
    let digits: String = text.trim().chars().take(4).collect();
    (digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Trims text and drops it when it's empty.
pub(super) fn nonblank(text: &str) -> Option<String> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_aac_lc_configs_report_their_channel_configuration() {
        // AAC LC, 44.1 kHz, stereo.
        let stereo = AacConfig::parse(&[0x12, 0x10]).unwrap();
        assert_eq!(stereo.object_type, 2);
        assert!(!stereo.sbr);
        assert_eq!(stereo.decoded_channels(), Some(2));
        // AAC LC, 48 kHz, mono.
        assert_eq!(
            AacConfig::parse(&[0x11, 0x88]).unwrap().decoded_channels(),
            Some(1)
        );
        // AAC LC, 48 kHz, 5.1.
        assert_eq!(
            AacConfig::parse(&[0x11, 0xB0]).unwrap().decoded_channels(),
            Some(6)
        );
    }

    #[test]
    fn backward_compatible_sbr_on_a_mono_core_decodes_as_stereo() {
        // From a real HE-AAC trailer: LC mono at 24 kHz, then 0x2B7, SBR present at 48 kHz,
        // with parametric stereo not signalled either way. FFmpeg decodes it as stereo.
        let config = AacConfig::parse(&[0x13, 0x08, 0x56, 0xE5, 0x98]).unwrap();
        assert_eq!(config.object_type, 2);
        assert_eq!(config.channel_configuration, 1);
        assert!(config.sbr);
        assert_eq!(config.parametric_stereo, None);
        assert_eq!(config.decoded_channels(), Some(2));
    }

    #[test]
    fn explicit_ps_signalling_decodes_as_stereo() {
        // Object type 29 (PS), 24 kHz, mono; extension 48 kHz; core AAC LC; GA config.
        // 11101 0110 0001 0011 00010 000 → padded.
        let config = AacConfig::parse(&[0xEB, 0x09, 0x88, 0x00]).unwrap();
        assert!(config.sbr);
        assert_eq!(config.parametric_stereo, Some(true));
        assert_eq!(config.object_type, 2);
        assert_eq!(config.decoded_channels(), Some(2));
    }

    #[test]
    fn explicitly_absent_ps_keeps_a_mono_core_mono() {
        // LC mono 24 kHz, 0x2B7, SBR present at 48 kHz, 0x548 with psPresentFlag = 0.
        // 00010 0110 0001 000 | 01010110111 00101 1 0011 | 10101001000 0 | padding
        let config = AacConfig::parse(&[0x13, 0x08, 0x56, 0xE5, 0x9D, 0x48, 0x00]).unwrap();
        assert!(config.sbr);
        assert_eq!(config.parametric_stereo, Some(false));
        assert_eq!(config.decoded_channels(), Some(1));
    }

    #[test]
    fn container_count_is_used_when_the_config_is_missing_or_a_pce() {
        assert_eq!(aac_channels(None, Some(2), false), Some(2));
        assert_eq!(aac_channels(None, Some(1), true), Some(2));
        // Channel configuration 0: a PCE defines the layout, so the container's count stands.
        assert_eq!(aac_channels(Some(&[0x12, 0x00]), Some(6), false), Some(6));
        assert_eq!(aac_channels(Some(&[]), Some(1), false), Some(1));
    }

    #[test]
    fn dolby_vision_records_give_profile_and_compatibility() {
        // Profile 8, level 6, rpu+bl present, compatibility 1 (HDR10 base).
        let profile_8 = DolbyVision::parse(&[1, 0, 0x10, 0x35, 0x10, 0, 0, 0]).unwrap();
        assert_eq!(profile_8.profile, 8);
        assert_eq!(profile_8.compatibility, 1);
        assert!(!profile_8.implies_pq());
        // Profile 5, compatibility 0.
        let profile_5 = DolbyVision::parse(&[1, 0, 0x0A, 0x35, 0x00]).unwrap();
        assert_eq!(profile_5.profile, 5);
        assert_eq!(profile_5.compatibility, 0);
        assert!(profile_5.implies_pq());
        assert_eq!(DolbyVision::parse(&[1, 0, 0x0A]), None);
    }

    #[test]
    fn finds_an_hdr10_plus_sei_in_a_length_prefixed_access_unit() {
        // Prefix SEI NAL (type 39), user_data_registered_itu_t_t35 (4), Samsung HDR10+ app 4.
        let sei = [
            0x4E, 0x01, 0x04, 0x07, 0xB5, 0x00, 0x3C, 0x00, 0x01, 0x04, 0x01, 0x80,
        ];
        let mut access_unit = (sei.len() as u32).to_be_bytes().to_vec();
        access_unit.extend_from_slice(&sei);
        assert!(has_hdr10_plus(&access_unit, 4));

        // The same message under another provider code isn't HDR10+.
        let mut other = access_unit.clone();
        other[4 + 7] = 0x3B;
        assert!(!has_hdr10_plus(&other, 4));
        // Truncated input doesn't panic.
        assert!(!has_hdr10_plus(&access_unit[..9], 4));
    }

    #[test]
    fn maps_transfer_codes() {
        assert_eq!(transfer_name(16), Some("pq"));
        assert_eq!(transfer_name(18), Some("hlg"));
        assert_eq!(transfer_name(1), Some("sdr"));
        assert_eq!(transfer_name(2), None);
    }

    #[test]
    fn snaps_measured_rates_to_standard_ones() {
        assert_eq!(snap_frame_rate(24.004), 24.0);
        assert_eq!(snap_frame_rate(23.98), 24000.0 / 1001.0);
        assert_eq!(snap_frame_rate(24.41), 24.41);
    }

    #[test]
    fn reads_years_and_sniffs_images() {
        assert_eq!(leading_year("2019-05-01"), Some(2019));
        assert_eq!(leading_year("19"), None);
        assert_eq!(image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(image_mime(b"nope"), None);
    }
}
