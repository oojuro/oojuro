//! Exact capture src/rt exclusion ported from trace dedupe.rs.
struct Members {
    src: Option<(usize, usize)>,
    rt: Option<(usize, usize)>,
}

/// Find the `src` and `rt` members of `args` (a JSON object); None when it
/// is not a well-formed object or names either member twice.
fn members(args: &str) -> Option<Members> {
    let b = args.as_bytes();
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    // end of the string starting at b[i] == '"'
    let string = |i: usize| -> Option<usize> {
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'"' => return Some(j + 1),
                _ => j += 1,
            }
        }
        None
    };
    let mut m = Members {
        src: None,
        rt: None,
    };
    let mut i = 0;
    ws(&mut i);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    ws(&mut i);
    if b.get(i) == Some(&b'}') {
        i += 1;
    } else {
        loop {
            ws(&mut i);
            if b.get(i) != Some(&b'"') {
                return None;
            }
            let ke = string(i)?;
            let key = &b[i + 1..ke - 1];
            i = ke;
            ws(&mut i);
            if b.get(i) != Some(&b':') {
                return None;
            }
            i += 1;
            ws(&mut i);
            let vs = i;
            match b.get(i)? {
                b'"' => i = string(i)?,
                b'{' | b'[' => {
                    let mut depth = 0usize;
                    while i < b.len() {
                        match b[i] {
                            b'"' => {
                                i = string(i)?;
                                continue;
                            }
                            b'{' | b'[' => depth += 1,
                            b'}' | b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    if depth != 0 {
                        return None;
                    }
                }
                _ => {
                    while i < b.len() && !matches!(b[i], b',' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
                    {
                        i += 1;
                    }
                }
            }
            if i == vs {
                return None;
            }
            let slot = match key {
                b"src" => Some(&mut m.src),
                b"rt" => Some(&mut m.rt),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return None;
                }
                *slot = Some((vs, i));
            }
            ws(&mut i);
            match b.get(i)? {
                b',' => i += 1,
                b'}' => {
                    i += 1;
                    break;
                }
                _ => return None,
            }
        }
    }
    ws(&mut i);
    (i == b.len()).then_some(m)
}

/// `src` as capture writes it: a string of digits and colons, no escapes.
fn src_token(args: &str, (a, b): (usize, usize)) -> Option<&str> {
    let t = args[a..b].strip_prefix('"')?.strip_suffix('"')?;
    t.bytes()
        .all(|c| c.is_ascii_digit() || c == b':')
        .then_some(t)
}

/// `rt` as capture writes it: a plain integer.
fn rt_token(args: &str, (a, b): (usize, usize)) -> Option<i64> {
    let t = &args[a..b];
    let v: i64 = t.parse().ok()?;
    (v.to_string() == t).then_some(v)
}

/// The args with the values of `src` and `rt` replaced by `0`, and those
/// values; None when `args` has neither member or they are not in the exact
/// form capture writes (then the args compare whole).
pub(super) fn normalize(args: &str) -> Option<String> {
    let m = members(args)?;
    if m.src.is_none() && m.rt.is_none() {
        return None;
    }
    let _src = m
        .src
        .map(|r| src_token(args, r).map(str::to_string))
        .map_or(Some(None), |x| x.map(Some))?;
    let _rt =
        m.rt.map(|r| rt_token(args, r))
            .map_or(Some(None), |x| x.map(Some))?;
    let mut spans: Vec<(usize, usize)> = m.src.into_iter().chain(m.rt).collect();
    spans.sort_unstable();
    let mut norm = String::with_capacity(args.len());
    let mut at = 0;
    for (a, b) in spans {
        norm.push_str(&args[at..a]);
        norm.push('0');
        at = b;
    }
    norm.push_str(&args[at..]);
    Some(norm)
}
