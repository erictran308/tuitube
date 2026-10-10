//! `tuitube --demo`: the real UI filled with made-up channels and videos,
//! for screenshots and for a look around. Nothing is fetched or played,
//! and the data folder isn't touched: the store lives in memory, and the
//! thumbnails and channel photos are drawn here.
//!
//! `cargo test -- --ignored export_hero_screen` writes one frame of it to
//! `target/hero/screen.json`, which `tools/hero.py` turns into
//! `docs/hero.png`.

use std::io::stdout;

use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use image::{DynamicImage, Rgb, RgbImage};
use ratatui_image::picker::Picker;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::app::{App, AppEvent, now};
use crate::ids::{ChannelId, VideoId};
use crate::images::{self, Images, Subject};
use crate::settings::Settings;
use crate::store::Store;
use crate::tools::Tools;
use crate::video::Video;

/// Made-up channels: the sidebar lists them by name.
const CHANNELS: [&str; 22] = [
    "Bitwise Boulevard",
    "Coastline Kitchen",
    "Ferrous Labs",
    "Field Notes Audio",
    "Glass & Grain",
    "Keyboard Garage",
    "Lantern Lo-Fi",
    "Midnight Compile",
    "Night Signal",
    "Orbit Explained",
    "Paper Planes Studio",
    "Pocket Synth Club",
    "Quiet Mornings",
    "Ridge & Trail",
    "Silicon Sketches",
    "Slow Roast Coffee",
    "Small Batch Bread",
    "Terminal Tales",
    "Tiny Home Tales",
    "Vim Valley",
    "Wander North",
    "Workbench Weekly",
];

/// What a made-up video is: its channel, title, description, how long ago
/// it came out (minutes), views, length (seconds; 0 for live) and how its
/// thumbnail looks.
struct Fake {
    channel: &'static str,
    title: &'static str,
    description: &'static str,
    ago: i64,
    views: u64,
    length: u32,
    look: Look,
}

#[derive(Clone, Copy)]
enum Look {
    Code,
    Bread,
    Ridge,
    Chip,
    Keys,
    Night,
    Vim,
    Rocket,
    Plane,
    Flow,
    Wood,
    Synth,
    Cabin,
    Coffee,
    Orbit,
}

const HOUR: i64 = 60;
const DAY: i64 = 24 * HOUR;

