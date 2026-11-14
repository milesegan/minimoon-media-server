#[path = "cli/console.rs"]
mod console;

use anyhow::{bail, Context, Result};
use minimoon_media_server::library::{
    IndexActivity, LibraryOptions, SharedFolderStatus, NOT_INDEXED_YET,
};
use minimoon_media_server::network::preferred_bind_ip;
use minimoon_media_server::scan::ScanOptions;
use minimoon_media_server::settings::AuthMode;
use minimoon_media_server::{MediaServer, ServerOptions};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing_subscriber::filter::LevelFilter;

const USAGE: &str = "usage: minimoon-media-server [options] [<folder>...]

Shares folders with the MiniMoon and Matinee apps over the MiniMoon Media Protocol.
Folders given here are added to the saved shared folders.

options:
  --data-dir <dir>              settings, slices and the index (default: the app-data folder)
  --port <port>                 port to listen on (default 41325)
  --bind <address>              address to listen on (default: every IPv4 interface)
  --name <name>                 display name shown to devices (saved)
  --pairing                     require a pairing code for new devices (saved)
  --open                        let any device on the network connect (saved)
  --exclude <subfolder>         publish a shared folder without this subfolder, e.g. one
                                it can't read (saved)
  --include <subfolder>         undo --exclude (saved)
  --follow-external-symlinks    follow links that point outside the shared folders
  --no-bonjour                  don't advertise over Bonjour
  --reindex                     re-read every file's metadata
  -v, --verbose                 log requests and scans";

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_GREEN: &str = "\x1b[32m";

#[derive(Debug, Default, PartialEq)]
struct CliOptions {
    folders: Vec<PathBuf>,
    data_dir: Option<PathBuf>,
    port: Option<u16>,
    bind: Option<std::net::IpAddr>,
    name: Option<String>,
    auth: Option<AuthMode>,
    exclude: Vec<PathBuf>,
    include: Vec<PathBuf>,
    follow_external_symlinks: bool,
    no_bonjour: bool,
    reindex: bool,
    verbose: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<CliOptions> {
    let mut options = CliOptions::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .with_context(|| format!("{flag} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--data-dir" => options.data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--port" => {
                options.port = Some(
                    value("--port")?
                        .parse()
                        .with_context(|| format!("--port needs a number\n\n{USAGE}"))?,
                )
            }
            "--bind" => {
                options.bind = Some(
                    value("--bind")?
                        .parse()
                        .with_context(|| format!("--bind needs an IP address\n\n{USAGE}"))?,
                )
            }
            "--name" => options.name = Some(value("--name")?),
            "--pairing" => options.auth = Some(AuthMode::Pairing),
            "--open" => options.auth = Some(AuthMode::None),
            "--exclude" => options.exclude.push(PathBuf::from(value("--exclude")?)),
            "--include" => options.include.push(PathBuf::from(value("--include")?)),
            "--follow-external-symlinks" => options.follow_external_symlinks = true,
            "--no-bonjour" => options.no_bonjour = true,
            "--reindex" => options.reindex = true,
            "--verbose" | "-v" => options.verbose = true,
            "--help" | "-h" => bail!(USAGE),
            _ if arg.starts_with('-') => bail!("unknown option {arg}\n\n{USAGE}"),
            _ => {
                let path = PathBuf::from(&arg);
                if !path.is_dir() {
                    bail!("not a folder: {arg}");
                }
                options.folders.push(path);
            }
        }
    }
    Ok(options)
}

/// A subfolder given on the command line, as the shared folder it's in and its path components
/// relative to that folder.
fn subfolder_of(
    folders: &[SharedFolderStatus],
    subfolder: &std::path::Path,
) -> Result<(uuid::Uuid, Vec<String>)> {
    let canonical = subfolder
        .canonicalize()
        .with_context(|| format!("can't find {}", subfolder.display()))?;
    for folder in folders {
        let Ok(root) = folder.path.canonicalize() else {
            continue;
        };
        if let Ok(relative) = canonical.strip_prefix(&root) {
            let components: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if components.is_empty() {
                bail!("{} is a shared folder itself", subfolder.display());
            }
            return Ok((folder.id, components));
        }
    }
    bail!("{} isn't inside a shared folder", subfolder.display())
}

