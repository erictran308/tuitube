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
    let chosen = choose(|name| std::env::var(name).ok())?;
    let _ = CHOSEN.set(chosen);
    Ok(())
}

/// The proxy in use, if any. Before [`init`] (tests, the demo): none.
pub fn get() -> Option<&'static Proxy> {
    CHOSEN.get().and_then(Option::as_ref)
}

fn choose(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Proxy>> {
    let Some((from, value)) = NAMES
        .iter()
        .find_map(|&name| Some((name, var(name).filter(|v| !v.trim().is_empty())?)))
    else {
        return Ok(None);
    };
    let value = value.trim();
    // `host:port` means an http proxy, as curl reads it.
    let url = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let parsed = reqwest::Url::parse(&url)
        .map_err(|_| anyhow::anyhow!("{from} doesn't hold a proxy address tuitube can read"))?;
    if parsed.scheme() != "http" || parsed.host_str().is_none() {
        anyhow::bail!(
            "{from} names a {} proxy; tuitube needs an http:// one, because mpv can't send \
             videos through any other kind and would go around it",
            parsed.scheme()
        );
    }
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

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
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
}
