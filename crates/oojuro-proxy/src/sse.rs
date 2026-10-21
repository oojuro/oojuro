//! Server-sent events, as the journal worker reads a finished (or cut off)
//! response body: `event:` and `data:` fields, events split by a blank line,
//! comments and other fields ignored. A last event without its blank line
//! (a stream cut off) is returned too.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub event: String,
    pub data: String,
}

pub fn parse(body: &str) -> Vec<Event> {
    let mut out = Vec::new();
    let mut event = String::new();
    let mut data: Vec<&str> = Vec::new();
    let mut any = false;
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if any {
                out.push(Event { event: std::mem::take(&mut event), data: data.join("\n") });
                data.clear();
                any = false;
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.find(':') {
            Some(i) => (&line[..i], line[i + 1..].strip_prefix(' ').unwrap_or(&line[i + 1..])),
            None => (line, ""),
        };
        match field {
            "event" => {
                event = value.to_string();
                any = true;
            }
            "data" => {
                data.push(value);
                any = true;
            }
            _ => {}
        }
    }
    if any {
        out.push(Event { event, data: data.join("\n") });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_crlf_comments_and_cut_tail() {
        let b = ": ping\r\nevent: a\r\ndata: {\"x\":1}\r\n\r\ndata: one\ndata: two\n\nevent: b\ndata: cut";
        let e = parse(b);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0], Event { event: "a".into(), data: "{\"x\":1}".into() });
        assert_eq!(e[1].data, "one\ntwo");
        assert_eq!(e[2], Event { event: "b".into(), data: "cut".into() });
    }
}
