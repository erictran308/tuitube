//! The visualizer: colored bars in the player bar that move with the sound,
//! in place of the picture while only the sound plays.
//!
//! tuitube never has the sound: mpv plays it. So mpv's own FFmpeg measures
//! it, through an audio filter tuitube adds over mpv's socket ([`filter`]):
//! the sound goes on through it unchanged, while a copy of it, made mono,
//! goes through one band-pass filter per band, and `astats` reports each
//! band's loudness as the filter's metadata. While the bars show, tuitube
//! asks mpv for those numbers every [`EVERY`] (about a kilobyte), and
//! [`Spectrum`] turns them into heights. No FFT, no capture of the
//! computer's sound, no library: nothing new is downloaded or kept, and the
//! numbers are only drawn.
//!
//! mpv measures the sound as it's decoded, [`LEAD`] before it's heard, so
//! each answer is held that long before it's drawn.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The bands mpv measures; the player bar spreads its bars over them. Each
/// costs mpv about a sixth of a percent of a core (`astats` looks at every
/// sample of each), on top of two and a half for the filter itself:
/// measured on an M-series Mac, 24 bands took mpv from 2.8% to 9.0%, 12 to
/// 6.8%, 32 to 10.8%.
pub const BANDS: usize = 24;

/// Each band's middle, in Hz: bass on the left, treble on the right, about
/// three eighths of an octave apart.
const MIDDLES: [u32; BANDS] = [
    40, 52, 65, 85, 115, 145, 190, 250, 320, 420, 540, 700, 900, 1200, 1500, 2000, 2600, 3400,
    4400, 5600, 7500, 9500, 12500, 16000,
];

/// How wide each band-pass is, in octaves: a little wider than the bands
/// are apart, so no sound falls between two.
const WIDTH: &str = "0.47";

/// The filter's label in mpv: `af-metadata/<label>` holds its numbers, and
/// `af remove @<label>` takes it out.
pub const LABEL: &str = "tuitube";

/// How often mpv is asked for the numbers while the bars show: 25 frames a
/// second.
pub const EVERY: Duration = Duration::from_millis(40);

/// How far ahead of what's heard mpv measures: its `audio-buffer`, 0.2 s
/// unless set otherwise (measured: 0.20 s with CoreAudio).
const LEAD: Duration = Duration::from_millis(200);

/// Answers kept waiting to be drawn, at most: a little over `LEAD`'s worth.
const MAX_WAITING: usize = 16;

/// The quietest level counted, in dB: `astats` says `-inf` for silence.
const FLOOR: f32 = -120.0;

/// The bars span this many dB below the loudest band lately: measured on a
/// song, most bars then move between a fifth and three quarters high.
const RANGE: f32 = 28.0;

/// The loudest level lately never counts as quieter than this, in dB, so a
/// quiet passage, or silence, isn't stretched into full bars.
const QUIETEST_TOP: f32 = -40.0;

/// How fast the loudest level lately falls back, in dB a second: the bars
/// grow again after a loud part, over a few seconds.
const TOP_FALLS: f32 = 4.0;

/// dB added to each band, so music's usual balance draws level bars: the
/// deepest bass and the treble are quieter than the rest in nearly all of
/// it (measured on a song: 190 Hz to 3.4 kHz within 4 dB, 40 Hz 17 dB
/// under them and 16 kHz 20 dB; the boost stops at 14).
const BOOST: [f32; BANDS] = [
    14.0, 9.0, 6.0, 6.0, 4.0, 4.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 2.0, 1.0, 1.0, 4.0,
    4.0, 6.0, 9.0, 14.0, 14.0,
];

/// How much of the way to a taller height a bar goes each frame: a beat
/// shows at once.
const RISE: f32 = 0.75;

/// How fast a bar falls, in heights a second: from full to nothing in
/// about half a second, as a meter's needle.
const FALL: f32 = 2.2;

/// How fast the colors go round, in degrees of hue a second...
const HUE_SPEED: f32 = 24.0;
/// ...in steps of this many degrees: between steps, only the cells whose
/// height changed are sent to the terminal again, not every one.
const HUE_STEP: f32 = 12.0;

