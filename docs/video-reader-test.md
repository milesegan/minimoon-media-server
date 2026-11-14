# Video reader test (MMP spec §10, step 7), 2026-11-14

**Recommendation: native Rust.** Use `matroska-demuxer`, `mp4-atom`, `scuffle-h265`, `h264-reader`, `scuffle-av1` and `lofty`, plus about 240 lines of hand-written parsing.
- **Correctness:** it agreed with ffprobe and MediaInfo on duration, codec, size, transfer, Dolby Vision, HDR10+ and covers for all 107 real files, and it caught every synthetic HDR and Dolby Vision case. The tools differed on frame rate, channel count, language and title for some files (below), and the probe doesn't yet read MP4 forced flags.
- **Speed and reads:** it's the fastest of the three, and it reads the least (median 100 KB per file).
- **Packaging:** it builds into the existing Rust server as a 3.2 MB binary, with permissive licences throughout.
- **Remaining gaps:** three small ones, listed below, each fixable.

## What was tested

**Real files:** 107 from a local test library, 106 MKV and 1 MP4.
- **Video:** AV1 ×49, HEVC ×52, H.264 ×6.
- **Audio:** Opus, E-AC-3, AAC (including HE-AAC), FLAC, AC-3.
- **Subtitles:** SRT, ASS, PGS. Some files carry up to 40 subtitle tracks.
- **Other:** font attachments in 73 files, and 2 embedded covers.
- **Not covered:** none of the real files are HDR.

**Synthetic files:** 10, built from x265, `dovi_tool`, `hdr10plus_tool`, mkvmerge and ffmpeg.
- **Formats:** HDR10, HLG, HDR10+ (profile A), Dolby Vision 8.1 (HDR10 base), and Dolby Vision 5 (untagged base, `dvh1` in MP4).
- **Containers:** each format in both MKV and MP4.
- **Contents of each file:**
  - two audio tracks: E-AC-3 5.1 Japanese (default), and AAC stereo English titled "Commentary";
  - two subtitle tracks: English (forced) and French;
  - a cover image;
  - MP4 TV atoms: title, show, season, episode, year and description.

**Candidates:**
1. **Native Rust:** the crates above, plus hand-written code for:
   - the Dolby Vision configuration record;
   - Matroska attachments, reading headers only;
   - the raw EBML and MP4 scanning those need;
   - HDR10+ detection from the first HEVC frame's SEI.
2. **ffprobe** 9.0.2: one call for streams, and one for the first video frame's side data.
3. **MediaInfo** 26.10, the CLI over MediaInfoLib.

**Baseline:** Matinee's own prober code (`VLCKitProber` from `VLCKitEngine.swift`, copied verbatim). It was built against the macOS VLCKit 4 framework in Beam's DerivedData and run over the same files.

The test scripts (the Rust probe, the Matinee baseline harness and the comparison script) and the raw outputs were kept outside this repository and aren't part of it.

## Results

### Synthetic files (ground truth known), correct / 10

| field | native Rust | ffprobe | MediaInfo |
|---|---|---|---|
| transfer (PQ / HLG) | 10 | 10 | 10 |
| Dolby Vision profile | 10 | 10 | 10 |
| HDR10+ | 10 | 10 | 10 |
| audio tracks (codec, channels, language, default) | 10 | 10 | 10 |
| subtitle tracks (codec, language, forced) | **5**: MP4 forced flag not read | 10 | 10 |
| tags (MKV title; MP4 title, show, season, episode, year, description) | 10 | 10 | 10 |
| cover art | 10 | 10 | 10 |

### Real files, where all three agree (107 files)

These rows are all 107/107: duration, video codec, size, transfer, Dolby Vision, HDR10+, cover art.

The rows below aren't. The remaining differences are real disagreements between the tools, not bugs in the probes:

| field | all three agree | the difference |
|---|---|---|
| frame rate | 61 | MediaInfo reports the average frame rate for variable-rate anime (24.41). Rust and ffprobe report the nominal rate (29.97). They agree with each other on 107. |
| audio tracks | 104 | Three trailers use HE-AAC with parametric stereo. The container (and Rust and MediaInfo) say 1 channel; ffprobe decodes and says 2. |
| subtitle tracks | 106 | ffprobe maps Cantonese (`yue`) to `zh`. Rust and MediaInfo keep `yue`. |
| title | 104 | `sample.mkv` has two titles (segment and tag), which each tool combines differently. Two others differ by a trailing space. |

### Speed and bytes read (117 files, local disk)

