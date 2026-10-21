//! `--when`: date phrases by rules and event-relative anchors
//! (`docs/ACTIVITY_SEARCH.md` §5.4-5.5; a port of the prototype
//! `scripts/activity/dates.py` and its tests). No model: anything that does
//! not parse is an error listing the forms, unless a relation word
//! (`before`, `after`, `around`, `during`, `the day`, ...) introduces it,
//! in which case it is an anchor found by searching the journal.
//!
//! Times are in the person's time zone (the `timezone` setting, else the
//! machine's), periods are half-open `[from, to)` in µs since the epoch,
//! and a period that would end in the future ends now.
use crate::Result;
use jiff::{
    Timestamp, ToSpan, Zoned,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const MIN: i64 = 60_000_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

const WEEKDAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// The forms, for error messages.
pub const FORMS: &str = "today, yesterday, tuesday, last tuesday, this|last week|month|year, past 3 days, \
    3 days ago, oct 14, 2026-10-14, 10/14, september, yesterday afternoon, last night, yesterday at 3pm, \
    since X, after X, before X, between X and Y, 7d, @unix, RFC 3339; anchors: before \"deploy\", \
    after the first \"failed test\", the day \"build broke\", around @4211, during the last \"release\"";

/// A half-open period `[from, to)`, µs since the epoch (`from` None: unbounded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    pub from: Option<i64>,
    pub to: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    First,
    Last,
    Top,
}

/// What an anchor names: a search phrase, or a record id (`@4211`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub query: String,
    pub id: Option<u64>,
    pub pick: Pick,
    /// The phrase gave an ordinal (`first`, `last`, `top`).
    pub pick_given: bool,
}

/// How a period hangs off an anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rel {
    /// [t - span, t)
    Before,
    /// [t, t + span)
    After,
    /// t ± 1 h
    Around,
    /// the calendar day of t
    Day,
    /// t ± 30 min
    Hour,
    /// the anchor's session, first to last record
    Session,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phrase {
    Period(Period),
    Anchored {
        rel: Rel,
        anchor: Anchor,
    },
    /// `between X and Y` where X or Y is anchored: from X's start to Y's end.
    Between(Box<Phrase>, Box<Phrase>),
}

/// "Now" in the person's time zone.
#[derive(Clone, Debug)]
pub struct Clock {
    pub now: Zoned,
}

impl Clock {
    pub fn new(now_us: i64, tz: TimeZone) -> Result<Self> {
        Ok(Self { now: Timestamp::from_microsecond(now_us)?.to_zoned(tz) })
    }
    fn tz(&self) -> TimeZone {
        self.now.time_zone().clone()
    }
    fn now_us(&self) -> i64 {
        self.now.timestamp().as_microsecond()
    }
    fn today(&self) -> Date {
        self.now.date()
    }
    /// Local midnight of a date (DST-correct), µs.
    fn day0(&self, d: Date) -> i64 {
        self.at(d, 0, 0)
    }
    /// Local `h:m` of a date (`h` may pass 24: the next day), µs.
    fn at(&self, d: Date, h: i64, m: i64) -> i64 {
        let d = d.checked_add((h / 24).days()).unwrap_or(d);
        let dt: DateTime = d.at((h % 24) as i8, m as i8, 0, 0);
        dt.to_zoned(self.tz()).map(|z| z.timestamp().as_microsecond()).unwrap_or(0)
    }
    fn date_of(&self, us: i64) -> Date {
        Timestamp::from_microsecond(us).map(|t| t.to_zoned(self.tz()).date()).unwrap_or(self.today())
    }
    fn add_days(d: Date, n: i64) -> Date {
        d.checked_add(n.days()).unwrap_or(d)
    }
    fn add_months(d: Date, n: i64) -> Date {
        d.first_of_month().checked_add(n.months()).unwrap_or(d)
    }
}

fn span(from: i64, to: i64) -> Period {
    Period { from: Some(from), to }
}

static PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.*?) ?(morning|afternoon|evening|night)$").unwrap());
static AT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:(.*?) )?(at|around) (.+)$").unwrap());
static CLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,2})(?::(\d{2}))?\s*(am|pm)?$").unwrap());
static WEEKDAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^(last |this |past )?({})( before last)?$", WEEKDAYS.join("|"))).unwrap());
static UNIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(this|last|previous|past) (hour|day|week|month|year)$").unwrap());
static ROLLING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:the )?(?:last|past) (\w+) (minute|min|hour|hr|day|week)s?$").unwrap());
static AGO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\w+) (minute|min|hour|hr|day|week|month)s? ago$").unwrap());
static ISO_DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{4})-(\d{2})-(\d{2})$").unwrap());
static MON_DAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-z]{3,9})\.? (\d{1,2})(?:st|nd|rd|th)?(?:,? (\d{4}))?$").unwrap());
static DAY_MON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{1,2})(?:st|nd|rd|th)? ([a-z]{3,9})(?: (\d{4}))?$").unwrap());
static SLASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,2})/(\d{1,2})(?:/(\d{4}))?$").unwrap());
static CLI_DUR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+(?:\.\d+)?)(s|m|h|d|w)$").unwrap());
static BETWEEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:between|from) (.+?) (?:and|to|until) (.+)$").unwrap());
static LEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:on|in|at|during) ").unwrap());

