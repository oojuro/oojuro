"""Date phrases and event-relative anchors by rules (no model).

    python3 -I dates.py --test
    python3 -I dates.py "last tuesday afternoon" [--now 2026-10-21T15:00:00-04:00]
    python3 -I dates.py 'before "deploy"' --resolve [--writer W]   # anchor via `oojuro search` (read-only)

`parse(phrase, now)` returns either a period `{"from": dt, "to": dt}` (half
open, local time zone of `now`) or an anchored period `{"rel": before|after|
day|hour|session|around|between, "anchor": {"query", "pick"}, ...}` whose
bounds come from a search hit. Grammar: docs/ACTIVITY_SEARCH.md, "Dates".
"""
import json
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone

WEEKDAYS = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]
MONTHS = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october",
          "november", "december"]
UNITS = {"minute": 60, "min": 60, "hour": 3600, "hr": 3600, "h": 3600, "day": 86400, "d": 86400,
         "week": 604800, "w": 604800, "month": None, "year": None}
PARTS = {"morning": (6, 12), "afternoon": (12, 18), "evening": (18, 24), "night": (18, 30)}
NUM_WORDS = {"a": 1, "an": 1, "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7,
             "eight": 8, "nine": 9, "ten": 10, "couple": 2, "few": 3}
PICKS = {"first": "first", "earliest": "first", "last": "last", "latest": "last", "final": "last",
         "best": "top", "top": "top"}


class ParseError(ValueError):
    pass


def day_start(t):
    return t.replace(hour=0, minute=0, second=0, microsecond=0)


def month_start(t):
    return day_start(t).replace(day=1)