/// Loudness in dB of each band, as `astats` measured it.
pub type Levels = [f32; BANDS];

/// The audio filter, as mpv's `af add` takes it: `aformat` makes the sound
/// stereo (as it nearly always is already) so the end knows its channels,
/// `asplit` copies it, `pan` makes the copy mono, `asplit` makes a copy per
/// band, each through the same band-pass twice (for steeper sides), then
/// `amerge` puts the bands after the sound as extra channels, `astats`
/// measures each channel's loudness over two blocks of sound (40 ms; its
/// other measures over a 10 ms window, the shortest every FFmpeg takes, as
/// they're not read), and `pan` keeps only the sound's two channels. The
/// bands can't be measured at a lower rate, which would cost less: no
/// FFmpeg filter that joins two streams keeps the numbers. Only tuitube's
/// own numbers go in: nothing from YouTube.
pub fn filter() -> String {
    let copies: String = (0..BANDS).map(|i| format!("[b{i}]")).collect();
    let bands: Vec<String> = MIDDLES
        .iter()
        .enumerate()
        .map(|(i, hz)| {
            let pass = format!("bandpass=f={hz}:width_type=o:width={WIDTH}");
            format!("[b{i}]{pass},{pass}[c{i}]")
        })
        .collect();
    let merged: String = (0..BANDS).map(|i| format!("[c{i}]")).collect();
    format!(
        "@{LABEL}:lavfi=[aformat=channel_layouts=stereo,asplit[sound][copy];\
         [copy]pan=mono|c0=0.5*c0+0.5*c1,asplit={BANDS}{copies};{};\
         [sound]{merged}amerge=inputs={},\
         astats=metadata=1:reset=2:length=0.01:measure_perchannel=RMS_level:measure_overall=none,\
         pan=stereo|c0=c0|c1=c1]",
        bands.join(";"),
        BANDS + 1,
    )
}

/// The property holding the filter's numbers.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn property() -> String {
    format!("af-metadata/{LABEL}")
}

/// The bands' levels in mpv's answer (`af-metadata`): `astats` numbers its
/// channels from 1, the sound's two first, so band `i` is channel `i + 3`.
/// Read leniently: a level that's missing, or isn't a finite number, is
/// silence; an answer with none at all isn't one.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn levels(data: &serde_json::Value) -> Option<Levels> {
    let mut levels = [FLOOR; BANDS];
    let mut found = false;
    for (key, value) in data.as_object()? {
        let Some(channel) = key
            .strip_prefix("lavfi.astats.")
            .and_then(|k| k.strip_suffix(".RMS_level"))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        let Some(band) = channel.checked_sub(3).filter(|&b| b < BANDS) else {
            continue;
        };
        let db = value.as_str().and_then(|s| s.trim().parse::<f32>().ok());
        levels[band] = db.filter(|db| db.is_finite()).map_or(FLOOR, |db| {
            // A level above full scale is clipping; below the floor, nothing.
            db.clamp(FLOOR, 0.0)
        });
        found = true;
    }
    found.then_some(levels)
}

/// The bars' heights, from mpv's levels as they come.
#[derive(Clone, Debug)]
pub struct Spectrum {
    /// Levels from mpv, oldest first, with when each came.
    waiting: VecDeque<(Instant, Levels)>,
    /// The levels being heard: the last that waited `LEAD`.
    heard: Option<Levels>,
    /// Each bar's height: 0 to 1.
    heights: [f32; BANDS],
    /// The loudest band lately, in dB with its boost: full height.
    top: f32,
    /// When the bars last moved.
    moved: Option<Instant>,
    /// Where the colors are, in degrees of hue.
    hue: f32,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self {
            waiting: VecDeque::with_capacity(MAX_WAITING),
            heard: None,
            heights: [0.0; BANDS],
            top: QUIETEST_TOP,
            moved: None,
            hue: 0.0,
        }
    }
}