fn number(w: &str) -> Option<i64> {
    if let Ok(n) = w.parse::<i64>() {
        return Some(n);
    }
    Some(match w {
        "a" | "an" | "one" => 1,
        "two" | "couple" => 2,
        "three" | "few" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => return None,
    })
}

fn month_of(name: &str) -> Option<i8> {
    (name.len() >= 3).then(|| MONTHS.iter().position(|m| m.starts_with(name)).map(|i| i as i8 + 1)).flatten()
}

fn clock(t: &str) -> Option<(i64, i64)> {
    let m = CLOCK.captures(t)?;
    if m.get(2).is_none() && m.get(3).is_none() {
        return None;
    }
    let (mut h, mi): (i64, i64) = (m[1].parse().ok()?, m.get(2).map_or(Some(0), |x| x.as_str().parse().ok())?);
    match m.get(3).map(|x| x.as_str()) {
        Some("pm") if h < 12 => h += 12,
        Some("am") if h == 12 => h = 0,
        _ => {}
    }
    (h < 24 && mi < 60).then_some((h, mi))
}

/// `oct 14`, `october 14th`, `14 oct`, `10/14`, `2026-10-14` -> that date.
fn month_day(t: &str, c: &Clock) -> Option<Date> {
    if let Some(m) = ISO_DAY.captures(t) {
        return Date::new(m[1].parse().ok()?, m[2].parse().ok()?, m[3].parse().ok()?).ok();
    }
    let (mon, day, year): (i8, i8, Option<i16>) = if let Some(m) = MON_DAY.captures(t)
        && let Some(mon) = month_of(&m[1])
    {
        (mon, m[2].parse().ok()?, m.get(3).and_then(|y| y.as_str().parse().ok()))
    } else if let Some(m) = DAY_MON.captures(t)
        && let Some(mon) = month_of(&m[2])
    {
        (mon, m[1].parse().ok()?, m.get(3).and_then(|y| y.as_str().parse().ok()))
    } else {
        let m = SLASH.captures(t)?;
        // month/day (US order), as documented
        (m[1].parse().ok()?, m[2].parse().ok()?, m.get(3).and_then(|y| y.as_str().parse().ok()))
    };
    let y = year.unwrap_or(c.today().year());
    let d = Date::new(y, mon, day).ok()?;
    if year.is_none() && c.day0(d) > c.now_us() {
        // the most recent past occurrence
        return Date::new(y - 1, mon, day).ok();
    }
    Some(d)
}