const VIDEOS: [Fake; 18] = [
    Fake {
        channel: "Ferrous Labs",
        title: "I rewrote my terminal emulator in Rust (it's fast now)",
        description: "Six months, two rewrites and one very patient borrow checker. Here's what changed.",
        ago: 2 * HOUR,
        views: 214_000,
        length: 18 * 60 + 42,
        look: Look::Code,
    },
    Fake {
        channel: "Coastline Kitchen",
        title: "The 10-minute focaccia anyone can make",
        description: "No mixer, no overnight rise: a hot pan, good olive oil and a pinch of flaky salt.",
        ago: 5 * HOUR,
        views: 1_340_000,
        length: 12 * 60 + 7,
        look: Look::Bread,
    },
    Fake {
        channel: "Lantern Lo-Fi",
        title: "lofi beats to refactor to, rainy night radio",
        description: "Soft keys, slow drums and rain on the window, all night long.",
        ago: 7 * HOUR,
        views: 8_400,
        length: 0,
        look: Look::Night,
    },
    Fake {
        channel: "Silicon Sketches",
        title: "How a CPU actually adds two numbers",
        description: "From a single transistor to a full adder, drawn out one gate at a time.",
        ago: DAY + 3 * HOUR,
        views: 642_000,
        length: 15 * 60 + 33,
        look: Look::Chip,
    },
    Fake {
        channel: "Ridge & Trail",
        title: "Sunrise on the ridge: a 5 AM summit hike",
        description: "Headlamps on at 3:40, coffee at the top, and the best light of the year.",
        ago: DAY + 9 * HOUR,
        views: 86_000,
        length: 24 * 60 + 16,
        look: Look::Ridge,
    },
    Fake {
        channel: "Keyboard Garage",
        title: "I built a split keyboard from scratch",
        description: "Hand-wired, 42 keys, and a case milled from a single block of walnut.",
        ago: 2 * DAY,
        views: 377_000,
        length: 31 * 60 + 8,
        look: Look::Keys,
    },
    Fake {
        channel: "Vim Valley",
        title: "Vim motions I wish I'd learned on day one",
        description: "Twelve motions that turn editing from typing into talking.",
        ago: 2 * DAY + 6 * HOUR,
        views: 158_000,
        length: 9 * 60 + 51,
        look: Look::Vim,
    },
    Fake {
        channel: "Night Signal",
        title: "Why every rocket engine needs a gimbal",
        description: "Steering a 60-tonne column of fire, explained with a broom and a hairdryer.",
        ago: 3 * DAY,
        views: 905_000,
        length: 21 * 60 + 47,
        look: Look::Rocket,
    },
    Fake {
        channel: "Paper Planes Studio",
        title: "This paper plane flies 30 meters",
        description: "Five folds, one trick with the wing tips, and a lot of hallway test flights.",
        ago: 4 * DAY,
        views: 2_100_000,
        length: 8 * 60 + 12,
        look: Look::Plane,
    },
    Fake {
        channel: "Midnight Compile",
        title: "Async Rust without the pain",
        description: "Futures, executors and pinning, without a single diagram of a state machine.",
        ago: 5 * DAY,
        views: 98_000,
        length: 27 * 60 + 30,
        look: Look::Flow,
    },
    Fake {
        channel: "Glass & Grain",
        title: "Restoring a rusty 1970s hand plane",
        description: "Electrolysis, a new tote from cherry offcuts, and the first shaving in decades.",
        ago: 6 * DAY,
        views: 430_000,
        length: 19 * 60 + 2,
        look: Look::Wood,
    },
    Fake {
        channel: "Pocket Synth Club",
        title: "A whole track on a pocket synth, start to finish",
        description: "Drums, bass and a melody, all from a synth smaller than a phone.",
        ago: 6 * DAY + 12 * HOUR,
        views: 61_000,
        length: 14 * 60 + 26,
        look: Look::Synth,
    },
    Fake {
        channel: "Wander North",
        title: "48 hours alone in a snow cabin",
        description: "No signal, a wood stove, and the quietest night I've ever had.",
        ago: 8 * DAY,
        views: 1_800_000,
        length: 35 * 60 + 44,
        look: Look::Cabin,
    },
    Fake {
        channel: "Slow Roast Coffee",
        title: "Better coffee with the gear you already have",
        description: "Grind size, water and patience matter more than a new machine.",
        ago: 9 * DAY,
        views: 263_000,
        length: 11 * 60 + 40,
        look: Look::Coffee,
    },
    Fake {
        channel: "Orbit Explained",
        title: "Orbits, explained with a bucket of water",
        description: "Why the space station is falling all the time, and never lands.",
        ago: 13 * DAY,
        views: 712_000,
        length: 13 * 60 + 5,
        look: Look::Orbit,
    },
    Fake {
        channel: "Terminal Tales",
        title: "10 TUI apps worth trying this fall",
        description: "File managers, git clients and one very good YouTube client.",
        ago: 15 * DAY,
        views: 54_000,
        length: 16 * 60 + 20,
        look: Look::Code,
    },
    Fake {
        channel: "Small Batch Bread",
        title: "Sourdough starter, day by day",
        description: "Flour, water and two weeks of watching a jar on the counter.",
        ago: 18 * DAY,
        views: 129_000,
        length: 17 * 60 + 58,
        look: Look::Bread,
    },
    Fake {
        channel: "Workbench Weekly",
        title: "The only five hand tools you need to start",
        description: "A saw, a plane, a chisel, a square and a mallet. Everything else can wait.",
        ago: 21 * DAY,
        views: 344_000,
        length: 20 * 60 + 33,
        look: Look::Wood,
    },
];