/// Applies `--exclude` and `--include` to the saved excluded subfolders.
fn update_excluded(server: &MediaServer, options: &CliOptions) -> Result<()> {
    if options.exclude.is_empty() && options.include.is_empty() {
        return Ok(());
    }
    let folders = server.library().shared_folders();
    let mut excluded: std::collections::HashMap<uuid::Uuid, Vec<Vec<String>>> = folders
        .iter()
        .map(|folder| (folder.id, folder.excluded.clone()))
        .collect();
    for path in &options.exclude {
        let (id, components) = subfolder_of(&folders, path)?;
        let list = excluded.entry(id).or_default();
        if !list.contains(&components) {
            list.push(components);
        }
    }
    for path in &options.include {
        let (id, components) = subfolder_of(&folders, path)?;
        excluded.entry(id).or_default().retain(|c| *c != components);
    }
    for folder in &folders {
        let list = excluded.remove(&folder.id).unwrap_or_default();
        if list != folder.excluded {
            server.library().set_excluded(folder.id, list)?;
        }
    }
    Ok(())
}

/// The data directory the desktop app and the CLI share by default.
fn default_data_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("no app-data folder on this system; pass --data-dir")?
        .join("com.plastaq.sync-server")
        .join("media-server"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = parse_args(std::env::args().skip(1))?;
    // lofty warns about every oddity in every file it reads; only its errors are worth showing.
    let level = if options.verbose {
        LevelFilter::DEBUG
    } else {
        LevelFilter::WARN
    };
    let filter = tracing_subscriber::filter::Targets::new()
        .with_default(level)
        .with_target("lofty", LevelFilter::ERROR)
        .with_target("mdns_sd", LevelFilter::ERROR);
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(console::LogWriter::default))
        .with(filter)
        .init();

    let data_dir = match options.data_dir.clone() {
        Some(dir) => dir,
        None => default_data_dir()?,
    };
    let mut library = LibraryOptions::new(&data_dir);
    library.scan = ScanOptions {
        follow_external_symlinks: options.follow_external_symlinks,
    };
    let mut server_options = ServerOptions::new(library);
    if let Some(port) = options.port {
        server_options.port = port;
    }
    if let Some(bind) = options.bind {
        server_options.bind = bind;
    }
    server_options.advertise = !options.no_bonjour;
    let server = MediaServer::start(server_options).await?;

    for folder in &options.folders {
        server.library().add_shared_folder(folder, None)?;
    }
    server.library().ensure_default_slice()?;
    update_excluded(&server, &options)?;
    if let Some(name) = options.name.clone() {
        server.set_name(Some(name))?;
    }
    if let Some(auth) = options.auth {
        server.set_auth_mode(auth)?;
    }
    if options.reindex {
        server.library().reindex_everything()?;
    }
    if server.library().shared_folders().is_empty() {
        server.shutdown().await;
        bail!("no shared folders yet; pass one or more folders to share\n\n{USAGE}");
    }

    let terminal = std::io::stdout().is_terminal();
    let use_color = terminal && std::env::var_os("NO_COLOR").is_none();
    console::enable_progress(terminal);
    let settings = server.library().settings();
    let address = server.local_addr().await.context("not listening")?;
    let ip = if address.ip().is_unspecified() {
        preferred_bind_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| "no private LAN address".into())
    } else {
        address.ip().to_string()
    };
    let mut rows = vec![
        ("Name", settings.display_name()),
        (
            "Access",
            match settings.auth {
                AuthMode::None => "open to this network".into(),
                AuthMode::Pairing => "pairing code required".into(),
            },
        ),
        ("Data", data_dir.display().to_string()),
    ];
    let folders = server.library().shared_folders();
    for folder in &folders {
        let mut value = format!("{} ({})", folder.name, folder.path.display());
        if let Some(note) = unshared_note(folder) {
            value.push_str(" - ");
            value.push_str(&note);
        }
        rows.push(("Shared folder", value));
    }
    rows.push(("Address", format!("http://{ip}:{}", address.port())));
    println!("{}", paint("MiniMoon Media Server", ANSI_BOLD, use_color));
    println!("{}", table(&rows, use_color));
    println!("{}", paint("Press Ctrl-C to stop.", ANSI_DIM, use_color));

    let mut pairing = server.pairing_requests();
    let report = async {
        let library = server.library();
        let mut monitor = Monitor::new(&folders);
        let mut line = console::ProgressLine::default();
        loop {
            // The driver can hold the library's lock for a whole batch of reads.
            let (folders, idle) =
                tokio::task::block_in_place(|| (library.shared_folders(), library.is_idle()));
            let (messages, task) = monitor.update(&folders, idle);
            for message in messages {
                console::println(message);
            }
            let busy = task.is_some();
            console::set_progress(
                task.map(|task| line.render(&task, Instant::now(), console::width(), use_color)),
            );
            tokio::time::sleep(if busy || !idle {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            })
            .await;
        }
    };
    let codes = async {
        while let Ok(request) = pairing.recv().await {
            console::println(format!(
                "{} {} wants to connect ({} on {}). Code: {}",
                paint("Pairing:", ANSI_BOLD, use_color),
                request.client_name,
                request.app,
                request.platform,
                paint(&request.code, ANSI_GREEN, use_color)
            ));
        }
    };
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("failed to listen for Ctrl-C")?,
        _ = report => {}
        _ = codes => {}
    }
    console::set_progress(None);
    server.shutdown().await;
    println!("Stopped sharing.");
    Ok(())
}