/// A plain (non-anchored) phrase as a period.
pub fn period(text: &str, c: &Clock) -> Result<Period> {
    let lowered = text.to_lowercase().replace(',', " ");
    let joined = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = LEAD.replace(&joined, "").into_owned();
    let t = t.as_str();
    let now = c.now_us();
    let today = c.today();
    let t0 = c.day0(today);
    let bad = || -> crate::Error { format!("not a date phrase: {text:?} (forms: {FORMS})").into() };
    // part of a day
    if t == "tonight" || t == "this evening" {
        return Ok(span(c.at(today, 18, 0), c.at(today, 24, 0)));
    }
    if t == "last night" {
        return Ok(span(c.at(Clock::add_days(today, -1), 18, 0), c.at(today, 6, 0)));
    }
    if let Some(m) = PART.captures(t) {
        let base =
            if matches!(&m[1], "this" | "") { today } else { c.date_of(period(&m[1], c)?.from.ok_or_else(bad)?) };
        let (a, b) = match &m[2] {
            "morning" => (6, 12),
            "afternoon" => (12, 18),
            "evening" => (18, 24),
            _ => (18, 30),
        };
        return Ok(span(c.at(base, a, 0), c.at(base, b, 0)));
    }
    // `<day> at 3pm` (that clock hour) / `around 3pm` (± 1 h); a bare clock is today
    if let Some(m) = AT.captures(t)
        && let Some((h, mi)) = clock(&m[3])
    {
        let base = match m.get(1) {
            None => today,
            Some(d) => c.date_of(period(d.as_str(), c)?.from.ok_or_else(bad)?),
        };
        let at = c.at(base, h, mi);
        return Ok(if &m[2] == "around" { span(at - HOUR, at + HOUR) } else { span(at, at + HOUR) });
    }
    if let Some((h, mi)) = clock(t) {
        let at = c.at(today, h, mi);
        return Ok(span(at, at + HOUR));
    }
    match t {
        "now" | "just now" => return Ok(span(now - 15 * MIN, now)),
        "today" => return Ok(span(t0, now)),
        "yesterday" => return Ok(span(c.day0(Clock::add_days(today, -1)), t0)),
        "day before yesterday" | "the day before yesterday" => {
            return Ok(span(c.day0(Clock::add_days(today, -2)), c.day0(Clock::add_days(today, -1))));
        }
        _ => {}
    }
    // weekdays: bare = the most recent on or before today; last = strictly before today
    if let Some(m) = WEEKDAY.captures(t) {
        let wd = WEEKDAYS.iter().position(|w| *w == &m[2]).unwrap() as i64;
        let mut back = (i64::from(today.weekday().to_monday_zero_offset()) - wd).rem_euclid(7);
        if m.get(1).map(|x| x.as_str()) == Some("last ") && back == 0 {
            back = 7;
        }
        if m.get(3).is_some() {
            back += 7;
        }
        let d = Clock::add_days(today, -back);
        return Ok(span(c.day0(d), c.day0(Clock::add_days(d, 1)).min(now)));
    }
    // this / last unit (calendar-aligned); past unit (rolling)
    if let Some(m) = UNIT.captures(t) {
        let (which, unit) = (&m[1], &m[2]);
        if which == "past" {
            let from = match unit {
                "hour" => now - HOUR,
                "day" => now - DAY,
                "week" => now - 7 * DAY,
                "month" => c.now.checked_sub(1.month()).map(|z| z.timestamp().as_microsecond()).unwrap_or(now),
                _ => c.now.checked_sub(1.year()).map(|z| z.timestamp().as_microsecond()).unwrap_or(now),
            };
            return Ok(span(from, now));
        }
        let prev = i64::from(which != "this");
        return Ok(match unit {
            "hour" => {
                let a = c.at(today, i64::from(c.now.hour()), 0) - prev * HOUR;
                span(a, (a + HOUR).min(now))
            }
            "day" => return period(if prev == 1 { "yesterday" } else { "today" }, c),
            "week" => {
                let monday = Clock::add_days(today, -i64::from(today.weekday().to_monday_zero_offset()) - 7 * prev);
                span(c.day0(monday), c.day0(Clock::add_days(monday, 7)).min(now))
            }
            "month" => {
                let a = Clock::add_months(today, -prev);
                span(c.day0(a), c.day0(Clock::add_months(a, 1)).min(now))
            }
            _ => {
                let a = Date::new(today.year() - prev as i16, 1, 1)?;
                span(c.day0(a), c.day0(Date::new(a.year() + 1, 1, 1)?).min(now))
            }
        });
    }
    // rolling windows ending now: `last 3 days`, `past 2 hours`, `the last few hours`
    if let Some(m) = ROLLING.captures(t)
        && let Some(n) = number(&m[1])
    {
        let from = match &m[2] {
            "minute" | "min" => now - n * MIN,
            "hour" | "hr" => now - n * HOUR,
            "day" => c.now.checked_sub(n.days()).map(|z| z.timestamp().as_microsecond())?,
            _ => c.now.checked_sub((7 * n).days()).map(|z| z.timestamp().as_microsecond())?,
        };
        return Ok(span(from, now));
    }
    // `N units ago`: the calendar unit containing that moment
    if let Some(m) = AGO.captures(t)
        && let Some(n) = number(&m[1])
    {
        return Ok(match &m[2] {
            "month" => {
                let a = Clock::add_months(today, -n);
                span(c.day0(a), c.day0(Clock::add_months(a, 1)))
            }
            "day" => {
                let d = Clock::add_days(today, -n);
                span(c.day0(d), c.day0(Clock::add_days(d, 1)))
            }
            "week" => {
                let d = Clock::add_days(today, -7 * n);
                let monday = Clock::add_days(d, -i64::from(d.weekday().to_monday_zero_offset()));
                span(c.day0(monday), c.day0(Clock::add_days(monday, 7)))
            }
            "hour" | "hr" => {
                let p = now - n * HOUR;
                let z = Timestamp::from_microsecond(p)?.to_zoned(c.tz());
                let a = c.at(z.date(), i64::from(z.hour()), 0);
                span(a, a + HOUR)
            }
            _ => {
                let p = now - n * MIN;
                span(p - MIN, p + MIN)
            }
        });
    }
    // month names: `october`, `in september` (the most recent occurrence)
    if t.chars().all(|ch| ch.is_ascii_alphabetic())
        && let Some(mon) = month_of(t)
    {
        let mut a = Date::new(today.year(), mon, 1)?;
        if c.day0(a) > now {
            a = Date::new(today.year() - 1, mon, 1)?;
        }
        return Ok(span(c.day0(a), c.day0(Clock::add_months(a, 1)).min(now)));
    }
    if let Some(d) = month_day(t, c) {
        return Ok(span(c.day0(d), c.day0(Clock::add_days(d, 1))));
    }
    // the existing CLI forms: a duration ago (`7d` = since then), @unix, RFC 3339
    if let Some(m) = CLI_DUR.captures(t) {
        let x: f64 = m[1].parse()?;
        let unit = match &m[2] {
            "s" => 1e6,
            "m" => 60e6,
            "h" => 3_600e6,
            "d" => 86_400e6,
            _ => 604_800e6,
        };
        return Ok(span(now - (x * unit) as i64, now));
    }
    if let Some(s) = t.strip_prefix('@')
        && let Ok(v) = s.parse::<f64>()
    {
        let a = (v * 1e6) as i64;
        return Ok(span(a, a + 1_000_000));
    }
    if let Some(a) = oojuro_capture::time::parse_rfc3339_us(text.trim()) {
        return Ok(span(a, a + 1_000_000));
    }
    Err(bad())
}

const PICKS: [(&str, Pick); 7] = [
    ("first", Pick::First),
    ("earliest", Pick::First),
    ("last", Pick::Last),
    ("latest", Pick::Last),
    ("final", Pick::Last),
    ("best", Pick::Top),
    ("top", Pick::Top),
];