/// A made-up channel id, the same for a name every time.
fn channel_id(name: &str) -> ChannelId {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let id: String = (0..22)
        .map(|i| {
            for b in name.bytes().chain([i as u8]) {
                hash = (hash ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
            }
            CHARS[(hash % CHARS.len() as u64) as usize] as char
        })
        .collect();
    ChannelId::parse(&format!("UC{id}")).expect("a made-up id is a channel id")
}

fn video_id(i: usize) -> VideoId {
    VideoId::parse(&format!("demoVideo{i:02}")).expect("a made-up id is a video id")
}

/// The demo's videos as the store keeps them.
fn videos(now: i64) -> Vec<Video> {
    VIDEOS
        .iter()
        .enumerate()
        .map(|(i, fake)| Video {
            id: video_id(i),
            title: fake.title.into(),
            channel_id: Some(channel_id(fake.channel)),
            channel: fake.channel.into(),
            description: fake.description.into(),
            published: Some(now - fake.ago * 60),
            views: Some(fake.views),
            duration: (fake.length > 0).then_some(fake.length),
            short: false,
            live: fake.length == 0,
            upcoming: false,
            thumbnail: None,
        })
        .collect()
}

/// The app as the demo shows it: subscribed to the made-up channels, Home
/// open with the second video selected, a video playing from the Jukebox
/// (sound only), three more in it, one video half watched, one watched, one
/// saved for later.
pub fn demo_app(picker: Picker, tx: UnboundedSender<AppEvent>) -> App {
    let now = now();
    let playing = Video {
        id: VideoId::parse("demoPlaying").expect("a made-up id is a video id"),
        title: "Rainy night jazz for deep focus".into(),
        channel_id: Some(channel_id("Lantern Lo-Fi")),
        channel: "Lantern Lo-Fi".into(),
        description: "Piano, upright bass and brushes, with rain on the window.".into(),
        published: Some(now - 3 * DAY * 60),
        views: Some(1_200_000),
        duration: Some(3600),
        short: false,
        live: false,
        upcoming: false,
        thumbnail: None,
    };
    let mut store = Store::in_memory();
    for name in CHANNELS {
        let id = channel_id(name);
        let _ = store.subscribe(&id, name);
        let _ = store.feed_checked(&id, now);
        let photo = format!("https://yt3.ggpht.com/demo-{id}=s176");
        let _ = store.set_avatar(&id, name, Some(&photo), now);
    }
    let videos = videos(now);
    let _ = store.save_videos(&videos);
    let _ = store.record_watch(&videos[4].id, now - 3600, Some(870.0), Some(1456.0));
    let _ = store.record_watch(&videos[3].id, now - 7200, Some(933.0), Some(933.0));
    let _ = store.toggle_watch_later(&videos[5].id, now);
    // Kept with no date, so Home doesn't list it.
    let _ = store.save_videos(&[Video {
        published: None,
        ..playing.clone()
    }]);
    let _ = store.add_to_jukebox(&playing.id);
    for i in [2, 11, 9] {
        let _ = store.add_to_jukebox(&videos[i].id);
    }

    let mut images = Images::new(picker, tx.clone(), crate::feed::client(), None);
    images.go_offline();
    for (i, fake) in VIDEOS.iter().enumerate() {
        images.preload(
            Subject::Thumbnail(video_id(i)),
            thumbnail(fake.look, i).into(),
        );
    }
    images.preload(
        Subject::Thumbnail(playing.id.clone()),
        thumbnail(VIDEOS[2].look, VIDEOS.len()).into(),
    );
    for (i, name) in CHANNELS.iter().enumerate() {
        images.preload(Subject::Avatar(channel_id(name)), avatar(i));
    }

    let mut app = App::new(
        store,
        Settings::default(),
        None,
        Tools::default(),
        None,
        crate::feed::client(),
        tx,
        images,
    );
    app.demo = true;
    app.status = None;
    app.selected = 1;
    app.show_playing(playing, 1421.0, 3600.0, true);
    app
}

pub async fn run() -> Result<()> {
    crate::need_terminal()?;
    let mut terminal = ratatui::init();
    std::panic::set_hook(Box::new(|info| {
        if images::panic_is_contained() {
            return;
        }
        let _ = execute!(stdout(), DisableBracketedPaste, crossterm::cursor::Show);
        ratatui::restore();
        crate::tmux::restore();
        eprintln!("tuitube crashed: {}", crate::text::clean(&info.to_string()));
        std::process::exit(101);
    }));
    crate::tmux::save();
    let picker = images::picker(Settings::default().image_mode());
    execute!(stdout(), EnableBracketedPaste)?;
    let (tx, rx) = unbounded_channel();
    let mut app = demo_app(picker, tx);
    let result = app.run(&mut terminal, rx).await;
    drop(app);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::tmux::restore();
    result
}

// The pictures, drawn here.

type Color = [u8; 3];

fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| {
        (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8
    })
}

