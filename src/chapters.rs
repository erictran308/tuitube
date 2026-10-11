//! A video's chapters, as yt-dlp reads them from YouTube (the chapters a
//! channel set, or the timestamps in the description), and going to a time
//! typed in the chapters popup (`t`).

/// Chapters kept per video, at most: a long mix lists a few hundred songs.
pub const MAX_CHAPTERS: usize = 500;
/// `[` within this many seconds of a chapter's start goes to the one
/// before; later, to the start of this one. mpv's own chapter keys do the
/// same (`--chapter-seek-threshold`).
pub const BACK_WITHIN: f64 = 5.0;

/// Where a chapter starts, and its title, cleaned and on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
}

/// The chapters as they're kept: started at a time that makes sense
/// (before `length`, when it's known), in order, one per start, at most
/// `MAX_CHAPTERS`.
pub fn tidy(mut chapters: Vec<Chapter>, length: Option<f64>) -> Vec<Chapter> {
    chapters
        .retain(|c| c.start.is_finite() && c.start >= 0.0 && length.is_none_or(|l| c.start < l));
    chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
    chapters.dedup_by(|a, b| a.start == b.start);
    chapters.truncate(MAX_CHAPTERS);
    chapters
}

/// The chapter `at` seconds is in: the last to start at or before it.
pub fn current(chapters: &[Chapter], at: f64) -> Option<usize> {
    chapters.iter().rposition(|c| c.start <= at)
}

/// Where `[` goes from `at`: the start of this chapter, or of the one
/// before if this one just started.
pub fn previous(chapters: &[Chapter], at: f64) -> Option<f64> {
    let i = current(chapters, at)?;
    if at - chapters[i].start > BACK_WITHIN || i == 0 {
        return Some(chapters[i].start);
    }
    Some(chapters[i - 1].start)
}

/// Where `]` goes from `at`: the next chapter's start.
pub fn next(chapters: &[Chapter], at: f64) -> Option<f64> {
    chapters.iter().map(|c| c.start).find(|&start| start > at)
}

/// The time `text` names, in seconds from the start: `1:23`, `1:02:03`,
/// `90`, a step from `at` (`+30`, `-1:00`) or a share of `length` (`50%`).
/// `None` if it names none.
pub fn parse_time(text: &str, at: f64, length: Option<f64>) -> Option<f64> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        let percent: f64 = percent.trim().parse().ok()?;
        return (0.0..=100.0)
            .contains(&percent)
            .then_some(length? * percent / 100.0);
    }
    let (sign, clock) = match text.as_bytes().first()? {
        b'+' => (1.0, &text[1..]),
        b'-' => (-1.0, &text[1..]),
        _ => (0.0, text),
    };
    let parts: Vec<&str> = clock.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0;
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        // Only the seconds may have a fraction; only the first part may be
        // 60 or more (`90:00` is an hour and a half).
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let ok = match part.split_once('.') {
            Some((whole, fraction)) => last && digits(whole) && digits(fraction),
            None => digits(part),
        };
        if !ok {
            return None;
        }
        let value: f64 = part.parse().ok()?;
        if i > 0 && value >= 60.0 {
            return None;
        }
        seconds = seconds * 60.0 + value;
    }
    let time = if sign == 0.0 {
        seconds
    } else {
        at + sign * seconds
    };
    time.is_finite().then_some(time.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapters(starts: &[f64]) -> Vec<Chapter> {
        starts
            .iter()
            .map(|&start| Chapter {
                start,
                title: format!("at {start}"),
            })
            .collect()
    }

    #[test]
    fn chapters_are_kept_in_order_once_each_and_within_the_video() {
        let kept = tidy(
            chapters(&[
                300.0,
                0.0,
                f64::NAN,
                -5.0,
                120.0,
                120.0,
                900.0,
                f64::INFINITY,
            ]),
            Some(600.0),
        );
        let starts: Vec<f64> = kept.iter().map(|c| c.start).collect();
        assert_eq!(starts, [0.0, 120.0, 300.0]);
        let many = chapters(&(0..2000).map(f64::from).collect::<Vec<_>>());
        assert_eq!(tidy(many, None).len(), MAX_CHAPTERS);
    }

    #[test]
    fn the_chapter_keys_go_where_youtube_and_mpv_go() {
        let list = chapters(&[0.0, 60.0, 180.0]);
        assert_eq!(current(&list, 0.0), Some(0));
        assert_eq!(current(&list, 59.9), Some(0));
        assert_eq!(current(&list, 60.0), Some(1));
        assert_eq!(current(&list, 999.0), Some(2));
        assert_eq!(current(&chapters(&[10.0]), 5.0), None, "before the first");

        assert_eq!(next(&list, 30.0), Some(60.0));
        assert_eq!(next(&list, 60.0), Some(180.0));
        assert_eq!(next(&list, 200.0), None, "the last one");
        // Well into a chapter: its start. Just after its start: the one
        // before.
        assert_eq!(previous(&list, 100.0), Some(60.0));
        assert_eq!(previous(&list, 62.0), Some(0.0));
        assert_eq!(previous(&list, 2.0), Some(0.0), "the first stays");
        assert_eq!(previous(&[], 50.0), None);
    }

    #[test]
    fn a_typed_time_can_be_a_clock_a_step_or_a_share() {
        let t = |text: &str| parse_time(text, 100.0, Some(600.0));
        assert_eq!(t("90"), Some(90.0));
        assert_eq!(t("1:23"), Some(83.0));
        assert_eq!(t(" 1:02:03 "), Some(3723.0));
        assert_eq!(t("90:00"), Some(5400.0), "the first part may be big");
        assert_eq!(t("0:05.5"), Some(5.5));
        assert_eq!(t("+30"), Some(130.0));
        assert_eq!(t("-1:00"), Some(40.0));
        assert_eq!(t("-5:00"), Some(0.0), "not before the start");
        assert_eq!(t("50%"), Some(300.0));
        assert_eq!(t("100%"), Some(600.0));
        assert_eq!(parse_time("50%", 0.0, None), None, "no length to share");
        for bad in [
            "", "+", "1:60", "1::2", "1:2:3:4", "1.5:00", "abc", "1e3", "150%", "-5%", ".5", "1:",
            "NaN", "inf", "+-3",
        ] {
            assert_eq!(t(bad), None, "{bad:?}");
        }
    }
}