/// `the first "failed test"`, `the deploy`, `last "migration"`, `@4211` -> an anchor.
pub fn anchor(text: &str) -> Result<Anchor> {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let mut pick = None;
    while let Some(w) = words.first() {
        let l = w.to_lowercase();
        if let Some((_, p)) = PICKS.iter().find(|(n, _)| *n == l) {
            pick = Some(*p);
        } else if !matches!(l.as_str(), "the" | "a" | "an") {
            break;
        }
        words.remove(0);
    }
    let mut q = words.join(" ");
    for quote in ['"', '\''] {
        if q.len() >= 2 && q.starts_with(quote) && q.ends_with(quote) {
            q = q[1..q.len() - 1].to_string();
            break;
        }
    }
    if q.is_empty() {
        return Err("empty anchor".into());
    }
    let id = q.strip_prefix('@').and_then(|x| x.parse::<u64>().ok());
    Ok(Anchor { query: q, id, pick: pick.unwrap_or(Pick::Last), pick_given: pick.is_some() })
}

const RELATIONS: [(&str, Rel); 13] = [
    ("before ", Rel::Before),
    ("until ", Rel::Before),
    ("after ", Rel::After),
    ("since ", Rel::After),
    ("around ", Rel::Around),
    ("the day of ", Rel::Day),
    ("day of ", Rel::Day),
    ("the day ", Rel::Day),
    ("the hour of ", Rel::Hour),
    ("hour of ", Rel::Hour),
    ("the session of ", Rel::Session),
    ("session of ", Rel::Session),
    ("during ", Rel::Session),
];

/// A phrase: a period, a combination, or an anchored period.
pub fn parse(text: &str, c: &Clock) -> Result<Phrase> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let low = t.to_lowercase();
    if low.len() == t.len()
        && let Some(m) = BETWEEN.captures(&low)
    {
        let (a, b) = (parse(&t[m.get(1).unwrap().range()], c)?, parse(&t[m.get(2).unwrap().range()], c)?);
        return Ok(match (a, b) {
            (Phrase::Period(a), Phrase::Period(b)) => Phrase::Period(Period { from: a.from, to: b.to }),
            (a, b) => Phrase::Between(Box::new(a), Box::new(b)),
        });
    }
    // a whole phrase that is a period wins (`the day before yesterday`, `around 9am`)
    if let Ok(p) = period(&t, c) {
        return Ok(Phrase::Period(p));
    }
    for (kw, rel) in RELATIONS {
        if !low.starts_with(kw) || low.len() != t.len() {
            continue;
        }
        let rest = &t[kw.len()..];
        let Ok(p) = period(rest, c) else {
            return Ok(Phrase::Anchored { rel, anchor: anchor(rest)? });
        };
        return Ok(Phrase::Period(match rel {
            Rel::Before => Period { from: None, to: p.from.unwrap_or(p.to) },
            Rel::After if kw == "after " => span(p.to, c.now_us()),
            Rel::After => span(p.from.unwrap_or(p.to), c.now_us()),
            Rel::Around => span(p.from.unwrap_or(p.to) - HOUR, p.to + HOUR),
            _ => p,
        }));
    }
    Err(format!("not a date phrase: {text:?} (forms: {FORMS})").into())
}

/// One search hit an anchor phrase may resolve to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchorHit {
    pub id: u64,
    pub ts_us: i64,
    pub bits: f64,
    pub session: u64,
}

/// Consecutive relevant hits of one session within this long are one occurrence.
pub const OCCURRENCE_GAP_US: i64 = 30 * MIN;

/// An occurrence of an anchor phrase: hits of one session close in time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Occurrence {
    pub first_id: u64,
    pub first_us: i64,
    pub last_id: u64,
    pub last_us: i64,
    pub session: String,
    pub bits: f64,
    pub hits: usize,
}

/// An anchor resolved to a record and a period.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub period: Period,
    pub id: u64,
    pub ts_us: i64,
    pub session: u64,
    pub pick: Pick,
    /// No ordinal was given and the occurrences fall on more than one day.
    pub ambiguous: bool,
    /// Relevant hits (at least `max(min_bits, best / 2)` bits).
    pub candidates: usize,
    pub occurrences: Vec<Occurrence>,
}

/// The bits a hit needs to count: `max(min_bits, best / 2)`, or `best / 2`
/// when even the best hit saves less than `min_bits`. A phrase found in a
/// single record saves almost nothing (the engine's background then is
/// that record, `docs/SEARCH_FIXES.md`), and a distinctive phrase is
/// exactly what an anchor should be.
pub fn relevance_floor(best: f64, min_bits: f64) -> f64 {
    if best >= min_bits { min_bits.max(best / 2.0) } else { best / 2.0 }
}