/// Why a shared folder isn't being served, or `None` when it is.
fn unshared_note(folder: &SharedFolderStatus) -> Option<String> {
    if folder.available {
        return None;
    }
    let reason = folder.unavailable_reason.as_deref();
    Some(match (folder.indexed, reason) {
        (false, None | Some(NOT_INDEXED_YET)) => "not shared until it's indexed".into(),
        (false, Some(reason)) => format!("not shared: {reason}"),
        (true, reason) => format!(
            "offline, so its files can't be played: {}",
            reason.unwrap_or("unknown")
        ),
    })
}

/// Turns the library's folder states into messages and the progress line.
struct Monitor {
    /// Why each folder isn't shared, as last reported (`None` while it's shared).
    notes: HashMap<uuid::Uuid, Option<String>>,
    /// Whether the summary after the first indexing has been printed.
    summarized: bool,
}

impl Monitor {
    fn new(folders: &[SharedFolderStatus]) -> Self {
        Self {
            notes: folders.iter().map(|f| (f.id, unshared_note(f))).collect(),
            summarized: false,
        }
    }

    fn update(
        &mut self,
        folders: &[SharedFolderStatus],
        idle: bool,
    ) -> (Vec<String>, Option<console::Task>) {
        let mut messages = Vec::new();
        for folder in folders {
            let note = unshared_note(folder);
            if self.notes.get(&folder.id) == Some(&note) {
                continue;
            }
            match &note {
                None => messages.push(format!("{} is now shared.", folder.name)),
                Some(note) => messages.push(format!("{} is {note}.", folder.name)),
            }
            self.notes.insert(folder.id, note);
        }
        if idle && !self.summarized {
            self.summarized = true;
            messages.extend(summary(folders));
        }
        (messages, self.task(folders))
    }

    /// What the progress line shows: reads for a scan first, then listing, then re-reads for a
    /// newer reader.
    fn task(&self, folders: &[SharedFolderStatus]) -> Option<console::Task> {
        let mut reading = Vec::new();
        let (mut done, mut total) = (0, 0);
        for folder in folders {
            if let IndexActivity::Reading {
                done: folder_done,
                total: folder_total,
            } = folder.activity
            {
                reading.push(folder);
                done += folder_done;
                total += folder_total;
            }
        }
        if !reading.is_empty() {
            let label = if reading.iter().any(|f| !f.indexed) {
                format!("Indexing {}", names(&reading))
            } else {
                format!("Reading new files in {}", names(&reading))
            };
            return Some(console::Task {
                label,
                progress: Some((done, total)),
            });
        }
        let listing: Vec<_> = folders
            .iter()
            .filter(|f| f.activity == IndexActivity::Listing)
            .collect();
        if !listing.is_empty() {
            let label = if listing.iter().any(|f| !f.indexed) {
                format!("Listing files in {}", names(&listing))
            } else {
                format!("Checking {} for changes", names(&listing))
            };
            return Some(console::Task {
                label,
                progress: None,
            });
        }
        let updating: Vec<_> = folders.iter().filter(|f| f.outdated > 0).collect();
        if updating.is_empty() {
            return None;
        }
        let total: usize = updating
            .iter()
            .map(|f| f.outdated_total.max(f.outdated))
            .sum();
        let left: usize = updating.iter().map(|f| f.outdated).sum();
        Some(console::Task {
            label: format!("Updating metadata in {}", names(&updating)),
            progress: Some((total - left, total)),
        })
    }
}

