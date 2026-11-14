use anyhow::{bail, Context, Result};
use minimoon_media_server::library::{LibraryOptions, SharedFolderStatus};
use minimoon_media_server::network::preferred_bind_ip;
use minimoon_media_server::scan::ScanOptions;
use minimoon_media_server::settings::AuthMode;
use minimoon_media_server::{MediaServer, ServerOptions};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;
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
        .with(tracing_subscriber::fmt::layer())
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

    let use_color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
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
    for folder in server.library().shared_folders() {
        rows.push((
            "Shared folder",
            format!("{} ({})", folder.name, folder.path.display()),
        ));
    }
    rows.push(("Address", format!("http://{ip}:{}", address.port())));
    println!("{}", paint("MiniMoon Media Server", ANSI_BOLD, use_color));
    println!("{}", table(&rows, use_color));
    println!("{}", paint("Press Ctrl-C to stop.", ANSI_DIM, use_color));

    let mut pairing = server.pairing_requests();
    let report = async {
        let library = server.library();
        tokio::task::block_in_place(|| library.wait_until_idle(Duration::from_secs(24 * 60 * 60)));
        let folders = library.shared_folders();
        let files: usize = folders.iter().map(|f| f.files).sum();
        let unreadable: usize = folders.iter().map(|f| f.unreadable).sum();
        println!(
            "Indexed {} files{}.",
            format_count(files),
            if unreadable > 0 {
                format!(" ({} with unreadable metadata)", format_count(unreadable))
            } else {
                String::new()
            }
        );
        for folder in &folders {
            if !folder.scan_errors.is_empty() {
                println!(
                    "{} couldn't be read completely, so {}:",
                    folder.name,
                    if folder.files > 0 {
                        "its previously published files are kept"
                    } else {
                        "it isn't shared yet"
                    }
                );
                for error in &folder.scan_errors {
                    println!(
                        "  {}: {}",
                        folder.path.join(&error.path).display(),
                        error.message
                    );
                }
                println!(
                    "  Fix the permissions, or share it without a subfolder by running again \
                     with --exclude <subfolder>."
                );
            } else if !folder.available {
                println!(
                    "{} is unavailable: {}",
                    folder.name,
                    folder.unavailable_reason.as_deref().unwrap_or("unknown")
                );
            }
        }
        std::future::pending::<()>().await
    };
    let codes = async {
        while let Ok(request) = pairing.recv().await {
            println!(
                "{} {} wants to connect ({} on {}). Code: {}",
                paint("Pairing:", ANSI_BOLD, use_color),
                request.client_name,
                request.app,
                request.platform,
                paint(&request.code, ANSI_GREEN, use_color)
            );
        }
    };
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("failed to listen for Ctrl-C")?,
        _ = report => {}
        _ = codes => {}
    }
    server.shutdown().await;
    println!("Stopped sharing.");
    Ok(())
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