/// Pick the anchor among `hits` (any order) and turn it into a period.
/// Relevant hits save at least [`relevance_floor`] bits; they group
/// into occurrences (one session, gaps under 30 min); `first` / `last` pick
/// the earliest / latest occurrence, `top` the one with the most bits. The
/// anchor time is the occurrence's first hit, or its last for `after`.
/// `span_us` is the reach of `before` / `after`. `Session` periods are
/// widened by the caller to the session's records.
pub fn resolve(a: &Anchor, rel: Rel, hits: &[AnchorHit], min_bits: f64, span_us: i64, c: &Clock) -> Option<Resolved> {
    let best = hits.iter().map(|h| h.bits).fold(f64::MIN, f64::max);
    let floor = relevance_floor(best, min_bits);
    let mut rel_hits: Vec<AnchorHit> = hits.iter().copied().filter(|h| h.bits >= floor).collect();
    rel_hits.sort_by_key(|h| (h.ts_us, h.id));
    if rel_hits.is_empty() {
        return None;
    }
    let mut occ: Vec<(Occurrence, u64)> = Vec::new();
    for h in &rel_hits {
        let joins = occ.iter_mut().rev().find(|(o, s)| *s == h.session && h.ts_us - o.last_us <= OCCURRENCE_GAP_US);
        match joins {
            Some((o, _)) => {
                o.last_id = h.id;
                o.last_us = h.ts_us;
                o.bits = o.bits.max(h.bits);
                o.hits += 1;
            }
            None => occ.push((
                Occurrence {
                    first_id: h.id,
                    first_us: h.ts_us,
                    last_id: h.id,
                    last_us: h.ts_us,
                    session: format!("{:016x}", h.session),
                    bits: h.bits,
                    hits: 1,
                },
                h.session,
            )),
        }
    }
    occ.sort_by_key(|(o, _)| (o.first_us, o.first_id));
    let (o, session) = match a.pick {
        Pick::First => occ.first(),
        Pick::Last => occ.last(),
        Pick::Top => occ.iter().max_by(|x, y| x.0.bits.total_cmp(&y.0.bits).then(x.0.first_us.cmp(&y.0.first_us))),
    }?
    .clone();
    let (id, t) = if rel == Rel::After { (o.last_id, o.last_us) } else { (o.first_id, o.first_us) };
    let period = match rel {
        Rel::Before => span(t - span_us, t),
        Rel::After => span(t, t + span_us),
        Rel::Around => span(t - HOUR, t + HOUR),
        Rel::Hour => span(t - 30 * MIN, t + 30 * MIN),
        Rel::Day => {
            let d = c.date_of(t);
            span(c.day0(d), c.day0(Clock::add_days(d, 1)))
        }
        Rel::Session => span(t, t + 1),
    };
    let days: std::collections::BTreeSet<Date> = rel_hits.iter().map(|h| c.date_of(h.ts_us)).collect();
    let mut top: Vec<Occurrence> = occ.iter().map(|x| x.0.clone()).collect();
    top.sort_by(|x, y| y.bits.total_cmp(&x.bits).then(y.first_us.cmp(&x.first_us)));
    top.truncate(5);
    Some(Resolved {
        period,
        id,
        ts_us: t,
        session,
        pick: a.pick,
        ambiguous: !a.pick_given && days.len() > 1,
        candidates: rel_hits.len(),
        occurrences: top,
    })
}

/// The person's time zone: an IANA name (the `timezone` setting), else the machine's.
pub fn time_zone(name: Option<&str>) -> Result<TimeZone> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => TimeZone::get(n).map_err(|e| format!("unknown time zone {n:?}: {e}").into()),
        None => Ok(TimeZone::try_system().unwrap_or(TimeZone::UTC)),
    }
}

/// Where an anchor phrase is searched, and how far `before` / `after` reach.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    /// Writers searched for anchors (empty = all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<String>,
    /// Leave out what is still in this session's context window (16 hex
    /// digits), so an agent's own words never anchor its query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_context_of: Option<String>,
    /// Reach of `before` / `after` (default 24 h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_us: Option<i64>,
    /// IANA time zone (default: the machine's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

/// An anchor as resolved (`when.anchor` in answers).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnchorOut {
    pub query: String,
    pub id: u64,
    pub ts: String,
    pub ts_us: i64,
    pub agent_id: String,
    pub pick: Pick,
    pub ambiguous: bool,
    pub candidates: usize,
    /// The strongest occurrences (at most 5), when there was a search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occurrences: Vec<Occurrence>,
    /// What was picked and how to pick another.
    pub note: String,
}

/// A resolved `--when` phrase: the interval it means, echoed back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct When {
    pub phrase: String,
    pub tz: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_us: Option<i64>,
    pub to_us: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub to: String,
    /// Local wall-clock rendering of the interval.
    pub local: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<AnchorOut>,
}

/// Anchor hits searched per phrase.
pub const ANCHOR_K: usize = 1000;