/// Folder names for a progress label.
fn names(folders: &[&SharedFolderStatus]) -> String {
    match folders {
        [one] => one.name.clone(),
        [a, b] => format!("{} and {}", a.name, b.name),
        _ => format!("{} folders", folders.len()),
    }
}

/// What the first indexing found: the file count and any folder that couldn't be read.
fn summary(folders: &[SharedFolderStatus]) -> Vec<String> {
    let files: usize = folders.iter().map(|f| f.files).sum();
    let unreadable: usize = folders.iter().map(|f| f.unreadable).sum();
    let mut lines = vec![format!(
        "Indexed {} files{}.",
        format_count(files),
        if unreadable > 0 {
            format!(" ({} with unreadable metadata)", format_count(unreadable))
        } else {
            String::new()
        }
    )];
    for folder in folders {
        if folder.scan_errors.is_empty() {
            continue;
        }
        lines.push(format!(
            "{} couldn't be read completely, so {}:",
            folder.name,
            if folder.files > 0 {
                "its previously published files are kept"
            } else {
                "it isn't shared yet"
            }
        ));
        for error in &folder.scan_errors {
            lines.push(format!(
                "  {}: {}",
                folder.path.join(&error.path).display(),
                error.message
            ));
        }
        lines.push(
            "  Fix the permissions, or share it without a subfolder by running again \
             with --exclude <subfolder>."
                .into(),
        );
    }
    lines
}

fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

fn paint(text: impl AsRef<str>, code: &str, use_color: bool) -> String {
    if use_color {
        format!("{code}{}{ANSI_RESET}", text.as_ref())
    } else {
        text.as_ref().to_string()
    }
}