impl Spectrum {
    /// mpv's levels, which came `at`.
    pub fn push(&mut self, levels: Levels, at: Instant) {
        if self.waiting.len() == MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back((at, levels));
    }

    /// Moves the bars to `now`: toward the levels heard now (the newest that
    /// came `LEAD` ago or more), rising fast and falling slower. Between
    /// answers they stay toward the last one heard.
    pub fn step(&mut self, now: Instant) {
        while let Some(&(at, levels)) = self.waiting.front() {
            if now.saturating_duration_since(at) < LEAD {
                break;
            }
            self.heard = Some(levels);
            self.waiting.pop_front();
        }
        let seconds = self
            .moved
            .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32())
            .min(0.25);
        self.moved = Some(now);
        self.hue = (self.hue + HUE_SPEED * seconds) % 360.0;
        let boosted = self.heard.map(|mut levels| {
            for (level, boost) in levels.iter_mut().zip(BOOST) {
                *level += boost;
            }
            levels
        });
        let loudest = boosted.map_or(FLOOR, |t| t.iter().copied().fold(FLOOR, f32::max));
        self.top = (self.top - TOP_FALLS * seconds)
            .max(loudest)
            .max(QUIETEST_TOP);
        let bottom = self.top - RANGE;
        for (i, height) in self.heights.iter_mut().enumerate() {
            let target = boosted.map_or(0.0, |t| ((t[i] - bottom) / RANGE).clamp(0.0, 1.0));
            if target > *height {
                *height += (target - *height) * RISE;
            } else {
                *height = (*height - FALL * seconds).max(target);
            }
        }
    }

    /// Levels heard now, not measured ahead: the demo's.
    pub fn hear(&mut self, levels: Levels, now: Instant) {
        self.waiting.clear();
        self.heard = Some(levels);
        self.step(now);
    }

    /// Each bar's height, 0 to 1, bass first.
    pub fn heights(&self) -> &[f32; BANDS] {
        &self.heights
    }

    /// Where the colors are, in degrees of hue: they go slowly round, a
    /// step at a time.
    pub fn hue(&self) -> f32 {
        (self.hue / HUE_STEP).floor() * HUE_STEP
    }
}