/// A number from 0 to 1 that looks random but is the same every time.
fn noise(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

const W: u32 = 640;
const H: u32 = 360;

/// A thumbnail in the look given, 640×360.
fn thumbnail(look: Look, seed: usize) -> RgbImage {
    let seed = seed as u32;
    let (w, h) = (W as f32, H as f32);
    match look {
        Look::Ridge | Look::Cabin => {
            let (sky, ridges, sun) = if matches!(look, Look::Ridge) {
                (
                    [[255, 179, 120], [255, 222, 173]],
                    [[143, 98, 140], [96, 64, 110], [52, 38, 74]],
                    [255, 241, 200],
                )
            } else {
                (
                    [[28, 44, 84], [120, 150, 196]],
                    [[226, 236, 248], [168, 190, 220], [92, 112, 150]],
                    [250, 250, 255],
                )
            };
            landscape(sky, ridges, sun)
        }
        Look::Night | Look::Synth => {
            let (top, bottom, sun) = if matches!(look, Look::Night) {
                ([24, 18, 54], [96, 52, 120], [255, 214, 165])
            } else {
                ([20, 10, 48], [228, 64, 140], [255, 196, 64])
            };
            RgbImage::from_fn(W, H, |x, y| {
                let (u, v) = (x as f32 / w, y as f32 / h);
                if v > 0.62 {
                    // A grid going off into the distance.
                    let depth = (v - 0.62) / 0.38;
                    let line_y = ((1.0 / (depth + 0.05)) * 3.0).fract() < 0.08;
                    let line_x =
                        (((u - 0.5) / (depth + 0.05)) * 6.0).fract().abs() < 0.04 * (1.0 + depth);
                    let ground = mix([16, 8, 36], [40, 16, 70], depth);
                    return Rgb(if line_x || line_y {
                        mix(ground, sun, 0.7)
                    } else {
                        ground
                    });
                }
                let sky = mix(top, bottom, v / 0.62);
                let d = ((u - 0.5) * w / h).hypot(v - 0.45);
                if d < 0.2 && (v < 0.42 || ((v * 40.0) as u32).is_multiple_of(2)) {
                    return Rgb(mix(sun, [255, 100, 120], (v - 0.25) * 2.5));
                }
                let star = noise(x, y + seed) > 0.997;
                Rgb(if star {
                    [255, 255, 255]
                } else {
                    mix(sky, sun, 0.25 * (-d * 4.0).exp())
                })
            })
        }
        Look::Code | Look::Vim | Look::Flow => {
            let accent = match look {
                Look::Code => [247, 118, 52],
                Look::Vim => [86, 196, 112],
                _ => [120, 140, 255],
            };
            RgbImage::from_fn(W, H, |x, y| {
                let (u, v) = (x as f32 / w, y as f32 / h);
                let bg = mix([18, 20, 30], mix([18, 20, 30], accent, 0.35), u * v);
                let window = (0.06..0.62).contains(&u) && (0.12..0.88).contains(&v);
                if !window {
                    let glow = (-((u - 0.82).hypot(v - 0.5)) * 5.0).exp();
                    return Rgb(mix(bg, accent, 0.5 * glow));
                }
                if v < 0.19 {
                    let dot = [0.1, 0.13, 0.16]
                        .iter()
                        .any(|c| (u - c).hypot((v - 0.155) * h / w) < 0.008);
                    return Rgb(if dot { [255, 95, 87] } else { [44, 46, 60] });
                }
                let line = ((v - 0.22) / 0.055) as u32;
                let start = 0.09 + 0.03 * (noise(line, seed) * 3.0).floor();
                let end = start + 0.08 + 0.4 * noise(line + 7, seed);
                let in_line =
                    ((v - 0.22) / 0.055).fract() < 0.45 && (start..end.min(0.6)).contains(&u);
                let color =
                    [[198, 120, 221], [97, 175, 239], [229, 192, 123], accent][(line % 4) as usize];
                Rgb(if in_line { color } else { [30, 32, 44] })
            })
        }
        Look::Bread | Look::Coffee => {
            let (bg, outer, inner) = if matches!(look, Look::Bread) {
                ([246, 214, 160], [70, 70, 76], [222, 160, 82])
            } else {
                ([214, 196, 176], [240, 240, 236], [150, 92, 54])
            };
            RgbImage::from_fn(W, H, |x, y| {
                let (u, v) = (x as f32 / w, y as f32 / h);
                let d = ((u - 0.62) * w / h).hypot(v - 0.5);
                if d < 0.36 {
                    let speck = noise(x / 3, y / 3 + seed) > 0.93;
                    let shade = mix(inner, [255, 236, 190], 0.4 * (1.0 - d / 0.36));
                    return Rgb(if speck {
                        mix(shade, [96, 60, 30], 0.6)
                    } else {
                        shade
                    });
                }
                if d < 0.42 {
                    return Rgb(outer);
                }
                Rgb(mix(bg, mix(bg, [0, 0, 0], 0.25), d))
            })
        }
        Look::Chip => RgbImage::from_fn(W, H, |x, y| {
            let (u, v) = (x as f32 / w, y as f32 / h);
            let (cu, cv) = ((u - 0.5) * w / h, v - 0.5);
            let bg = mix([8, 20, 48], [20, 80, 140], 1.0 - cu.hypot(cv));
            let chip = cu.abs() < 0.22 && cv.abs() < 0.22;
            let pin = (cu.abs() < 0.3
                && cv.abs() < 0.2
                && (cv * 30.0).fract().abs() < 0.4
                && cu.abs() > 0.22)
                || (cv.abs() < 0.3
                    && cu.abs() < 0.2
                    && (cu * 30.0).fract().abs() < 0.4
                    && cv.abs() > 0.22);
            let trace = ((u * 24.0).fract() < 0.06 || (v * 14.0).fract() < 0.06)
                && noise((u * 24.0) as u32, (v * 14.0) as u32) > 0.6;
            Rgb(if chip {
                mix([40, 44, 52], [90, 96, 110], cv + 0.5)
            } else if pin {
                [214, 178, 96]
            } else if trace {
                mix(bg, [80, 200, 255], 0.6)
            } else {
                bg
            })
        }),
        Look::Keys => RgbImage::from_fn(W, H, |x, y| {
            let (u, v) = (x as f32 / w, y as f32 / h);
            let bg = mix([238, 222, 246], [186, 214, 250], u);
            let half = |u: f32| (0.06..0.46).contains(&u) || (0.54..0.94).contains(&u);
            let (ku, kv) = ((u * 16.0).fract(), (v * 9.0).fract());
            let key = half(u) && (0.2..0.8).contains(&v) && ku > 0.12 && kv > 0.14;
            let accent = noise((u * 16.0) as u32, (v * 9.0) as u32 + seed) > 0.8;
            Rgb(if key {
                if accent {
                    [255, 128, 112]
                } else {
                    mix([250, 250, 252], [210, 212, 224], kv)
                }
            } else if half(u) && (0.17..0.83).contains(&v) {
                [60, 60, 72]
            } else {
                bg
            })
        }),
        Look::Rocket => RgbImage::from_fn(W, H, |x, y| {
            let (u, v) = (x as f32 / w, y as f32 / h);
            let sky = mix([12, 24, 64], [255, 150, 90], v);
            let body = (u - 0.62).abs() < 0.035 && (0.18..0.66).contains(&v);
            let nose = v < 0.18 && v > 0.08 && (u - 0.62).abs() < 0.035 * (v - 0.08) / 0.1;
            let flame = v > 0.66 && (u - 0.62).abs() < 0.03 * (1.0 - (v - 0.66) / 0.3) + 0.01;
            Rgb(if body || nose {
                mix([236, 238, 242], [170, 176, 190], (u - 0.585) / 0.07)
            } else if flame {
                mix([255, 250, 200], [255, 120, 40], (v - 0.66) / 0.3)
            } else {
                sky
            })
        }),
        Look::Plane => RgbImage::from_fn(W, H, |x, y| {
            let (u, v) = (x as f32 / w, y as f32 / h);
            let sky = mix([120, 196, 255], [222, 242, 255], v);
            // A paper plane: a triangle pointing up and right.
            let (pu, pv) = (u - 0.45, v - 0.55);
            let inside = pu > 0.0 && pv < 0.0 && pu < 0.32 && -pv < pu * 0.9 && -pv > pu * 0.35;
            let fold = pu > 0.0 && pu < 0.32 && (-pv - pu * 0.6).abs() < 0.006;
            let trail = (v - 0.55 - (0.45 - u) * 0.25).abs() < 0.004
                && u < 0.43
                && ((u * 40.0) as u32).is_multiple_of(2);
            Rgb(if fold {
                [190, 204, 220]
            } else if inside {
                mix([255, 255, 255], [222, 230, 240], pu / 0.32)
            } else if trail {
                [255, 255, 255]
            } else {
                sky
            })
        }),
        Look::Wood => RgbImage::from_fn(W, H, |x, y| {
            let (u, v) = (x as f32 / w, y as f32 / h);
            let grain = ((v * 40.0 + 3.0 * (u * 6.0 + seed as f32).sin()).sin() + 1.0) / 2.0;
            let wood = mix([150, 92, 52], [196, 136, 82], grain);
            let tool = (0.3..0.75).contains(&u) && (0.38..0.6).contains(&v);
            let handle = ((u - 0.68).hypot((v - 0.32) * 1.6)) < 0.08;
            Rgb(if tool {
                mix([70, 76, 86], [140, 148, 160], (v - 0.38) / 0.22)
            } else if handle {
                [110, 40, 32]
            } else {
                wood
            })
        }),
        Look::Orbit => RgbImage::from_fn(W, H, |x, y| {
            let (u, v) = (x as f32 / w, y as f32 / h);
            let (cu, cv) = ((u - 0.6) * w / h, v - 0.55);
            let d = cu.hypot(cv);
            let ring = ((cu / 0.42).powi(2) + (cv / 0.1).powi(2) - 1.0).abs() < 0.08
                && !(d < 0.25 && cv < 0.0);
            Rgb(if d < 0.25 {
                mix([255, 190, 120], [180, 70, 60], (cu + cv + 0.35) / 0.7)
            } else if ring {
                [230, 210, 170]
            } else if noise(x, y + seed) > 0.996 {
                [255, 255, 255]
            } else {
                mix([6, 8, 24], [30, 20, 60], v)
            })
        }),
    }
}

/// Mountains under a sky that fades to the horizon, the sun behind them.
fn landscape(sky: [Color; 2], ridges: [Color; 3], sun: Color) -> RgbImage {
    let (w, h) = (W as f32, H as f32);
    let (sun_x, sun_y, radius) = (0.68 * w, 0.42 * h, 0.08 * h);
    let ridge = |i: usize, x: f32| {
        let n = i as f32;
        let u = x / w;
        let shape = 0.6 * (u * (5.0 + 2.0 * n) + 1.3 * n).sin()
            + 0.3 * (u * (13.0 + 3.0 * n) + 0.7).sin()
            + 0.1 * (u * 31.0 + n).sin();
        h * (0.56 + 0.13 * n) - h * (0.12 - 0.025 * n) * shape
    };
    RgbImage::from_fn(W, H, |x, y| {
        let (x, y) = (x as f32, y as f32);
        for i in (0..3).rev() {
            let top = ridge(i, x);
            if y >= top {
                return Rgb(mix(ridges[i], [0, 0, 0], (y - top) / h * 0.6));
            }
        }
        let distance = (x - sun_x).hypot(y - sun_y);
        if distance <= radius {
            return Rgb(sun);
        }
        let glow = 0.6 * (-(distance - radius) / (1.6 * radius)).exp();
        Rgb(mix(mix(sky[0], sky[1], y / (0.75 * h)), sun, glow))
    })
}

/// A channel photo: a two-color gradient with a soft light in it.
fn avatar(seed: usize) -> DynamicImage {
    const PAIRS: [(Color, Color); 8] = [
        ([243, 139, 168], [250, 179, 135]),
        ([137, 180, 250], [180, 190, 254]),
        ([166, 227, 161], [148, 226, 213]),
        ([203, 166, 247], [245, 194, 231]),
        ([249, 226, 175], [250, 179, 135]),
        ([116, 199, 236], [137, 220, 235]),
        ([235, 160, 172], [203, 166, 247]),
        ([148, 226, 213], [166, 227, 161]),
    ];
    let (a, b) = PAIRS[seed % PAIRS.len()];
    RgbImage::from_fn(176, 176, |x, y| {
        let (u, v) = (x as f32 / 176.0, y as f32 / 176.0);
        let light = (-((u - 0.35).hypot(v - 0.3)) * 4.0).exp();
        Rgb(mix(mix(a, b, (u + v) / 2.0), [255, 255, 255], 0.35 * light))
    })
    .into()
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color as Paint, Modifier};
    use ratatui_image::FontSize;
    use ratatui_image::picker::ProtocolType;

    use super::*;

    fn picker(protocol: ProtocolType, font: FontSize) -> Picker {
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(font);
        picker.set_protocol_type(protocol);
        picker
    }

    #[tokio::test]
    async fn the_demo_fills_the_real_ui_and_fetches_nothing() {
        let (tx, _rx) = unbounded_channel();
        let mut app = demo_app(picker(ProtocolType::Halfblocks, FontSize::new(8, 16)), tx);
        let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains("The 10-minute focaccia"),
            "Home lists the videos"
        );
        assert!(text.contains("Coastline Kitchen"));
        assert!(text.contains("Rainy night jazz"), "the player bar");
        assert!(text.contains("Subscriptions (22)"));
        assert!(app.status.is_none());
        assert!(app.refreshing.is_none(), "no feeds fetched");
    }

    fn hex(color: Paint) -> serde_json::Value {
        match color {
            Paint::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}").into(),
            Paint::White => "#ffffff".into(),
            Paint::Black => "#000000".into(),
            _ => serde_json::Value::Null,
        }
    }

    /// One frame of the demo for `tools/hero.py`: every cell's character,
    /// colors and weight, and where the images go. The cell size must be
    /// the one hero.py draws with, so pictures keep their shape.
    #[tokio::test]
    #[ignore = "writes target/hero/screen.json for tools/hero.py"]
    async fn export_hero_screen() {
        let (cols, rows) = (140u16, 42u16);
        let font = FontSize::new(16, 34);
        let (tx, _rx) = unbounded_channel();
        let mut app = demo_app(picker(ProtocolType::Kitty, font), tx);
        // As Ghostty shows it, whatever terminal runs the test.
        app.icons = crate::icons::NERD;
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        // The first frames ask for the pictures; then they're ready.
        for _ in 0..3 {
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            app.images.build_wanted_now();
        }
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let mut cells = Vec::new();
        for y in 0..rows {
            for x in 0..cols {
                let cell = &buffer[(x, y)];
                // Kitty's image cells hold its escape codes and placeholder
                // characters: hero.py draws the picture there. What the UI
                // drew over a picture (the length badge) stays.
                let image = cell.symbol().contains(['\u{1b}', '\u{10EEEE}']);
                let symbol = if image { " " } else { cell.symbol() };
                cells.push(serde_json::json!([
                    symbol,
                    hex(cell.fg),
                    hex(cell.bg),
                    cell.modifier.contains(Modifier::BOLD),
                ]));
            }
        }
        let rect = |r: ratatui::layout::Rect| serde_json::json!([r.x, r.y, r.width, r.height]);
        let images: Vec<_> = app
            .images
            .placed
            .iter()
            .map(|p| match &p.subject {
                Subject::Thumbnail(id) => {
                    let video = app.videos.iter().find(|v| v.id == *id);
                    serde_json::json!({
                        "kind": "thumbnail",
                        "id": id.as_str(),
                        "title": video.map(|v| v.title.clone()),
                        "channel": video.map(|v| v.channel.clone()),
                        "area": rect(p.area),
                        "shown": rect(p.shown),
                    })
                }
                Subject::Avatar(id) => {
                    let name = CHANNELS.iter().find(|n| channel_id(n) == *id);
                    serde_json::json!({
                        "kind": "avatar",
                        "id": id.as_str(),
                        "name": name,
                        "area": rect(p.area),
                        "shown": rect(p.shown),
                    })
                }
            })
            .collect();
        let screen = serde_json::json!({
            "cols": cols,
            "rows": rows,
            "cell": [font.width, font.height],
            "bg": hex(app.colors.bg),
            "fg": hex(app.colors.text),
            "cells": cells,
            "images": images,
        });
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/hero");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("screen.json"), screen.to_string()).unwrap();
        for (i, fake) in VIDEOS.iter().enumerate() {
            thumbnail(fake.look, i)
                .save(dir.join(format!("{}.png", video_id(i))))
                .unwrap();
        }
        eprintln!("wrote {}", dir.join("screen.json").display());
    }
}