fn table(rows: &[(&str, String)], use_color: bool) -> String {
    let label_width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    let value_width = rows
        .iter()
        .map(|(_, value)| value.chars().count())
        .max()
        .unwrap_or(0);
    let border = paint(
        format!(
            "+{}+{}+",
            "-".repeat(label_width + 2),
            "-".repeat(value_width + 2)
        ),
        ANSI_DIM,
        use_color,
    );
    let mut lines = vec![border.clone()];
    for (label, value) in rows {
        let padding = value_width - value.chars().count();
        let code = if *label == "Address" {
            ANSI_GREEN
        } else {
            ANSI_BOLD
        };
        lines.push(format!(
            "| {} | {}{} |",
            paint(format!("{label:<label_width$}"), ANSI_CYAN, use_color),
            paint(value, code, use_color),
            " ".repeat(padding)
        ));
    }
    lines.push(border);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options_and_folders() {
        let options = parse_args(args(&[
            "--port",
            "9000",
            "--name",
            "Den",
            "--pairing",
            "-v",
            ".",
        ]))
        .unwrap();
        assert_eq!(options.port, Some(9000));
        assert_eq!(options.name.as_deref(), Some("Den"));
        assert_eq!(options.auth, Some(AuthMode::Pairing));
        assert!(options.verbose);
        assert_eq!(options.folders, vec![PathBuf::from(".")]);
    }

    #[test]
    fn parses_exclusions() {
        let options = parse_args(args(&["--exclude", "a/b", "--include", "c"])).unwrap();
        assert_eq!(options.exclude, vec![PathBuf::from("a/b")]);
        assert_eq!(options.include, vec![PathBuf::from("c")]);
        assert!(parse_args(args(&["--exclude"])).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_args(args(&["--unknown"])).is_err());
        assert!(parse_args(args(&["--port"])).is_err());
        assert!(parse_args(args(&["--port", "x"])).is_err());
        assert!(parse_args(args(&["/definitely/not/a/folder"])).is_err());
    }

    fn folder(name: &str) -> SharedFolderStatus {
        SharedFolderStatus {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            path: PathBuf::from("/m"),
            available: true,
            unavailable_reason: None,
            indexed: true,
            scanning: false,
            activity: IndexActivity::Idle,
            outdated: 0,
            outdated_total: 0,
            scan_errors: Vec::new(),
            failed_scans: 0,
            excluded: Vec::new(),
            files: 0,
            temporary_failures: 0,
            unreadable: 0,
        }
    }

    #[test]
    fn says_why_a_folder_is_not_shared() {
        let shared = folder("Music");
        assert_eq!(unshared_note(&shared), None);
        let new = SharedFolderStatus {
            available: false,
            indexed: false,
            unavailable_reason: Some(NOT_INDEXED_YET.into()),
            ..folder("Music")
        };
        assert_eq!(
            unshared_note(&new).as_deref(),
            Some("not shared until it's indexed")
        );
        let unplugged = SharedFolderStatus {
            unavailable_reason: Some("the drive isn't mounted".into()),
            ..new.clone()
        };
        assert_eq!(
            unshared_note(&unplugged).as_deref(),
            Some("not shared: the drive isn't mounted")
        );
        let offline = SharedFolderStatus {
            indexed: true,
            ..unplugged
        };
        assert_eq!(
            unshared_note(&offline).as_deref(),
            Some("offline, so its files can't be played: the drive isn't mounted")
        );
    }

    #[test]
    fn reports_a_first_index_until_the_folder_is_shared() {
        let mut music = SharedFolderStatus {
            available: false,
            indexed: false,
            unavailable_reason: Some(NOT_INDEXED_YET.into()),
            activity: IndexActivity::Listing,
            ..folder("Music")
        };
        let mut monitor = Monitor::new(std::slice::from_ref(&music));
        let (messages, task) = monitor.update(std::slice::from_ref(&music), false);
        assert!(messages.is_empty());
        assert_eq!(task.unwrap().label, "Listing files in Music");

        music.activity = IndexActivity::Reading {
            done: 10,
            total: 40,
        };
        let (_, task) = monitor.update(std::slice::from_ref(&music), false);
        let task = task.unwrap();
        assert_eq!(task.label, "Indexing Music");
        assert_eq!(task.progress, Some((10, 40)));

        music = SharedFolderStatus {
            files: 1234,
            ..folder("Music")
        };
        let (messages, task) = monitor.update(std::slice::from_ref(&music), true);
        assert_eq!(
            messages,
            vec!["Music is now shared.", "Indexed 1,234 files."]
        );
        assert_eq!(task, None);
        let (messages, _) = monitor.update(std::slice::from_ref(&music), true);
        assert!(messages.is_empty(), "the summary is printed once");
    }

    #[test]
    fn says_when_a_folder_being_indexed_loses_its_volume() {
        let mut music = SharedFolderStatus {
            available: false,
            indexed: false,
            unavailable_reason: Some(NOT_INDEXED_YET.into()),
            ..folder("Music")
        };
        let mut monitor = Monitor::new(std::slice::from_ref(&music));
        music.unavailable_reason = Some("the drive isn't mounted".into());
        let (messages, _) = monitor.update(std::slice::from_ref(&music), false);
        assert_eq!(
            messages,
            vec!["Music is not shared: the drive isn't mounted."]
        );
        let (messages, _) = monitor.update(std::slice::from_ref(&music), false);
        assert!(messages.is_empty());
    }

    #[test]
    fn shows_metadata_updates_against_the_count_at_the_start() {
        let mut music = SharedFolderStatus {
            outdated: 1000,
            outdated_total: 1000,
            ..folder("Music")
        };
        let mut monitor = Monitor::new(std::slice::from_ref(&music));
        let (_, task) = monitor.update(std::slice::from_ref(&music), false);
        let task = task.unwrap();
        assert_eq!(task.label, "Updating metadata in Music");
        assert_eq!(task.progress, Some((0, 1000)));
        music.outdated = 744;
        let (_, task) = monitor.update(std::slice::from_ref(&music), false);
        assert_eq!(task.unwrap().progress, Some((256, 1000)));
        music.outdated = 0;
        let (_, task) = monitor.update(std::slice::from_ref(&music), true);
        assert_eq!(task, None);
    }

    #[test]
    fn formats_counts_and_tables() {
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(
            table(
                &[("Name", "Den".into()), ("Address", "http://x:1".into())],
                false
            ),
            "+---------+------------+\n| Name    | Den        |\n| Address | http://x:1 |\n+---------+------------+"
        );
    }
}
