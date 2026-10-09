mod app;
mod config;
mod feed;
mod icons;
mod ids;
mod images;
mod player;
mod settings;
mod store;
mod takeout;
mod text;
mod theme;
mod tools;
mod ui;
mod video;
mod ytdlp;

use std::io::{Write, stdout};

use anyhow::{Context, Result, bail};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;

#[tokio::main]
async fn main() -> Result<()> {
    config::load_dotenv();
    let mut args = std::env::args().skip(1);
    let mut check = false;
    let import = match args.next().as_deref() {
        None => None,
        Some("--check") => {
            check = true;
            None
        }
        Some("-h" | "--help") => return print(&help()?),
        Some("-V" | "--version") => {
            return print(&format!("tuitube {}\n", env!("CARGO_PKG_VERSION")));
        }
        Some("--import") => Some(
            args.next()
                .context("--import needs the path to subscriptions.csv")?,
        ),
        Some(other) => bail!(
            "unknown argument {:?}; see tuitube --help",
            text::clean(other)
        ),
    };

    // Everything that can fail does so before the terminal is taken over,
    // so errors print as normal text.
    let config = config::Config::load()?;
    let settings_path = settings::path(&config.data_dir);
    let settings = settings::Settings::load(&settings_path)?;
    let mut store = store::Store::open(&config.data_dir.join("tuitube.db"))?;

    if let Some(path) = import {
        return import_subscriptions(&mut store, &path);
    }
    if check {
        return print_check(&config, &settings);
    }

    let tools = tools::Tools::find(
        settings.yt_dlp.as_deref(),
        settings.mpv.as_deref(),
        settings.deno.as_deref(),
    );
    let yt = ytdlp::YtDlp::new(&tools, &config.data_dir)?;
    let image_cache = config.data_dir.join("cache").join("images");
    config::private_dir(&image_cache)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let http = feed::client();

    let mut terminal = ratatui::init();
    // A panic anywhere but in an image decoder (caught there) ends the app:
    // the terminal is put back, then the message is printed without control
    // characters, since it can quote a video's title.
    std::panic::set_hook(Box::new(|info| {
        if images::panic_is_contained() {
            return;
        }
        let _ = execute!(stdout(), DisableBracketedPaste);
        ratatui::restore();
        eprintln!("tuitube crashed: {}", text::clean(&info.to_string()));
        std::process::exit(101);
    }));
    // Ask the terminal which image protocol it speaks and its cell size in
    // pixels. Must happen before key reading starts.
    let picker = images::picker(settings.image_mode());
    // A pasted path arrives as one event instead of keystrokes.
    execute!(stdout(), EnableBracketedPaste)?;

    let images = images::Images::new(picker, tx.clone(), http.clone(), Some(image_cache));
    let mut app = app::App::new(
        store,
        settings,
        Some(settings_path),
        tools,
        yt,
        http,
        tx,
        images,
    );
    let result = app.run(&mut terminal, rx).await;
    drop(app);

    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

/// `tuitube --check`: what tuitube found, to see why something is missing.
fn print_check(config: &config::Config, settings: &settings::Settings) -> Result<()> {
    let tools = tools::Tools::find(
        settings.yt_dlp.as_deref(),
        settings.mpv.as_deref(),
        settings.deno.as_deref(),
    );
    let found = |path: &Option<std::path::PathBuf>| {
        path.as_deref()
            .map_or("not found".to_string(), config::shown)
    };
    let env = |name: &str| text::clean(&std::env::var(name).unwrap_or_default());
    // The image query needs the terminal: it's skipped when stdout isn't one.
    let images = if std::io::IsTerminal::is_terminal(&stdout()) {
        images::check(settings.image_mode())
    } else {
        "not a terminal".into()
    };
    print(&format!(
        "tuitube {version}
data folder  {data}
yt-dlp       {yt}
deno         {deno}
mpv          {mpv}
terminal     TERM={term} TERM_PROGRAM={program} {tmux}
images       {images}
",
        version = env!("CARGO_PKG_VERSION"),
        data = config::shown(&config.data_dir),
        yt = found(&tools.yt_dlp),
        deno = found(&tools.deno),
        mpv = found(&tools.mpv),
        term = env("TERM"),
        program = env("TERM_PROGRAM"),
        tmux = if env("TMUX").is_empty() {
            ""
        } else {
            "(in tmux)"
        },
    ))
}

fn import_subscriptions(store: &mut store::Store, path: &str) -> Result<()> {
    let channels = takeout::read(std::path::Path::new(path))?;
    for (id, name) in &channels {
        store.subscribe(id, name)?;
    }
    print(&format!(
        "Imported {} subscriptions. Start tuitube to see their videos.\n",
        channels.len()
    ))
}

/// Writes to stdout. A reader that stops early (`tuitube --help | head -1`)
/// is fine, where `println!` would panic on the closed pipe.
fn print(text: &str) -> Result<()> {
    match stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

fn help() -> Result<String> {
    Ok(format!(
        "tuitube {version}
YouTube in your terminal: your subscriptions, search and playback, with
arrow keys or vim keys. No Google account, no login.

Usage: tuitube [-h | --help] [-V | --version] [--check]
               [--import subscriptions.csv]

Inside the app, ? lists the keys and q quits. --check says what tuitube
found: yt-dlp, deno, mpv, and how your terminal draws images.

Subscriptions come from Google Takeout (takeout.google.com → YouTube and
YouTube Music → subscriptions): press I in the app, or run
  tuitube --import path/to/subscriptions.csv

tuitube needs three programs, found on PATH:
  yt-dlp  talks to YouTube (searches, channels, what to play)
  deno    runs the JavaScript yt-dlp needs for YouTube
  mpv     plays videos
On macOS: brew install yt-dlp deno mpv

Your subscriptions, Watch later, history and settings are kept only on
this computer, in:
  {data}

Environment:
  TT_DATA_DIR            keep them somewhere else
  TT_YT_DLP, TT_MPV,     where those programs are, if not on PATH
  TT_DENO
  TT_IMAGES              how images are drawn: auto, kitty, sixel, iterm2
                         or blocks (also `images` in settings.toml)
  TT_ICONS               which icons: auto, nerd (Nerd Font) or plain
                         (also `icons` in settings.toml)

tuitube isn't made or endorsed by YouTube or Google.
",
        version = env!("CARGO_PKG_VERSION"),
        data = config::shown(&config::data_dir()?),
    ))
}