def add_months(t, n):
    m = t.month - 1 + n
    return t.replace(year=t.year + m // 12, month=m % 12 + 1, day=1)


def span(a, b):
    return {"from": a, "to": b}


def _month_day(text, now):
    """`oct 14`, `october 14th`, `14 oct`, `10/14`, `2026-10-14` -> that day."""
    t = text
    m = re.fullmatch(r"(\d{4})-(\d{2})-(\d{2})", t)
    if m:
        try:
            return day_start(now.replace(year=int(m[1]), month=int(m[2]), day=int(m[3])))
        except ValueError:
            return None
    mon = day = None
    m = re.fullmatch(r"([a-z]{3,9})\.? (\d{1,2})(?:st|nd|rd|th)?(?:,? (\d{4}))?", t)
    if m and any(x.startswith(m[1]) for x in MONTHS) and len(m[1]) >= 3:
        mon = next(i for i, x in enumerate(MONTHS) if x.startswith(m[1])) + 1
        day, year = int(m[2]), m[3]
    m2 = re.fullmatch(r"(\d{1,2})(?:st|nd|rd|th)? ([a-z]{3,9})(?: (\d{4}))?", t)
    if mon is None and m2 and any(x.startswith(m2[2]) for x in MONTHS):
        mon = next(i for i, x in enumerate(MONTHS) if x.startswith(m2[2])) + 1
        day, year = int(m2[1]), m2[3]
    m3 = re.fullmatch(r"(\d{1,2})/(\d{1,2})(?:/(\d{4}))?", t)
    if mon is None and m3:  # month/day (US order), stated in the docs
        mon, day, year = int(m3[1]), int(m3[2]), m3[3]
    if mon is None:
        return None
    y = int(year) if year else now.year
    try:
        d = day_start(now.replace(year=y, month=mon, day=day))
    except ValueError:
        return None
    if not year and d > now:  # the most recent past occurrence
        d = d.replace(year=y - 1)
    return d


def _clock(text):
    """`3pm`, `15:30`, `9am` -> (hour, minute)."""
    m = re.fullmatch(r"(\d{1,2})(?::(\d{2}))?\s*(am|pm)?", text)
    if not m or (not m[2] and not m[3]):
        return None
    h, mi = int(m[1]), int(m[2] or 0)
    if m[3] == "pm" and h < 12:
        h += 12
    if m[3] == "am" and h == 12:
        h = 0
    return (h, mi) if h < 24 and mi < 60 else None


def _number(w):
    return int(w) if w.isdigit() else NUM_WORDS.get(w)


def period(text, now):
    """A plain (non-anchored) phrase -> {"from", "to"}; ParseError if not one."""
    t = " ".join(text.lower().replace(",", " ").split())
    t = re.sub(r"^(?:on|in|at|during) ", "", t)
    today = day_start(now)
    # part of a day: `<day phrase> morning`, `this afternoon`, `last night`, `tonight`
    if t in ("tonight", "this evening"):
        return span(today + timedelta(hours=18), today + timedelta(hours=24))
    if t == "last night":
        return span(today - timedelta(hours=6), today + timedelta(hours=6))
    m = re.fullmatch(r"(.*?) ?(morning|afternoon|evening|night)", t)
    if m and m[2] in PARTS:
        base = today if m[1] in ("this", "") else period(m[1], now)["from"]
        a, b = PARTS[m[2]]
        return span(base + timedelta(hours=a), base + timedelta(hours=b))
    # `<day phrase> at 3pm` / `around 3pm`: the clock hour (around: +-1 h)
    m = re.fullmatch(r"(?:(.*?) )?(at|around) (.+)", t)
    if m and _clock(m[3]):
        h, mi = _clock(m[3])
        base = today if not m[1] else period(m[1], now)["from"]
        c = base + timedelta(hours=h, minutes=mi)
        return span(c - timedelta(hours=1), c + timedelta(hours=1)) if m[2] == "around" else \
            span(c, c + timedelta(hours=1))
    if t in ("now", "just now"):
        return span(now - timedelta(minutes=15), now)
    if t == "today":
        return span(today, now)
    if t == "yesterday":
        return span(today - timedelta(days=1), today)
    if t in ("day before yesterday", "the day before yesterday"):
        return span(today - timedelta(days=2), today - timedelta(days=1))
    # weekdays: bare = most recent on or before today; last = strictly before today
    m = re.fullmatch(r"(last |this |past )?(" + "|".join(WEEKDAYS) + r")( before last)?", t)
    if m:
        wd = WEEKDAYS.index(m[2])
        back = (today.weekday() - wd) % 7
        if m[1] == "last " and back == 0:
            back = 7
        if m[3]:
            back += 7
        d = today - timedelta(days=back)
        return span(d, min(d + timedelta(days=1), now))
    # this/last/previous unit (calendar-aligned)
    m = re.fullmatch(r"(this|last|previous|past) (hour|day|week|month|year)", t)
    if m:
        unit, prev = m[2], m[1] != "this"
        if m[1] == "past":  # rolling: the past day = the last 24 hours
            return span(now - timedelta(seconds={"hour": 3600, "day": 86400, "week": 604800}.get(unit, 0))
                        if unit in ("hour", "day", "week") else add_months(now, -1 if unit == "month" else -12),
                        now)
        if unit == "hour":
            a = now.replace(minute=0, second=0, microsecond=0) - timedelta(hours=prev)
            return span(a, min(a + timedelta(hours=1), now))
        if unit == "day":
            return period("yesterday" if prev else "today", now)
        if unit == "week":  # weeks start on Monday
            a = today - timedelta(days=today.weekday()) - timedelta(days=7 * prev)
            return span(a, min(a + timedelta(days=7), now))
        if unit == "month":
            a = add_months(month_start(now), -prev)
            return span(a, min(add_months(a, 1), now))
        a = month_start(now).replace(month=1, year=now.year - prev)
        return span(a, min(a.replace(year=a.year + 1), now))
    # rolling windows ending now: `last 3 days`, `past 2 hours`, `the last few hours`
    m = re.fullmatch(r"(?:the )?(?:last|past) (\w+) (minute|min|hour|hr|day|week)s?", t)
    if m and _number(m[1]):
        return span(now - timedelta(seconds=_number(m[1]) * UNITS[m[2]]), now)
    # `N units ago`: the calendar unit (day, hour, week) containing that moment
    m = re.fullmatch(r"(\w+) (minute|min|hour|hr|day|week|month)s? ago", t)
    if m and _number(m[1]):
        n, unit = _number(m[1]), m[2]
        if unit == "month":
            a = add_months(month_start(now), -n)
            return span(a, add_months(a, 1))
        p = now - timedelta(seconds=n * UNITS[unit])
        if unit in ("day",):
            return span(day_start(p), day_start(p) + timedelta(days=1))
        if unit == "week":
            a = day_start(p) - timedelta(days=p.weekday())
            return span(a, a + timedelta(days=7))
        if unit in ("hour", "hr"):
            a = p.replace(minute=0, second=0, microsecond=0)
            return span(a, a + timedelta(hours=1))
        return span(p - timedelta(minutes=1), p + timedelta(minutes=1))
    # month names: `october`, `in september`
    if t in MONTHS or (len(t) >= 3 and any(x.startswith(t) for x in MONTHS) and t.isalpha()):
        mon = next(i for i, x in enumerate(MONTHS) if x.startswith(t)) + 1
        a = month_start(now).replace(month=mon)
        if a > now:
            a = a.replace(year=a.year - 1)
        return span(a, min(add_months(a, 1), now))
    d = _month_day(t, now)
    if d is not None:
        return span(d, d + timedelta(days=1))
    # the existing CLI forms: RFC 3339, @unix, a duration ago (`7d` = since then)
    m = re.fullmatch(r"(\d+(?:\.\d+)?)(s|m|h|d|w)", t)
    if m:
        sec = float(m[1]) * {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}[m[2]]
        return span(now - timedelta(seconds=sec), now)
    if t.startswith("@"):
        a = datetime.fromtimestamp(float(t[1:]), now.tzinfo)
        return span(a, a + timedelta(seconds=1))
    try:
        a = datetime.fromisoformat(text.strip().replace("Z", "+00:00"))
        if a.tzinfo is None:
            a = a.replace(tzinfo=now.tzinfo)
        return span(a, a + timedelta(seconds=1))
    except ValueError:
        pass
    raise ParseError(f"not a date phrase: {text!r}")


def _anchor(text):
    """`the first "failed test"`, `the deploy`, `last "migration"` -> anchor."""
    t = text.strip()
    pick = None
    words = t.split()
    while words and words[0].lower() in ("the", "a", "an") | PICKS.keys():
        w = words.pop(0).lower()
        if w in PICKS:
            pick = PICKS[w]
    q = " ".join(words).strip()
    if len(q) >= 2 and q[0] in "\"'" and q[-1] == q[0]:
        q = q[1:-1]
    if not q:
        raise ParseError("empty anchor")
    return {"query": q, "pick": pick or "last", "pick_given": pick is not None}


def parse(text, now):
    """A phrase -> a period, a bounded combination, or an anchored period."""
    t = " ".join(text.split())
    low = t.lower()
    m = re.fullmatch(r"(?:between|from) (.+?) (?:and|to|until) (.+)", low)
    if m:
        a, b = parse(t[len(t) - len(low) + m.start(1):m.end(1)], now), parse(t[m.start(2):], now)
        if "from" in a and "from" in b:
            return span(a["from"], b["to"])
        return {"rel": "between", "a": a, "b": b}
    for kw, rel in (("before ", "before"), ("until ", "before"), ("after ", "after"), ("since ", "after"),
                    ("around ", "around"), ("the day of ", "day"), ("day of ", "day"),
                    ("the day ", "day"), ("the hour of ", "hour"), ("hour of ", "hour"),
                    ("the session of ", "session"), ("session of ", "session"), ("during ", "during")):
        if low.startswith(kw):
            rest = t[len(kw):]
            try:
                p = period(rest, now)
            except ParseError:
                return {"rel": "session" if rel == "during" else rel, "anchor": _anchor(rest)}
            if rel == "before":
                return span(None, p["from"])
            if rel == "after":
                return span(p["to"] if kw == "after " else p["from"], now)
            if rel == "around":
                return span(p["from"] - timedelta(hours=1), p["to"] + timedelta(hours=1))
            return p
    return period(t, now)


SPAN_DEFAULT = timedelta(hours=24)


def resolve_anchor(a, hits, rel, span_=SPAN_DEFAULT, tz=timezone.utc):
    """Pick the anchor hit and turn it into a period.

    `hits`: search results [{"id", "ts_us", "score_bits", "session"}] already
    filtered to the query scope. Relevant hits: score >= half the best. The
    pick: first/last by time, or top by score; default last. Ambiguous when
    no pick was given and relevant hits fall on more than one day."""
    if not hits:
        return {"error": "anchor not found", "query": a["query"]}
    best = max(h["score_bits"] for h in hits)
    rel_hits = [h for h in hits if h["score_bits"] >= best / 2]
    rel_hits.sort(key=lambda h: h["ts_us"])
    if a["pick"] == "first":
        h = rel_hits[0]
    elif a["pick"] == "top":
        h = max(rel_hits, key=lambda h: (h["score_bits"], h["ts_us"]))
    else:
        h = rel_hits[-1]
    days = sorted({datetime.fromtimestamp(x["ts_us"] / 1e6, tz).date().isoformat() for x in rel_hits})
    t = datetime.fromtimestamp(h["ts_us"] / 1e6, tz)
    if rel == "before":
        p = span(t - span_, t)
    elif rel == "after":
        p = span(t, t + span_)
    elif rel == "around":
        p = span(t - timedelta(hours=1), t + timedelta(hours=1))
    elif rel == "hour":
        p = span(t - timedelta(minutes=30), t + timedelta(minutes=30))
    elif rel == "day":
        p = span(day_start(t), day_start(t) + timedelta(days=1))
    else:  # session: the caller widens to the anchor's session bounds
        p = span(t, t)
    return {**p, "anchor_id": h["id"], "anchor_ts_us": h["ts_us"], "anchor_session": h.get("session"),
            "candidates": len(rel_hits), "candidate_days": len(days),
            "ambiguous": not a["pick_given"] and len(days) > 1}


# ------------------------------------------------------------------ tests

def _fmt(p):
    return {k: (v.isoformat() if isinstance(v, datetime) else v) for k, v in p.items()}


def _test():
    tz = timezone(timedelta(hours=-4))
    now = datetime(2026, 10, 21, 15, 0, tzinfo=tz)  # a Wednesday
    d = lambda *a: datetime(*a, tzinfo=tz)  # noqa: E731
    cases = {
        "yesterday": (d(2026, 10, 20), d(2026, 10, 21)),
        "today": (d(2026, 10, 21), now),
        "last tuesday": (d(2026, 10, 20), d(2026, 10, 21)),
        "tuesday": (d(2026, 10, 20), d(2026, 10, 21)),
        "wednesday": (d(2026, 10, 21), now),
        "last wednesday": (d(2026, 10, 14), d(2026, 10, 15)),
        "monday before last": (d(2026, 10, 12), d(2026, 10, 13)),
        "3 days ago": (d(2026, 10, 18), d(2026, 10, 19)),
        "three days ago": (d(2026, 10, 18), d(2026, 10, 19)),
        "2 hours ago": (d(2026, 10, 21, 13), d(2026, 10, 21, 14)),
        "Oct 14": (d(2026, 10, 14), d(2026, 10, 15)),
        "october 14th": (d(2026, 10, 14), d(2026, 10, 15)),
        "14 oct": (d(2026, 10, 14), d(2026, 10, 15)),
        "10/14": (d(2026, 10, 14), d(2026, 10, 15)),
        "Dec 3": (d(2025, 12, 3), d(2025, 12, 4)),
        "2026-10-14": (d(2026, 10, 14), d(2026, 10, 15)),
        "last week": (d(2026, 10, 12), d(2026, 10, 19)),
        "this week": (d(2026, 10, 19), now),
        "last month": (d(2026, 9, 1), d(2026, 10, 1)),
        "september": (d(2026, 9, 1), d(2026, 10, 1)),
        "past 3 days": (d(2026, 10, 18, 15), now),
        "the last few hours": (d(2026, 10, 21, 12), now),
        "yesterday afternoon": (d(2026, 10, 20, 12), d(2026, 10, 20, 18)),
        "last tuesday morning": (d(2026, 10, 20, 6), d(2026, 10, 20, 12)),
        "this morning": (d(2026, 10, 21, 6), d(2026, 10, 21, 12)),
        "last night": (d(2026, 10, 20, 18), d(2026, 10, 21, 6)),
        "yesterday at 3pm": (d(2026, 10, 20, 15), d(2026, 10, 20, 16)),
        "oct 14 around 9am": (d(2026, 10, 14, 8), d(2026, 10, 14, 10)),
        "between monday and yesterday": (d(2026, 10, 19), d(2026, 10, 21)),
        "since monday": (d(2026, 10, 19), now),
        "before oct 14": (None, d(2026, 10, 14)),
        "7d": (d(2026, 10, 14, 15), now),
    }
    bad = 0
    for phrase, (a, b) in cases.items():
        got = parse(phrase, now)
        if (got.get("from"), got.get("to")) != (a, b):
            bad += 1
            print("FAIL", phrase, _fmt(got), a, b)
    anchors = {
        "before the deploy": ("before", "deploy", "last", False),
        'after the first "failed test"': ("after", "failed test", "first", True),
        "the day the build broke": ("day", "build broke", "last", False),
        'around "migration 0042"': ("around", "migration 0042", "last", False),
        "during the last release": ("session", "release", "last", True),
        "after top 'oom killed'": ("after", "oom killed", "top", True),
    }
    for phrase, (rel, q, pick, given) in anchors.items():
        got = parse(phrase, now)
        exp = {"rel": rel, "anchor": {"query": q, "pick": pick, "pick_given": given}}
        if got != exp:
            bad += 1
            print("FAIL", phrase, got)
    hits = [{"id": 1, "ts_us": 1_000_000_000_000_000, "score_bits": 30.0},
            {"id": 2, "ts_us": 1_000_090_000_000_000, "score_bits": 40.0},
            {"id": 3, "ts_us": 1_000_190_000_000_000, "score_bits": 10.0}]
    r = resolve_anchor({"query": "x", "pick": "last", "pick_given": False}, hits, "before")
    if r["anchor_id"] != 2 or not r["ambiguous"] or r["candidates"] != 2:
        bad += 1
        print("FAIL resolve last", r)
    r = resolve_anchor({"query": "x", "pick": "first", "pick_given": True}, hits, "after")
    if r["anchor_id"] != 1 or r["ambiguous"]:
        bad += 1
        print("FAIL resolve first", r)
    for junk in ("purple elephant", "13/45", "the"):
        try:
            got = parse(junk, now)
            if "anchor" not in got:
                bad += 1
                print("FAIL accepted", junk, got)
        except ParseError:
            pass
    print(f"{len(cases) + len(anchors) + 5 - bad}/{len(cases) + len(anchors) + 5} ok")
    return bad == 0


def main():
    a = sys.argv[1:]
    if a == ["--test"]:
        sys.exit(0 if _test() else 1)
    opt = dict(zip(a[1::2], a[2::2])) if len(a) > 1 and a[1] != "--resolve" else {}
    now = datetime.fromisoformat(opt["--now"]) if "--now" in opt else datetime.now().astimezone()
    p = parse(a[0], now)
    if "anchor" in p and "--resolve" in a:
        cmd = ["oojuro", "--json", "search", p["anchor"]["query"], "-k", opt.get("-k", "1000"), "--excerpt-bytes", "16"]
        if "--writer" in a:
            cmd += ["--writer", a[a.index("--writer") + 1]]
        hits = json.loads(subprocess.run(cmd, capture_output=True, text=True, check=True).stdout)
        hits = hits if isinstance(hits, list) else hits.get("results", [])
        r = resolve_anchor(p["anchor"], hits, p["rel"], tz=now.tzinfo)
        print(json.dumps(_fmt(r)))  # ids and times only; no excerpts
    else:
        print(json.dumps(_fmt(p) if "from" in p else p, default=str))


if __name__ == "__main__":
    main()
