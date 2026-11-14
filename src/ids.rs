//! Item and folder ids (spec §5, identity).
//!
//! An id is a hash of the shared folder's UUID and the path, so it survives index rebuilds, is
//! the same in every slice, and changes only when the file is moved or renamed on disk.

use base64::Engine;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const ITEM: u8 = 0;
const FOLDER: u8 = 1;

fn hash_id(tag: u8, folder: Uuid, path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(folder.as_bytes());
    hasher.update([tag]);
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    // 96 bits: collisions are negligible for any library size.
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..12])
}

/// The id of a file.
pub fn item_id(folder: Uuid, path: &str) -> String {
    hash_id(ITEM, folder, path)
}

/// The id of a folder; `path` is empty for the shared folder itself.
pub fn folder_id(folder: Uuid, path: &str) -> String {
    hash_id(FOLDER, folder, path)
}

/// A weak ETag derived from a hash of a body's contents, never from a counter (spec §5).
pub fn weak_etag(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("W/\"{hex}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        let folder: Uuid = "a41f0000-0000-4000-8000-000000000000".parse().unwrap();
        assert_eq!(item_id(folder, "a.flac"), item_id(folder, "a.flac"));
        assert_ne!(item_id(folder, "a.flac"), item_id(folder, "b.flac"));
        assert_ne!(item_id(folder, "a"), folder_id(folder, "a"));
        assert_ne!(item_id(folder, "a.flac"), item_id(Uuid::new_v4(), "a.flac"));
        assert_eq!(item_id(folder, "a.flac").len(), 16);
    }

    #[test]
    fn etags_are_weak_and_content_derived() {
        assert!(weak_etag(b"x").starts_with("W/\""));
        assert_eq!(weak_etag(b"x"), weak_etag(b"x"));
        assert_ne!(weak_etag(b"x"), weak_etag(b"y"));
    }
}
