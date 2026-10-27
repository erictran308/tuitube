//! One proxy for everything: tuitube's own requests (feeds, pictures,
//! SponsorBlock), yt-dlp and mpv.
//!
//! Each of them reads the proxy variables its own way: reqwest takes
//! `HTTPS_PROXY` for https but never `HTTP_PROXY`, yt-dlp takes any of them
//! and falls back to the system's proxy settings, and mpv (FFmpeg) only
//! `http_proxy`. Left to themselves, a proxy meant to hide your address
//! could carry the feeds while mpv fetched the video straight from Google,
//! or the other way round. So tuitube picks one at start, from the first of
//! [`NAMES`] that's set, and hands that same proxy to all three: reqwest gets
//! it explicitly, the children get it under every name they read, and with
//! none set, yt-dlp is told to connect directly rather than use the system's
//! settings, which tuitube and mpv don't read.
//!
//! mpv can only use an `http://` proxy, so any other kind is refused at
//! start: playing would otherwise go around it. The proxy's address may hold
//! a password, so it's never printed or put on a command line.

use std::ffi::OsString;
use std::sync::OnceLock;

/// The variables a proxy is taken from, first set first.
pub const NAMES: [&str; 6] = [
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "http_proxy",
    "HTTP_PROXY",
];

/// Where the proxy came from, and its address (`http://…`, maybe with a
/// password).
#[derive(Clone, Debug, PartialEq)]
pub struct Proxy {
    pub from: &'static str,
    pub url: String,
}

static CHOSEN: OnceLock<Option<Proxy>> = OnceLock::new();

/// Picks the proxy, once, at start. An error names the variable but never
/// its value.
pub fn init() -> anyhow::Result<()> {
    let chosen = choose(|name| std::env::var_os(name))?;
    let _ = CHOSEN.set(chosen);
    Ok(())
}

/// The proxy in use, if any. Before [`init`] (tests, the demo): none.
pub fn get() -> Option<&'static Proxy> {
    CHOSEN.get().and_then(Option::as_ref)
}

fn choose(var: impl Fn(&str) -> Option<OsString>) -> anyhow::Result<Option<Proxy>> {
    // One that isn't text is set all the same: refused below, never read as
    // none, which would send everything direct. As tuinews's.
    let Some((from, value)) = NAMES.iter().find_map(|&name| {
        let value = var(name)?;
        let set = value.to_str().is_none_or(|v| !v.trim().is_empty());
        set.then_some((name, value))
    }) else {
        return Ok(None);
    };
    let Some(value) = value.to_str() else {
        anyhow::bail!("{from} doesn't hold a proxy address tuitube can read");
    };
    let value = value.trim();
    // `host:port` means an http proxy, as curl reads it.
    let (scheme, rest) = value.split_once("://").unwrap_or(("http", value));
    let unreadable = || anyhow::anyhow!("{from} doesn't hold a proxy address tuitube can read");
    // The URL parser drops a tab or a line's end, reads `\` as `/` and skips
    // a slash too many after `://`, but FFmpeg and yt-dlp read the address as
    // written: they'd take another proxy than reqwest's, or none.
    if rest.starts_with('/')
        || rest
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(unreadable());
    }
    let parsed = reqwest::Url::parse(&format!("{scheme}://{rest}")).map_err(|_| unreadable())?;
    if parsed.scheme() != "http" || parsed.host_str().is_none() {
        anyhow::bail!(
            "{from} names a {} proxy; tuitube needs an http:// one, because mpv can't send \
             videos through any other kind and would go around it",
            parsed.scheme()
        );
    }
    // Written as mpv's FFmpeg reads it: `http://` in lower case. An
    // `HTTP://` one it would take for no proxy, and go around it.
    let url = format!("http://{rest}");
    // Refused here, not when a client is built (`feed::builder`), where it
    // would leave tuitube's own requests to go direct.
    reqwest::Proxy::all(url.as_str()).map_err(|_| unreadable())?;
    Ok(Some(Proxy { from, url }))
}

/// The proxy for showing: where it's from and its host, without a password.
pub fn shown() -> String {
    match get() {
        None => "none (direct)".into(),
        Some(proxy) => {
            let host = reqwest::Url::parse(&proxy.url)
                .ok()
                .map(|u| {
                    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
                    format!("{}{port}", u.host_str().unwrap_or(""))
                })
                .unwrap_or_default();
            crate::text::clean(&format!("http://{host} (from {})", proxy.from))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_first_set_variable_is_the_proxy_for_everything() {
        assert_eq!(choose(env(&[])).unwrap(), None);
        assert_eq!(choose(env(&[("NO_PROXY", "*")])).unwrap(), None);
        let both = [("HTTPS_PROXY", "http://a:1"), ("https_proxy", "http://b:2")];
        assert_eq!(choose(env(&both)).unwrap().unwrap().url, "http://b:2");
        // An http proxy alone is used for https too, as yt-dlp and mpv do.
        let http = [("HTTP_PROXY", "proxy.example:3128")];
        assert_eq!(
            choose(env(&http)).unwrap(),
            Some(Proxy {
                from: "HTTP_PROXY",
                url: "http://proxy.example:3128".into()
            })
        );
    }

    #[test]
    fn a_proxy_mpv_cant_use_is_refused_without_showing_it() {
        for value in ["socks5h://127.0.0.1:9050", "https://u:secret@p.example"] {
            let error = choose(env(&[("ALL_PROXY", value)]))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("ALL_PROXY") && error.contains("http://"),
                "{error}"
            );
            assert!(
                !error.contains("secret") && !error.contains("9050"),
                "{error}"
            );
        }
        let bad = choose(env(&[("https_proxy", "http://")])).unwrap_err();
        assert!(bad.to_string().contains("https_proxy"), "{bad}");
    }

    #[test]
    fn the_proxy_is_written_as_mpv_reads_it() {
        let shouted = [("https_proxy", "HTTP://proxy.example:3128")];
        assert_eq!(
            choose(env(&shouted)).unwrap().unwrap().url,
            "http://proxy.example:3128",
            "FFmpeg takes only a lower-case http:// for a proxy"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_proxy_that_isnt_text_is_refused_not_taken_for_none() {
        use std::os::unix::ffi::OsStringExt;
        let garbled = |name: &str| {
            (name == "https_proxy").then(|| OsString::from_vec(b"http://pr\xffoxy:3128".to_vec()))
        };
        let error = choose(garbled).unwrap_err().to_string();
        assert!(error.contains("https_proxy"), "{error}");
    }

    #[test]
    fn a_proxy_the_children_would_read_otherwise_is_refused() {
        // Each of these the URL parser alone reads as proxy.example.
        for value in [
            "http:///proxy.example:3128",
            "//proxy.example:3128",
            "http://\\proxy.example:3128",
            "http://pro\txy.example:3128",
            "http://proxy.example\n:3128",
            // FFmpeg reads elsewhere.example: `\` doesn't end its host.
            "http://proxy.example\\@elsewhere.example:3128",
        ] {
            let url = value
                .strip_prefix("//")
                .map_or(value.into(), |v| format!("http:////{v}"));
            let parsed = reqwest::Url::parse(&url).unwrap();
            assert_eq!(parsed.host_str(), Some("proxy.example"), "{value:?}");
            let error = choose(env(&[("https_proxy", value)]))
                .unwrap_err()
                .to_string();
            assert!(error.contains("https_proxy"), "{value:?}: {error}");
        }
    }
}