/// Made-up levels for the demo, `seconds` in: a kick drum twice a second
/// and a melody wandering over the rest.
pub fn demo_levels(seconds: f64) -> Levels {
    let beat = (seconds * 2.0).fract();
    let kick = (1.0 - beat * 4.0).max(0.0) as f32;
    let mut levels = [FLOOR; BANDS];
    for (i, level) in levels.iter_mut().enumerate() {
        let x = i as f64;
        let melody = (seconds * 1.3 + x * 0.35).sin() * 0.4
            + (seconds * 0.45 - x * 0.2).sin() * 0.4
            + (seconds * 3.1 + x * 2.3).sin() * 0.3;
        let bass = if i < 6 {
            kick * (6 - i) as f32 * 2.5
        } else {
            0.0
        };
        *level = -30.0 + melody as f32 * 7.0 + bass - BOOST[i];
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_is_one_graph_of_tuitubes_own_numbers() {
        let filter = filter();
        assert!(filter.starts_with("@tuitube:lavfi=["), "{filter}");
        assert!(filter.ends_with(']'));
        assert_eq!(filter.matches("bandpass=").count(), 2 * BANDS);
        assert!(filter.contains(&format!("amerge=inputs={}", BANDS + 1)));
        assert!(filter.contains("measure_perchannel=RMS_level"));
        // The sound comes out as it went in: its two channels, nothing else.
        assert!(filter.ends_with("pan=stereo|c0=c0|c1=c1]"));
        // One graph: mpv's option parser splits at a bracket left open.
        assert_eq!(filter.matches('[').count(), filter.matches(']').count());
        assert!(!filter.contains(' ') && !filter.contains('\n'));
    }

    #[test]
    fn levels_are_read_leniently_from_mpvs_answer() {
        let answer = serde_json::json!({
            "lavfi.astats.1.RMS_level": "-20.0",
            "lavfi.astats.2.RMS_level": "-20.0",
            "lavfi.astats.3.RMS_level": "-37.6",
            "lavfi.astats.4.RMS_level": "-inf",
            "lavfi.astats.5.RMS_level": "nan",
            "lavfi.astats.6.RMS_level": "3.5",
            "lavfi.astats.7.RMS_level": "loud",
            "lavfi.astats.14.RMS_level": "-30.0",
            "lavfi.astats.27.RMS_level": "-1.0",
            "lavfi.astats.99999999999999999999.RMS_level": "-1.0",
            "lavfi.astats.8.Peak_level": "-1.0",
            "something.else": 4,
        });
        let read = levels(&answer).unwrap();
        assert_eq!(read[0], -37.6);
        assert_eq!(read[1], FLOOR, "silence");
        assert_eq!(read[2], FLOOR, "not a number");
        assert_eq!(read[3], 0.0, "clipping is full scale");
        assert_eq!(read[4], FLOOR, "not a number");
        assert_eq!(read[5], FLOOR, "only RMS levels");
        assert_eq!(read[11], -30.0);
        assert_eq!(levels(&serde_json::json!({})), None, "no numbers");
        assert_eq!(levels(&serde_json::json!("odd")), None);
        assert_eq!(levels(&serde_json::Value::Null), None);
    }

    #[test]
    fn bars_wait_until_the_sound_is_heard_then_rise_and_fall() {
        let start = Instant::now();
        let mut spectrum = Spectrum::default();
        let mut loud = [FLOOR; BANDS];
        loud[0] = -10.0;
        spectrum.push(loud, start);
        spectrum.step(start + Duration::from_millis(100));
        assert_eq!(spectrum.heights()[0], 0.0, "not heard yet");
        spectrum.step(start + LEAD);
        let risen = spectrum.heights()[0];
        assert!(risen > 0.7, "a beat shows at once: {risen}");
        assert_eq!(spectrum.heights()[5], 0.0, "only the bass");
        // Silence after it: the bar falls, not at once.
        spectrum.push([FLOOR; BANDS], start + LEAD);
        spectrum.step(start + LEAD * 2);
        let falling = spectrum.heights()[0];
        assert!(falling < risen && falling > 0.0, "{falling}");
        spectrum.step(start + LEAD * 2 + Duration::from_secs(2));
        assert_eq!(spectrum.heights()[0], 0.0);
        assert!(spectrum.heights().iter().all(|h| (0.0..=1.0).contains(h)));
    }

    #[test]
    fn quiet_music_fills_the_bars_but_silence_doesnt() {
        let start = Instant::now();
        let mut quiet = Spectrum::default();
        // Lasts long enough for the loudest level lately to come down.
        for frame in 0..400 {
            let at = start + EVERY * frame;
            quiet.push([-40.0; BANDS], at);
            quiet.step(at);
        }
        let max = quiet.heights().iter().copied().fold(0.0, f32::max);
        assert!(max > 0.9, "quiet music still fills them: {max}");
        let mut silent = Spectrum::default();
        for frame in 0..400 {
            let at = start + EVERY * frame;
            // A lossy codec's noise, under music that stopped.
            silent.push([-100.0; BANDS], at);
            silent.step(at);
        }
        assert!(
            silent.heights().iter().all(|h| *h == 0.0),
            "{:?}",
            silent.heights()
        );
    }

    #[test]
    fn answers_that_pile_up_are_dropped_oldest_first() {
        let start = Instant::now();
        let mut spectrum = Spectrum::default();
        for i in 0..100 {
            spectrum.push([FLOOR; BANDS], start + Duration::from_millis(i));
        }
        assert_eq!(spectrum.waiting.len(), MAX_WAITING);
    }

    #[test]
    fn the_demo_moves() {
        let a = demo_levels(1.0);
        let b = demo_levels(1.3);
        assert_ne!(a, b);
        assert!(a.iter().all(|l| l.is_finite() && *l <= 0.0 && *l >= FLOOR));
    }
}
