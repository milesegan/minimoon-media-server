//! The HTTP contract (spec §4–§6), against a real server on a free port.

use lofty::config::WriteOptions;
use lofty::id3::v2::Id3v2Tag;
use lofty::picture::{Picture, PictureType};
use lofty::tag::{Accessor, Tag, TagExt};
use minimoon_media_server::library::LibraryOptions;
use minimoon_media_server::settings::AuthMode;
use minimoon_media_server::{MediaServer, ServerOptions};
use reqwest::header;
use reqwest::StatusCode;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

struct Fixture {
    media: tempfile::TempDir,
    _data: tempfile::TempDir,
    server: MediaServer,
    base: String,
    client: reqwest::Client,
}

fn silent_wav() -> Vec<u8> {
    let sample_rate: u32 = 8_000;
    let data_len = sample_rate * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);
    wav
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([10, 120, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn write_media(root: &Path) {
    std::fs::create_dir_all(root.join("Album")).unwrap();
    let song = root.join("Album/01 Song.wav");
    std::fs::write(&song, silent_wav()).unwrap();
    let mut id3 = Id3v2Tag::new();
    id3.set_title("Song".into());
    id3.set_artist("Artist".into());
    let mut tag: Tag = id3.into();
    tag.push_picture(
        Picture::unchecked(png(300, 200))
            .pic_type(PictureType::CoverFront)
            .build(),
    );
    Id3v2Tag::from(tag)
        .save_to_path(&song, WriteOptions::new())
        .unwrap();
    std::fs::write(root.join("Album/cover.png"), png(800, 800)).unwrap();
    // Big enough that a transfer is still running when the test changes the file.
    let big: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("big.bin"), big).unwrap();
    std::fs::write(root.join("small.bin"), b"0123456789").unwrap();
}

async fn start() -> Fixture {
    let media = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_media(media.path());
    let mut library = LibraryOptions::new(data.path());
    library.workers = 2;
    let mut options = ServerOptions::new(library);
    options.port = 0;
    options.advertise = false;
    let server = MediaServer::start(options).await.unwrap();
    server
        .library()
        .add_shared_folder(media.path(), Some("Music".into()))
        .unwrap();
    server.library().ensure_default_slice().unwrap();
    let port = server.local_addr().await.unwrap().port();
    let fixture = Fixture {
        media,
        _data: data,
        server,
        base: format!("http://127.0.0.1:{port}"),
        client: reqwest::Client::new(),
    };
    fixture.wait_for_items(4).await;
    fixture
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn wait_for_items(&self, count: usize) {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(index) = self.try_index().await {
                if index["items"].as_array().map(Vec::len) == Some(count) {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "the scan didn't finish");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn server_info(&self) -> Value {
        self.client
            .get(self.url("/v1/server"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn slice_id(&self) -> String {
        self.server_info().await["slices"][0]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn try_index(&self) -> Result<Value, StatusCode> {
        let info = self.server_info().await;
        let Some(id) = info["slices"][0]["id"].as_str() else {
            return Err(StatusCode::NOT_FOUND);
        };
        let response = self
            .client
            .get(self.url(&format!("/v1/slices/{id}/index")))
            .send()
            .await
            .unwrap();
        if response.status() != StatusCode::OK {
            return Err(response.status());
        }
        Ok(response.json().await.unwrap())
    }

    async fn item_id(&self, path: &str) -> String {
        let index = self.try_index().await.unwrap();
        index["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == path)
            .unwrap_or_else(|| panic!("no {path} in {index}"))["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn server_info_lists_folders_and_slices() {
    let f = start().await;
    let info = f.server_info().await;
    assert_eq!(info["auth"], "none");
    assert_eq!(info["protocol"]["major"], 1);
    assert_eq!(info["software"]["name"], "minimoon-media-server");
    assert_eq!(info["sharedFolders"][0]["name"], "Music");
    assert_eq!(info["sharedFolders"][0]["available"], true);
    let slice = &info["slices"][0];
    assert_eq!(slice["name"], "Everything");
    assert_eq!(slice["available"], true);
    assert!(slice["etag"].as_str().unwrap().starts_with("W/\""));
    assert!(slice["filesEtag"].as_str().unwrap().starts_with("W/\""));
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn index_has_weak_etags_304s_gzip_and_both_views() {
    let f = start().await;
    let id = f.slice_id().await;
    let url = f.url(&format!("/v1/slices/{id}/index"));
    let response = f.client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    assert!(etag.starts_with("W/\""));
    assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
    let index: Value = response.json().await.unwrap();
    let song = index["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "01 Song.wav")
        .unwrap();
    assert_eq!(song["tags"]["title"], "Song");
    assert_eq!(song["tags"]["hasEmbeddedArt"], true);
    assert_eq!(song["audio"]["sampleRate"], 8000);
    assert_eq!(song["mime"], "audio/wav");
    assert!(index["folders"].as_array().unwrap().len() >= 2);

    // The same ETag answers 304 whichever encoding was used.
    let not_modified = f
        .client
        .get(&url)
        .header(header::IF_NONE_MATCH, &etag)
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

    let gzipped = f
        .client
        .get(&url)
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(gzipped.headers()[header::CONTENT_ENCODING], "gzip");
    assert_eq!(gzipped.headers()[header::ETAG].to_str().unwrap(), etag);
    let bytes = gzipped.bytes().await.unwrap();
    let mut plain = String::new();
    flate2::read::GzDecoder::new(&bytes[..])
        .read_to_string(&mut plain)
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&plain).unwrap(), index);

    let files = f
        .client
        .get(format!("{url}?view=files"))
        .send()
        .await
        .unwrap();
    let files_etag = files.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_ne!(files_etag, etag);
    let files: Value = files.json().await.unwrap();
    assert!(files.get("folders").is_none());
    assert_eq!(files["items"].as_array().unwrap().len(), 4);

    let bad = f
        .client
        .get(format!("{url}?view=all"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let bad: Value = bad.json().await.unwrap();
    assert_eq!(bad["error"]["code"], "unsupported_feature");

    let missing = f
        .client
        .get(f.url("/v1/slices/6b0e0000-0000-4000-8000-000000000000/index"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn content_honours_ranges_and_validators() {
    let f = start().await;
    let id = f.item_id("small.bin").await;
    let url = f.url(&format!("/v1/items/{id}/content"));

    let whole = f.client.get(&url).send().await.unwrap();
    assert_eq!(whole.status(), StatusCode::OK);
    assert_eq!(whole.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(whole.headers()[header::CONTENT_LENGTH], "10");
    assert!(whole.headers().contains_key(header::LAST_MODIFIED));
    assert!(whole.headers().get(header::CONTENT_ENCODING).is_none());
    let whole_last_modified = whole.headers()[header::LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_string();
    let etag = whole.headers()[header::ETAG].to_str().unwrap().to_string();
    assert!(
        etag.starts_with("\"10-") && etag.ends_with("-0\""),
        "{etag}"
    );
    assert_eq!(whole.bytes().await.unwrap(), "0123456789");

    let get = |range: &'static str| f.client.get(&url).header(header::RANGE, range).send();
    let part = get("bytes=2-5").await.unwrap();
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
    assert_eq!(part.bytes().await.unwrap(), "2345");
    assert_eq!(get("bytes=7-").await.unwrap().bytes().await.unwrap(), "789");
    assert_eq!(get("bytes=-3").await.unwrap().bytes().await.unwrap(), "789");
    let past = get("bytes=10-").await.unwrap();
    assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(past.headers()[header::CONTENT_RANGE], "bytes */10");
    let several = get("bytes=0-1,4-5").await.unwrap();
    assert_eq!(several.status(), StatusCode::OK);

    let stale = f
        .client
        .get(&url)
        .header(header::RANGE, "bytes=2-5")
        .header(header::IF_RANGE, "\"10-1-0\"")
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::OK);
    let by_date = f
        .client
        .get(&url)
        .header(header::RANGE, "bytes=2-5")
        .header(header::IF_RANGE, &whole_last_modified)
        .send()
        .await
        .unwrap();
    assert_eq!(
        by_date.status(),
        StatusCode::OK,
        "a date can't validate a range"
    );
    let current = f
        .client
        .get(&url)
        .header(header::RANGE, "bytes=2-5")
        .header(header::IF_RANGE, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(current.status(), StatusCode::PARTIAL_CONTENT);
    let cached = f
        .client
        .get(&url)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    let head = f.client.head(&url).send().await.unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
    assert_eq!(head.headers()[header::ETAG].to_str().unwrap(), etag);

    let wrong = f.client.post(f.url("/v1/server")).send().await.unwrap();
    assert_eq!(wrong.status(), StatusCode::METHOD_NOT_ALLOWED);
    let wrong: Value = wrong.json().await.unwrap();
    assert_eq!(wrong["error"]["code"], "method_not_allowed");
    let unknown = f
        .client
        .get(f.url("/v1/items/nope/content"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_changed_mid_transfer_is_cut_short() {
    let f = start().await;
    let id = f.item_id("big.bin").await;
    let mut response = f
        .client
        .get(f.url(&format!("/v1/items/{id}/content")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first = response.chunk().await.unwrap().unwrap();
    assert!(!first.is_empty());
    // A tagger rewrites the file in place and restores its mtime: the watcher reports the
    // write, so the transfer must not complete.
    let path = f.media.path().join("big.bin");
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(8 * 1024 * 1024 - 1)).unwrap();
        file.write_all(&[0xFF]).unwrap();
        file.set_modified(mtime).unwrap();
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut received = first.len();
    let result = loop {
        match response.chunk().await {
            Ok(Some(chunk)) => received += chunk.len(),
            Ok(None) => break Ok(received),
            Err(error) => break Err(error),
        }
    };
    assert!(
        result.is_err(),
        "the transfer completed ({received} bytes) although the file changed"
    );
    assert!(received < 8 * 1024 * 1024);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn art_lyrics_and_resizing() {
    let f = start().await;
    let song = f.item_id("Album/01 Song.wav").await;
    let art = f
        .client
        .get(f.url(&format!("/v1/items/{song}/art")))
        .send()
        .await
        .unwrap();
    assert_eq!(art.status(), StatusCode::OK);
    assert_eq!(art.headers()[header::CONTENT_TYPE], "image/png");
    let etag = art.headers()[header::ETAG].to_str().unwrap().to_string();
    let image = image::load_from_memory(&art.bytes().await.unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (300, 200));
    let cached = f
        .client
        .get(f.url(&format!("/v1/items/{song}/art")))
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);

    let small = f
        .client
        .get(f.url(&format!("/v1/items/{song}/art?maxSize=60")))
        .send()
        .await
        .unwrap();
    assert_eq!(small.headers()[header::CONTENT_TYPE], "image/jpeg");
    let image = image::load_from_memory(&small.bytes().await.unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (60, 40));

    let cover = f.item_id("Album/cover.png").await;
    let cover_small = f
        .client
        .get(f.url(&format!("/v1/items/{cover}/art?maxSize=100")))
        .send()
        .await
        .unwrap();
    let image = image::load_from_memory(&cover_small.bytes().await.unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (100, 100));

    let bad = f
        .client
        .get(f.url(&format!("/v1/items/{song}/art?maxSize=0")))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let lyrics = f
        .client
        .get(f.url(&format!("/v1/items/{song}/lyrics")))
        .send()
        .await
        .unwrap();
    assert_eq!(lyrics.status(), StatusCode::NOT_FOUND);
    let none = f.item_id("small.bin").await;
    let no_art = f
        .client
        .get(f.url(&format!("/v1/items/{none}/art")))
        .send()
        .await
        .unwrap();
    assert_eq!(no_art.status(), StatusCode::NOT_FOUND);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpublished_items_are_not_served() {
    let f = start().await;
    let id = f.item_id("small.bin").await;
    let slice: uuid::Uuid = f.slice_id().await.parse().unwrap();
    f.server
        .library()
        .set_slice_published(slice, false)
        .unwrap();
    f.server.library().wait_until_idle(WAIT);
    let info = f.server_info().await;
    assert_eq!(info["slices"], serde_json::json!([]));
    let response = f
        .client
        .get(f.url(&format!("/v1/items/{id}/content")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let index = f
        .client
        .get(f.url(&format!("/v1/slices/{slice}/index")))
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::NOT_FOUND);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_tokens_signed_urls_and_revocation() {
    let f = start().await;
    let id = f.item_id("big.bin").await;
    let slice = f.slice_id().await;
    f.server.set_auth_mode(AuthMode::Pairing).unwrap();

    let info = f.server_info().await;
    assert_eq!(info["auth"], "pairing");
    assert!(info.get("slices").is_none());
    assert!(info.get("sharedFolders").is_none());
    let index_url = f.url(&format!("/v1/slices/{slice}/index"));
    let unpaired = f.client.get(&index_url).send().await.unwrap();
    assert_eq!(unpaired.status(), StatusCode::UNAUTHORIZED);
    let unpaired: Value = unpaired.json().await.unwrap();
    assert_eq!(unpaired["error"]["code"], "not_paired");

    let mut codes = f.server.pairing_requests();
    let pairing: Value = f
        .client
        .post(f.url("/v1/pair"))
        .json(
            &serde_json::json!({"clientName": "Living Room", "app": "matinee", "platform": "tvOS"}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pairing_id = pairing["pairingId"].as_str().unwrap().to_string();
    assert_eq!(pairing["expiresIn"], 300);
    let request = codes.recv().await.unwrap();
    assert_eq!(request.client_name, "Living Room");
    let confirm_url = f.url(&format!("/v1/pair/{pairing_id}/confirm"));
    let wrong_code = if request.code == "000000" {
        "111111"
    } else {
        "000000"
    };
    let wrong = f
        .client
        .post(&confirm_url)
        .json(&serde_json::json!({"code": wrong_code}))
        .send()
        .await
        .unwrap();
    let wrong: Value = wrong.json().await.unwrap();
    assert_eq!(wrong["error"]["code"], "pairing_code_wrong");
    let paired: Value = f
        .client
        .post(&confirm_url)
        .json(&serde_json::json!({"code": request.code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = paired["token"].as_str().unwrap().to_string();
    let client_id = paired["clientId"].as_str().unwrap().to_string();
    assert_eq!(f.server.paired_devices().len(), 1);

    let bearer = format!("Bearer {token}");
    let index = f
        .client
        .get(&index_url)
        .header(header::AUTHORIZATION, &bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let info: Value = f
        .client
        .get(f.url("/v1/server"))
        .header(header::AUTHORIZATION, &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info["slices"].is_array());

    let signed: Value = f
        .client
        .get(f.url(&format!("/v1/items/{id}/content-url")))
        .header(header::AUTHORIZATION, &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let signed_url = f.url(signed["url"].as_str().unwrap());
    assert_eq!(signed["expiresIn"], 43200);
    let tampered = signed_url.replace("&exp=", "&exp=9");
    let tampered = f.client.get(&tampered).send().await.unwrap();
    assert_eq!(tampered.status(), StatusCode::UNAUTHORIZED);
    let ranged = f
        .client
        .get(&signed_url)
        .header(header::RANGE, "bytes=0-3")
        .send()
        .await
        .unwrap();
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);

    // Revoking closes the device's open download and stops its token and URLs.
    let mut download = f.client.get(&signed_url).send().await.unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    download.chunk().await.unwrap().unwrap();
    f.server.revoke_device(&client_id).unwrap();
    let mut received = 0usize;
    let result = loop {
        match download.chunk().await {
            Ok(Some(chunk)) => received += chunk.len(),
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    assert!(
        result.is_err(),
        "the download finished ({received} bytes) after revocation"
    );
    let revoked: Value = f
        .client
        .get(&index_url)
        .header(header::AUTHORIZATION, &bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revoked["error"]["code"], "token_revoked");
    let revoked_url = f.client.get(&signed_url).send().await.unwrap();
    assert_eq!(revoked_url.status(), StatusCode::UNAUTHORIZED);
    assert!(f.server.paired_devices().is_empty());

    // Turning pairing off makes the server ignore tokens.
    f.server.set_auth_mode(AuthMode::None).unwrap();
    let open = f.client.get(&index_url).send().await.unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_announce_slice_changes() {
    let f = start().await;
    let mut events = f.client.get(f.url("/v1/events")).send().await.unwrap();
    assert_eq!(events.headers()[header::CONTENT_TYPE], "text/event-stream");
    std::fs::write(f.media.path().join("new.bin"), b"new").unwrap();
    let deadline = Instant::now() + WAIT;
    let mut text = String::new();
    while !text.contains("event: slice-changed") {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "no slice-changed event: {text}");
        if let Ok(Ok(Some(chunk))) = tokio::time::timeout(remaining, events.chunk()).await {
            text.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    assert!(text.contains("\"filesEtag\""));
    f.server.shutdown().await;
}

/// Reads an SSE response until it ends or `wait` passes; true if it ended.
async fn stream_ends(response: &mut reqwest::Response, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, response.chunk()).await {
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None)) | Ok(Err(_)) => return true,
            Err(_) => return false,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_or_pausing_closes_event_streams() {
    let f = start().await;
    f.server.set_auth_mode(AuthMode::Pairing).unwrap();
    let mut codes = f.server.pairing_requests();
    let pairing: Value = f
        .client
        .post(f.url("/v1/pair"))
        .json(&serde_json::json!({"clientName": "Den", "app": "minimoon", "platform": "iOS"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request = codes.recv().await.unwrap();
    let paired: Value = f
        .client
        .post(f.url(&format!(
            "/v1/pair/{}/confirm",
            pairing["pairingId"].as_str().unwrap()
        )))
        .json(&serde_json::json!({"code": request.code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bearer = format!("Bearer {}", paired["token"].as_str().unwrap());
    let open_events = || async {
        let response = f
            .client
            .get(f.url("/v1/events"))
            .header(header::AUTHORIZATION, &bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response
    };

    let mut events = open_events().await;
    assert!(!stream_ends(&mut events, Duration::from_millis(300)).await);
    f.server
        .revoke_device(paired["clientId"].as_str().unwrap())
        .unwrap();
    assert!(
        stream_ends(&mut events, Duration::from_secs(5)).await,
        "the revoked device's event stream stayed open"
    );

    f.server.set_auth_mode(AuthMode::None).unwrap();
    let mut events = f.client.get(f.url("/v1/events")).send().await.unwrap();
    assert!(!stream_ends(&mut events, Duration::from_millis(300)).await);
    f.server.set_paused(true).await.unwrap();
    assert!(
        stream_ends(&mut events, Duration::from_secs(5)).await,
        "the event stream stayed open while paused"
    );
    f.server.shutdown().await;
}
