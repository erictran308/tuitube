mod app;
mod bundle;
mod chapters;
mod commands;
mod config;
mod dearrow;
mod demo;
mod feed;
mod icons;
mod ids;
mod images;
mod links;
mod mpris;
mod output;
mod pepper;
mod player;
mod proxy;
mod screen;
mod settings;
mod shuffle;
mod sponsorblock;
mod store;
mod takeout;
mod text;
mod theme;
mod tmux;
mod tools;
mod typed;
mod ui;
mod video;
mod visualizer;
mod ytdlp;

use std::io::{Write, stdout};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    // Before any thread exists: PATH keeps only its absolute folders, as
    // `tools::on_path` does. tuitube names every program it starts by its
    // full path, but code it uses starts some by name (the image library's
    // `tmux`, crossterm's `tput`), and so does xdg-open: an empty or relative
    // entry would let one planted in the folder tuitube was started from run.
    #[cfg(unix)]
    match tools::absolute_path_entries(std::env::var_os("PATH")) {
        // SAFETY: first thing in main, before the runtime or any thread.
        Some(path) => unsafe { std::env::set_var("PATH", path) },
        // SAFETY: as above.
        None => unsafe { std::env::remove_var("PATH") },
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    config::load_dotenv();
    let mut args = std::env::args().skip(1);
    let mut check = false;
    let import = match args.next().as_deref() {
        None => None,
        Some("--check") => {
            check = true;
            None
        }
        Some("--demo") => return demo::run().await,
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
    proxy::init()?;
    let config = config::Config::load()?;
    let settings_path = settings::path(&config.data_dir);
    let (settings, unknown_settings) = settings::Settings::load_checked(&settings_path)?;
    let mut store = store::Store::open(&config.data_dir.join("tuitube.db"))?;

    if let Some(path) = import {
        return import_takeout(&mut store, &path, settings.history);
    }
    if check {
        return print_check(&config, &settings).await;
    }

    let mut tools = tools::Tools::find(
        settings.yt_dlp.as_deref(),
        settings.mpv.as_deref(),
        settings.deno.as_deref(),
    );
    tools.pepper_play = tools::Tools::find_pepper_play(settings.pepper_play.as_deref());
    let yt = ytdlp::YtDlp::new(&tools, &config.data_dir)?;
    let image_cache = config.data_dir.join("cache").join("images");
    config::private_dir(&image_cache)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let http = feed::client();
    // Linux's Now Playing: tuitube on the session bus, if there is one.
    let mpris = if settings.now_playing {
        mpris::Mpris::start(tx.clone()).await.ok()
    } else {
        None
    };

    let mut terminal = screen::take()?;
    // With animations on, the logo from now on, while the rest loads behind
    // it (`ui::splash`).
    let splash = settings
        .animations
        .then(|| ui::splash::show(&mut terminal, &settings.theme));
    // On macOS mpv runs as tuitube.app, so notch apps show what plays. Never
    // fatal, and slow when mpv changed (copied and signed again), so it's
    // made behind the splash; codesign and mpv write nowhere but to it.
    let bundle_failed = bundle::put_mpv_in(&mut tools, &config.data_dir);
    // Ask the terminal which image protocol it speaks and its cell size in
    // pixels. Must happen before key reading starts. Inside tmux this turns
    // the pane's allow-passthrough on: its old value is put back at the end.
    tmux::save();
    let picker = images::picker(settings.image_mode());

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
    if !unknown_settings.is_empty() {
        app.error(format!(
            "settings.toml: tuitube doesn't know {}, so it's ignored",
            unknown_settings.join(", ")
        ));
    }
    app.mpris = mpris;
    app.pepper = pepper::port();
    // macOS: what plays pauses when the headphones go (`pause_on_unplug`),
    // watched from the first video played.
    app.output_watch = app::OutputWatch::Later;
    app.effects = ui::effects::Effects::for_terminal();
    if let Some(since) = splash {
        app.keep_splash(since);
    }
    if let Some(why) = bundle_failed {
        app.error(format!(
            "mpv runs as itself, so notch apps may not show what plays: {why}"
        ));
    }
    let result = app.run(&mut terminal, rx).await;
    drop(app);

    screen::hand_back(terminal);
    result
}

/// `tuitube --check`: what tuitube found, to see why something is missing.
async fn print_check(config: &config::Config, settings: &settings::Settings) -> Result<()> {
    let mut tools = tools::Tools::find(
        settings.yt_dlp.as_deref(),
        settings.mpv.as_deref(),
        settings.deno.as_deref(),
    );
    let found = |path: &Option<std::path::PathBuf>| {
        path.as_deref()
            .map_or("not found".to_string(), config::shown)
    };
    // How mpv runs: as tuitube.app on macOS (`bundle`).
    let mut mpv = found(&tools.mpv);
    let bare = tools.mpv.clone();
    match bundle::put_mpv_in(&mut tools, &config.data_dir) {
        Some(why) => mpv = format!("{mpv}\n             runs as itself: {why}"),
        None if tools.mpv != bare => {
            mpv = format!("{mpv}\n             runs as {}", found(&tools.mpv));
        }
        None => {}
    }
    // yt-dlp's version, and whether it's old enough to update.
    let mut yt = found(&tools.yt_dlp);
    if let (Some(path), Some(yt_dlp)) =
        (&tools.yt_dlp, ytdlp::YtDlp::new(&tools, &config.data_dir)?)
    {
        match yt_dlp.version().await {
            Ok(version) => {
                yt = format!("{yt} ({version})");
                let today = chrono::Local::now().date_naive();
                if let Some(notice) = ytdlp::old_notice(&version, path, today) {
                    yt = format!("{yt}\n             {notice}");
                }
            }
            Err(e) => yt = format!("{yt} (doesn't run: {})", text::clean(&format!("{e:#}"))),
        }
    }
    // Whether what plays shows in Now Playing, and through what.
    let now_playing = if !settings.now_playing {
        "off (now_playing = false)".to_string()
    } else if mpris::AVAILABLE {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        match mpris::Mpris::start(tx).await {
            Ok(_) => "on the session bus (MPRIS)".to_string(),
            Err(e) => format!("not shown: {}", text::clean(&format!("{e:#}"))),
        }
    } else if player::NOW_PLAYING {
        "mpv's own (Control Center, notch apps)".to_string()
    } else {
        "not on this system yet".to_string()
    };
    // Where a video's picture plays: inside Pepper, or in mpv's window.
    let pepper_play = tools::Tools::find_pepper_play(settings.pepper_play.as_deref());
    let inline = if pepper::port().is_none() {
        "in mpv's window (not in Pepper, or in tmux)".to_string()
    } else if !settings.inline_video {
        "in mpv's window (inline_video = false)".to_string()
    } else if pepper_play.is_none() {
        "in mpv's window: pepper-play not found (a Pepper without it, or PEPPER_PLAY unset)"
            .to_string()
    } else {
        format!("in the terminal, through {}", found(&pepper_play))
    };
    let env = |name: &str| text::clean(&std::env::var(name).unwrap_or_default());
    // The image query needs the terminal: it's skipped when stdout isn't one.
    let images = if std::io::IsTerminal::is_terminal(&stdout()) {
        tmux::save();
        let answer = images::check(settings.image_mode());
        tmux::restore();
        answer
    } else {
        "not a terminal".into()
    };
    print(&format!(
        "tuitube {version}
data folder  {data}
yt-dlp       {yt}
deno         {deno}
mpv          {mpv}
now playing  {now_playing}
video        {inline}
proxy        {proxy}
terminal     TERM={term} TERM_PROGRAM={program} {tmux}
images       {images}
",
        version = env!("CARGO_PKG_VERSION"),
        data = config::shown(&config.data_dir),
        deno = found(&tools.deno),
        proxy = proxy::shown(),
        term = env("TERM"),
        program = env("TERM_PROGRAM"),
        tmux = if env("TMUX").is_empty() {
            ""
        } else {
            "(in tmux)"
        },
    ))
}

fn import_takeout(store: &mut store::Store, path: &str, history: bool) -> Result<()> {
    let found = takeout::find(std::path::Path::new(path))?;
    let imported = takeout::save(store, found, history)?;
    print(&format!(
        "{}. Start tuitube to see it.\n",
        imported.summary()
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

Usage: tuitube [-h | --help] [-V | --version] [--check] [--demo]
               [--import subscriptions.csv]

Inside the app, ? lists the keys and q quits. --check says what tuitube
found: yt-dlp, deno, mpv, and how your terminal draws images.
--demo shows made-up channels and videos, fetching and playing nothing.

Subscriptions come from Google Takeout (takeout.google.com → YouTube and
YouTube Music → subscriptions): press I in the app, or run
  tuitube --import path/to/subscriptions.csv

tuitube needs three programs, found on PATH:
  yt-dlp  talks to YouTube (searches, channels, what to play)
  deno    runs the JavaScript yt-dlp needs for YouTube
  mpv     plays videos
On macOS: brew install yt-dlp mpv (Homebrew's yt-dlp brings Deno)

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
  https_proxy, ...       one http:// proxy for tuitube, yt-dlp and mpv alike:
                         the first set of https_proxy, HTTPS_PROXY, all_proxy,
                         ALL_PROXY, http_proxy, HTTP_PROXY

tuitube isn't made or endorsed by YouTube or Google.
",
        version = env!("CARGO_PKG_VERSION"),
        data = config::shown(&config::data_dir()?),
    ))
}