| | median ms | max ms | median KB read | max KB read |
|---|---|---|---|---|
| native Rust (in-process) | 19 | 407 | **100** | 10,688 (one MP4 with a large `moov`) |
| ffprobe (two processes) | 58 | 114 | 5,069 | 19,028 |
| MediaInfo (one process) | 101 | 280 | not measured | not measured |
| Matinee's VLCKit parse | 8 | 100 | not measured | not measured |

- **Process start-up:** the ffprobe and MediaInfo times include starting a process, and linked into the server they'd be faster.
- **ffprobe's reads:** it reads its default 5 MB probe size per file.
- **Rust's reads:** the first version of the Rust probe read whole font attachments (median 1.2 MB). Reading only attachment headers brought that to 100 KB.

### Packaging

- **Native Rust:**
  - the probe binary is 3.2 MB, and all crates are MIT or Apache-2.0;
  - it would live in the server, next to the server's copy of the tag-reading code.
- **MediaInfoLib:**
  - `libmediainfo` is 8.0 MB plus `libzen` 0.3 MB, both BSD-2;
  - it's C++, so the Rust server would need FFI bindings.
- **ffmpeg:**
  - Homebrew's build is configured with `--enable-gpl --enable-version3`, and `libavcodec` alone is 9.3 MB, linking 22 other libraries;
  - shipping it would mean a custom LGPL build, plus a decoder for the HDR10+ frame check.

## Findings about the formats

1. **Colour information is in the bitstream, not the container.** None of the 106 real MKVs, and none of mkvmerge's own output, have Matroska `Colour` elements. ffprobe finds a transfer value for 52 of the 107 real files only by parsing the HEVC or H.264 SPS colour fields (VUI) or the AV1 sequence header. So any reader has to decode codec headers. `scuffle-h265`, `h264-reader` and `scuffle-av1` did that correctly on every file.
2. **HDR10+ has no container-level marker.** It exists only in SEI messages inside the frames. Detecting it means reading the first video frame (Rust, MediaInfo) or decoding it (ffprobe).
3. **Dolby Vision lives in a configuration record:** a Matroska `BlockAdditionMapping` (`dvcC`/`dvvC`) or an MP4 `dvcC`/`dvvC` box. Profile 5 files carry no transfer value in their base layer. MediaInfo reports `PQ` for them anyway, inferred from Dolby Vision; Rust and ffprobe report none. The spec should state that a `dolbyVision` profile 5 stream implies PQ.
4. **`mp4-atom` drops unknown child boxes** when it decodes a sample entry, which loses `dvcC`/`dvvC`. The Rust probe searches the raw `moov` bytes instead. A `dvh1` sample entry comes back as `Codec::Unknown` with its raw body, which works.
5. **Matroska's Language defaults to `eng`** when the element is absent. A reader has to apply that default, as ffprobe and MediaInfo do.
6. **`lofty` reads no video fields at all.** It parsed 6 of the 117 files, all MP4s, and failed on every MKV. It's useful only for MP4 tags and covers (iTunes atoms and `covr`), which it read correctly.

## Matinee's prober today (from running its code)

- **No HDR on most files.** The VLCKit path never sets `dynamicRange`, so MKV files and every share file show no HDR or Dolby Vision label (0 of 107). Only local MP4 and MOV files, which go through AVFoundation, can get one.
- **Missing frame rates.** The VLCKit path returns no frame rate for 5 of 107 real files.
- **Wrong channel count for HE-AAC.** It reports HE-AAC parametric-stereo audio as 1 channel, so those files would be labelled "Mono".
- **AVFoundation path:** it detects Dolby Vision only by sample-entry type (`dvh1`, `dvhe`, `dvav`, `dva1`). Dolby Vision 8.1 in `hvc1` (with `dvvC`) comes back as HDR10, which is acceptable since the HDR10 base is real.

## Gaps left in the native Rust probe

All three are small and fixable:
- **MP4 forced and default flags, and track titles:** not read. ffmpeg stores forced and default in track flags and the track title in `udta` `name`.
- **HE-AAC parametric stereo:** reported as mono. Fixing it means parsing the AAC AudioSpecificConfig, including the SBR/PS extension, as ffprobe effectively does.
- **Frame rate definition:** the spec should define `frameRate` as nominal. That's the Matroska `DefaultDuration`, or the MP4 sample durations, with a first-frames fallback, which the probe already uses for one AV1 file. Variable-rate files then report their nominal rate, as ffprobe does.

## Not tested

- **Real Dolby Vision and HDR10+ releases:** only synthetic clips were used. Dolby Vision profile 7 (dual layer, with an enhancement-layer track) wasn't tested at all.
- **Other sources:** iTunes-purchased MP4s, AVI, MPEG-TS and WebM.
- **Network volumes:** reading from a network volume mounted on the server host. Bytes read are measured, but timing over the network isn't.
- **Windows.**
