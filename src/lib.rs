//! minimoon-media-server: the LAN media server behind the MiniMoon and Matinee apps, speaking
//! the MiniMoon Media Protocol (MMP) v1 (`docs/minimoon-media-protocol-v1.md`).

pub mod bonjour;
pub mod http;
pub mod ids;
pub mod index;
pub mod library;
pub mod model;
pub mod network;
pub mod published;
pub mod reader;
pub mod scan;
pub mod server;
pub mod settings;
pub mod sleep;
pub mod slices;
pub mod volume;
pub mod watch;

pub use library::{Library, LibraryOptions};
pub use server::{MediaServer, ServerOptions};