fn local(us: i64, tz: &TimeZone) -> String {
    Timestamp::from_microsecond(us)
        .map(|t| t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M %Z").to_string())
        .unwrap_or_default()
}

/// Resolve `phrase` against the journal: date phrases by rules, anchors by
/// searching (time-ordered relevant hits, the caller's context left out).
pub fn resolve_when(r: &crate::Reader, phrase: &str, scope: &Scope, now_us: i64, min_bits: f64) -> Result<When> {
    let tz = time_zone(scope.tz.as_deref())?;
    let c = Clock::new(now_us, tz.clone())?;
    let span_us = scope.span_us.unwrap_or(DAY);
    let mut anchors = Vec::new();
    let p = resolve_phrase(r, &parse(phrase, &c)?, scope, &c, span_us, min_bits, &mut anchors)?;
    let fmt = oojuro_capture::time::format_rfc3339_us;
    Ok(When {
        phrase: phrase.to_string(),
        tz: tz.iana_name().unwrap_or("local").to_string(),
        from_us: p.from,
        to_us: p.to,
        from: p.from.map(fmt),
        to: fmt(p.to),
        local: format!("{} .. {}", p.from.map_or("(start)".to_string(), |f| local(f, &tz)), local(p.to, &tz)),
        anchors,
    })
}

fn resolve_phrase(
    r: &crate::Reader,
    p: &Phrase,
    scope: &Scope,
    c: &Clock,
    span_us: i64,
    min_bits: f64,
    out: &mut Vec<AnchorOut>,
) -> Result<Period> {
    match p {
        Phrase::Period(p) => Ok(*p),
        Phrase::Between(a, b) => {
            let a = resolve_phrase(r, a, scope, c, span_us, min_bits, out)?;
            let b = resolve_phrase(r, b, scope, c, span_us, min_bits, out)?;
            Ok(Period { from: a.from, to: b.to })
        }
        Phrase::Anchored { rel, anchor: a } => {
            let db = r.db();
            let hits: Vec<AnchorHit> = match a.id {
                Some(id) => {
                    let x = db.info(id).ok_or_else(|| format!("no record @{id}"))?;
                    vec![AnchorHit { id, ts_us: x.ts_us, bits: f64::MAX, session: x.session }]
                }
                None => {
                    let args = crate::SearchArgs {
                        query: a.query.clone(),
                        k: ANCHOR_K,
                        filters: crate::Filters {
                            writers: scope.writers.clone(),
                            outside_context_of: scope.outside_context_of.clone(),
                            ..crate::Filters::default()
                        },
                        excerpt_bytes: 1,
                        max_query_bytes: crate::config::HARD_MAX_QUERY_BYTES,
                    };
                    r.search_hits(&args)?
                        .into_iter()
                        .filter_map(|(bits, id)| {
                            db.info(id).map(|x| AnchorHit { id, ts_us: x.ts_us, bits, session: x.session })
                        })
                        .collect()
                }
            };
            let res = resolve(a, *rel, &hits, if a.id.is_some() { 0.0 } else { min_bits }, span_us, c)
                .ok_or_else(|| format!("anchor {:?} not found in the journal", a.query))?;
            let mut period = res.period;
            if *rel == Rel::Session {
                let ts: Vec<i64> = db.session_info(res.session).iter().map(|x| x.ts_us).collect();
                if let (Some(lo), Some(hi)) = (ts.iter().min(), ts.iter().max()) {
                    period = span(*lo, hi + 1);
                }
            }
            let pick = match res.pick {
                Pick::First => "the first",
                Pick::Last => "the latest",
                Pick::Top => "the strongest",
            };
            let n = res.occurrences.len();
            let note = if a.id.is_some() {
                format!("record @{}", res.id)
            } else if res.ambiguous {
                format!("{pick} of {n}+ occurrences on several days; say first/last/top, or use @id from `occurrences`")
            } else {
                format!("{pick} occurrence")
            };
            out.push(AnchorOut {
                query: a.query.clone(),
                id: res.id,
                ts: oojuro_capture::time::format_rfc3339_us(res.ts_us),
                ts_us: res.ts_us,
                agent_id: format!("{:016x}", res.session),
                pick: res.pick,
                ambiguous: res.ambiguous,
                candidates: res.candidates,
                occurrences: if a.id.is_some() { Vec::new() } else { res.occurrences },
                note,
            });
            Ok(period)
        }
    }
}

/// Narrow explicit bounds by a resolved `--when` (the intersection).
pub fn narrow(f: &mut crate::Filters, w: &When) {
    if let Some(a) = w.from_us {
        f.from_us = Some(f.from_us.map_or(a, |x| x.max(a)));
    }
    // filters are inclusive at `to`; a period is half-open
    let b = w.to_us - 1;
    f.to_us = Some(f.to_us.map_or(b, |x| x.min(b)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::offset;

    fn clock_at(tz: TimeZone, y: i16, mo: i8, d: i8, h: i8) -> Clock {
        let now = jiff::civil::date(y, mo, d).at(h, 0, 0, 0).to_zoned(tz).unwrap();
        Clock { now }
    }

    /// The prototype's 43 self-tests (`scripts/activity/dates.py --test`):
    /// 32 periods, 6 anchors, 2 resolutions and 3 phrases that must not parse.
    #[test]
    fn the_prototype_cases_all_pass() {
        let tz = TimeZone::fixed(offset(-4));
        let c = clock_at(tz.clone(), 2026, 10, 21, 15); // a Wednesday
        let d = |y: i16, mo: i8, dd: i8, h: i8| -> i64 {
            jiff::civil::date(y, mo, dd).at(h, 0, 0, 0).to_zoned(tz.clone()).unwrap().timestamp().as_microsecond()
        };
        let now = c.now_us();
        let cases: Vec<(&str, Option<i64>, i64)> = vec![
            ("yesterday", Some(d(2026, 10, 20, 0)), d(2026, 10, 21, 0)),
            ("today", Some(d(2026, 10, 21, 0)), now),
            ("last tuesday", Some(d(2026, 10, 20, 0)), d(2026, 10, 21, 0)),
            ("tuesday", Some(d(2026, 10, 20, 0)), d(2026, 10, 21, 0)),
            ("wednesday", Some(d(2026, 10, 21, 0)), now),
            ("last wednesday", Some(d(2026, 10, 14, 0)), d(2026, 10, 15, 0)),
            ("monday before last", Some(d(2026, 10, 12, 0)), d(2026, 10, 13, 0)),
            ("3 days ago", Some(d(2026, 10, 18, 0)), d(2026, 10, 19, 0)),
            ("three days ago", Some(d(2026, 10, 18, 0)), d(2026, 10, 19, 0)),
            ("2 hours ago", Some(d(2026, 10, 21, 13)), d(2026, 10, 21, 14)),
            ("Oct 14", Some(d(2026, 10, 14, 0)), d(2026, 10, 15, 0)),
            ("october 14th", Some(d(2026, 10, 14, 0)), d(2026, 10, 15, 0)),
            ("14 oct", Some(d(2026, 10, 14, 0)), d(2026, 10, 15, 0)),
            ("10/14", Some(d(2026, 10, 14, 0)), d(2026, 10, 15, 0)),
            ("Dec 3", Some(d(2025, 12, 3, 0)), d(2025, 12, 4, 0)),
            ("2026-10-14", Some(d(2026, 10, 14, 0)), d(2026, 10, 15, 0)),
            ("last week", Some(d(2026, 10, 12, 0)), d(2026, 10, 19, 0)),
            ("this week", Some(d(2026, 10, 19, 0)), now),
            ("last month", Some(d(2026, 9, 1, 0)), d(2026, 10, 1, 0)),
            ("september", Some(d(2026, 9, 1, 0)), d(2026, 10, 1, 0)),
            ("past 3 days", Some(d(2026, 10, 18, 15)), now),
            ("the last few hours", Some(d(2026, 10, 21, 12)), now),
            ("yesterday afternoon", Some(d(2026, 10, 20, 12)), d(2026, 10, 20, 18)),
            ("last tuesday morning", Some(d(2026, 10, 20, 6)), d(2026, 10, 20, 12)),
            ("this morning", Some(d(2026, 10, 21, 6)), d(2026, 10, 21, 12)),
            ("last night", Some(d(2026, 10, 20, 18)), d(2026, 10, 21, 6)),
            ("yesterday at 3pm", Some(d(2026, 10, 20, 15)), d(2026, 10, 20, 16)),
            ("oct 14 around 9am", Some(d(2026, 10, 14, 8)), d(2026, 10, 14, 10)),
            ("between monday and yesterday", Some(d(2026, 10, 19, 0)), d(2026, 10, 21, 0)),
            ("since monday", Some(d(2026, 10, 19, 0)), now),
            ("before oct 14", None, d(2026, 10, 14, 0)),
            ("7d", Some(d(2026, 10, 14, 15)), now),
        ];
        let mut passed = 0;
        for (phrase, from, to) in &cases {
            match parse(phrase, &c) {
                Ok(Phrase::Period(p)) if p.from == *from && p.to == *to => passed += 1,
                other => panic!("{phrase}: {other:?}"),
            }
        }
        let anchors = [
            ("before the deploy", Rel::Before, "deploy", Pick::Last, false),
            ("after the first \"failed test\"", Rel::After, "failed test", Pick::First, true),
            ("the day the build broke", Rel::Day, "build broke", Pick::Last, false),
            ("around \"migration 0042\"", Rel::Around, "migration 0042", Pick::Last, false),
            ("during the last release", Rel::Session, "release", Pick::Last, true),
            ("after top 'oom killed'", Rel::After, "oom killed", Pick::Top, true),
        ];
        for (phrase, rel, q, pick, given) in anchors {
            let want = Phrase::Anchored { rel, anchor: Anchor { query: q.into(), id: None, pick, pick_given: given } };
            assert_eq!(parse(phrase, &c).unwrap(), want, "{phrase}");
            passed += 1;
        }
        let utc = clock_at(TimeZone::UTC, 2026, 10, 21, 15);
        let hits = [
            AnchorHit { id: 1, ts_us: 1_000_000_000_000_000, bits: 30.0, session: 0 },
            AnchorHit { id: 2, ts_us: 1_000_090_000_000_000, bits: 40.0, session: 0 },
            AnchorHit { id: 3, ts_us: 1_000_190_000_000_000, bits: 10.0, session: 0 },
        ];
        let last = Anchor { query: "x".into(), id: None, pick: Pick::Last, pick_given: false };
        let r = resolve(&last, Rel::Before, &hits, 8.0, DAY, &utc).unwrap();
        assert!(r.id == 2 && r.ambiguous && r.candidates == 2, "resolve last: {r:?}");
        passed += 1;
        let first = Anchor { query: "x".into(), id: None, pick: Pick::First, pick_given: true };
        let r = resolve(&first, Rel::After, &hits, 8.0, DAY, &utc).unwrap();
        assert!(r.id == 1 && !r.ambiguous, "resolve first: {r:?}");
        passed += 1;
        for junk in ["purple elephant", "13/45", "the"] {
            assert!(!matches!(parse(junk, &c), Ok(Phrase::Period(_))), "accepted {junk}");
            passed += 1;
        }
        assert_eq!(passed, 43);
    }

    #[test]
    fn local_days_follow_daylight_saving_and_the_year_wraps() {
        let ny = TimeZone::get("America/New_York").unwrap();
        // 2026-11-01: clocks go back at 02:00, so that day has 25 hours
        let c = clock_at(ny.clone(), 2026, 11, 2, 9);
        let Phrase::Period(p) = parse("yesterday", &c).unwrap() else { panic!() };
        assert_eq!(p.to - p.from.unwrap(), 25 * HOUR);
        let Phrase::Period(p) = parse("yesterday morning", &c).unwrap() else { panic!() };
        assert_eq!(p.to - p.from.unwrap(), 6 * HOUR, "06:00-12:00 local on the long day");
        // 2026-03-08: clocks go forward, 23 hours
        let c = clock_at(ny.clone(), 2026, 3, 9, 9);
        let Phrase::Period(p) = parse("yesterday", &c).unwrap() else { panic!() };
        assert_eq!(p.to - p.from.unwrap(), 23 * HOUR);
        // the same phrase in two zones is a different instant
        let utc = clock_at(TimeZone::UTC, 2026, 3, 9, 14);
        let Phrase::Period(a) = parse("yesterday", &utc).unwrap() else { panic!() };
        let Phrase::Period(b) = parse("yesterday", &c).unwrap() else { panic!() };
        assert_ne!(a.from, b.from);
        // year wrap: in January, `december` and `dec 3` are last year's; `last month` too
        let jan = clock_at(TimeZone::UTC, 2027, 1, 5, 12);
        let day = |y, m, d| jiff::civil::date(y, m, d).to_zoned(TimeZone::UTC).unwrap().timestamp().as_microsecond();
        let Phrase::Period(p) = parse("december", &jan).unwrap() else { panic!() };
        assert_eq!((p.from, p.to), (Some(day(2026, 12, 1)), day(2027, 1, 1)));
        let Phrase::Period(p) = parse("dec 3", &jan).unwrap() else { panic!() };
        assert_eq!(p.from, Some(day(2026, 12, 3)));
        let Phrase::Period(p) = parse("last month", &jan).unwrap() else { panic!() };
        assert_eq!((p.from, p.to), (Some(day(2026, 12, 1)), day(2027, 1, 1)));
        let Phrase::Period(p) = parse("last week", &jan).unwrap() else { panic!() };
        assert_eq!(p.from, Some(day(2026, 12, 28)), "weeks start on Monday, across the year");
        // future dates: `this week` and `today` end now; an explicit future day is kept as asked
        let Phrase::Period(p) = parse("this week", &jan).unwrap() else { panic!() };
        assert_eq!(p.to, jan.now_us());
        let Phrase::Period(p) = parse("2027-02-01", &jan).unwrap() else { panic!() };
        assert_eq!(p.from, Some(day(2027, 2, 1)));
        // a bare clock, `around`, and `the day before yesterday` (the prototype read it as an anchor)
        let Phrase::Period(p) = parse("around 9am", &jan).unwrap() else { panic!() };
        assert_eq!(p.from, Some(day(2027, 1, 5) + 8 * HOUR));
        let Phrase::Period(p) = parse("the day before yesterday", &jan).unwrap() else { panic!() };
        assert_eq!(p.from, Some(day(2027, 1, 3)));
    }

    #[test]
    fn anchors_pick_occurrences_and_flag_ambiguity() {
        let c = clock_at(TimeZone::UTC, 2026, 10, 21, 15);
        let t0 = 1_791_000_000_000_000;
        let h = |id, min: i64, bits, session| AnchorHit { id, ts_us: t0 + min * MIN, bits, session };
        // session 7: three hits within 30 minutes = one occurrence; session 8 the next day
        let hits =
            [h(10, 0, 20.0, 7), h(11, 10, 25.0, 7), h(12, 35, 22.0, 7), h(20, 1500, 21.0, 8), h(30, 1600, 3.0, 8)];
        let a = |pick, given| Anchor { query: "q".into(), id: None, pick, pick_given: given };
        let r = resolve(&a(Pick::Last, false), Rel::Before, &hits, 8.0, DAY, &c).unwrap();
        assert_eq!((r.id, r.candidates, r.occurrences.len()), (20, 4, 2));
        assert!(r.ambiguous, "two days, no ordinal");
        assert_eq!(r.period, span(t0 + 1500 * MIN - DAY, t0 + 1500 * MIN));
        let r = resolve(&a(Pick::First, true), Rel::After, &hits, 8.0, DAY, &c).unwrap();
        assert_eq!(r.id, 12, "after: the last hit of the first occurrence");
        assert!(!r.ambiguous);
        let r = resolve(&a(Pick::Top, true), Rel::Hour, &hits, 8.0, DAY, &c).unwrap();
        assert_eq!((r.id, r.period), (10, span(t0 - 30 * MIN, t0 + 30 * MIN)));
        // a lone weak hit (a phrase found once) still anchors; none at all does not
        let r = resolve(&a(Pick::Last, false), Rel::Day, &hits[4..], 8.0, DAY, &c).unwrap();
        assert_eq!(r.id, 30);
        assert!(resolve(&a(Pick::Last, false), Rel::Day, &[], 8.0, DAY, &c).is_none());
        assert_eq!(anchor("@4211").unwrap().id, Some(4211));
        assert_eq!(anchor("the latest \"deploy-prod\"").unwrap().query, "deploy-prod");
    }
}
