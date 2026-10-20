//! The terminal tuitube draws on: taken over at the start (raw mode, the
//! alternate screen, bracketed paste) and handed back at the end, normally
//! or from the panic hook. Its tab may be closed with tuitube in it: then
//! every write to it fails, and nothing here may panic (a panic in the
//! panic hook aborts) or report that, which is expected.

use std::io::{IsTerminal, Write, stdout};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::{DefaultTerminal, Terminal};

/// Whether stderr is the terminal too, not a file: then it goes when the
/// terminal does (`gone`).
#[cfg(unix)]
static STDERR_IS_IT: AtomicBool = AtomicBool::new(false);

/// A plain error, before the screen is taken over, when tuitube isn't run in
/// a terminal (piped, or from a script): there'd be nothing to draw on.
pub fn need() -> Result<()> {
    if !std::io::stdin().is_terminal() || !stdout().is_terminal() {
        bail!("tuitube needs a terminal to run in (tuitube --help says more)");
    }
    Ok(())
}

/// Takes the terminal over. The panic hook goes first, so a panic from here
/// on hands it back (ratatui's own, `ratatui::init`, isn't used: it prints
/// with `eprintln!`). What a step that failed leaves is put back.
pub fn take() -> Result<DefaultTerminal> {
    need()?;
    #[cfg(unix)]
    STDERR_IS_IT.store(same_terminal(), Ordering::Relaxed);
    set_panic_hook();
    // A pasted path arrives as one event instead of keystrokes.
    let taken = enable_raw_mode()
        .and_then(|()| execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste))
        .and_then(|()| Terminal::new(CrosstermBackend::new(stdout())));
    if taken.is_err() {
        restore();
    }
    Ok(taken?)
}

/// Hands the terminal back at the end, and tmux's pane setting. `terminal`
/// is forgotten, not dropped: its drop shows the cursor, done here already,
/// and prints a failure with `eprintln!`.
pub fn hand_back(terminal: DefaultTerminal) {
    std::mem::forget(terminal);
    restore();
    crate::tmux::restore();
}

/// A panic anywhere but in an image decoder (caught there) ends the app:
/// the terminal is handed back, then the message is printed without control
/// characters, since it can quote a video's title.
fn set_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        if crate::images::panic_is_contained() {
            return;
        }
        restore();
        crate::tmux::restore();
        // Not eprintln!, which panics when stderr is gone: a panic in this
        // hook aborts.
        let _ = writeln!(
            std::io::stderr(),
            "tuitube crashed: {}",
            crate::text::clean(&info.to_string())
        );
        std::process::exit(101);
    }));
}

/// Puts the terminal back as it was, each step on its own, so one that
/// fails doesn't stop the other, and says on stderr what failed. A
/// terminal that's gone has nothing to put back.
fn restore() {
    if gone() {
        return;
    }
    let raw = disable_raw_mode();
    let screen = execute!(
        stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    for err in [raw.err(), screen.err()].into_iter().flatten() {
        let _ = writeln!(
            std::io::stderr(),
            "tuitube: the terminal wasn't put back: {err}"
        );
    }
}

/// Whether the terminal is gone: its tab closed with tuitube in it (SIGHUP,
/// then a quit as with `q`). Every write to it fails with EIO then, even of
/// nothing, which a live terminal takes; `isatty` and `tcgetattr` still
/// work while tuitube leads its session, and a SIGHUP may come from `kill`.
/// Once gone, stdout points at /dev/null, and stderr too when it was the
/// terminal: what's still written on the way out, by tuitube or the code it
/// uses, then can't fail, where a failed `eprintln!` panics.
#[cfg(unix)]
pub fn gone() -> bool {
    use std::os::fd::AsRawFd;
    static GONE: AtomicBool = AtomicBool::new(false);
    if GONE.load(Ordering::Relaxed) {
        return true;
    }
    // SAFETY: a write of no bytes, from a pointer that's valid for none.
    let wrote = unsafe { libc::write(libc::STDOUT_FILENO, [0u8; 0].as_ptr().cast(), 0) };
    if wrote >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EIO) {
        return false;
    }
    GONE.store(true, Ordering::Relaxed);
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        // SAFETY: dup2 points the fd at /dev/null in place; Rust's stdout and
        // stderr only ever use the fd number.
        unsafe {
            libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO);
            if STDERR_IS_IT.load(Ordering::Relaxed) {
                libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO);
            }
        }
    }
    true
}

#[cfg(not(unix))]
pub fn gone() -> bool {
    false
}

/// Whether stderr is the terminal stdout is, rather than a file or another
/// terminal. Asked at the start: once the tab is gone, neither says.
#[cfg(unix)]
fn same_terminal() -> bool {
    let device = |fd| {
        // SAFETY: fstat only fills in the stat given.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        (unsafe { libc::fstat(fd, &mut stat) } == 0).then_some(stat.st_rdev)
    };
    std::io::stderr().is_terminal() && device(libc::STDERR_FILENO) == device(libc::STDOUT_FILENO)
}
