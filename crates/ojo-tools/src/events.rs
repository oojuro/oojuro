//! Generic Event commands; no capture or application schema.
use super::{Arguments, Result, checkpoint, hex, private_bytes, unhex, value_file, write_new};
use ojo::events::{AppendIntent, Cursor, Event, Journal, Page};
use serde_json::json;
use std::{
    ffi::OsStr,
    io::{self, Write},
    path::Path,
    time::{Duration, Instant},
};

const MAX_PAGE_BYTES: usize = 64 << 20;

fn cursor(value: &OsStr) -> Result<Cursor> {
    Ok(Cursor::from_bytes(
        unhex(value, 40)?
            .try_into()
            .map_err(|_| "cursor must contain exactly 40 bytes")?,
    )?)
}
fn number<T: std::str::FromStr>(value: &OsStr) -> Result<T> {
    value
        .to_str()
        .ok_or("numeric argument must be UTF-8")?
        .parse()
        .map_err(|_| "invalid numeric argument".into())
}
fn packet(
    journal: &mut Journal,
    args: &Arguments,
    after: Option<Cursor>,
    limit: usize,
) -> Result<(Vec<Event>, Page)> {
    let from = args.from.as_deref().map(number::<i64>).transpose()?;
    let to = args.to.as_deref().map(number::<i64>).transpose()?;
    let mut events = Vec::new();
    let mut bytes = 0usize;
    let mut last = after;
    let mut capped = false;
    let mut visit = |event: Event| {
        if event.data.len() > MAX_PAGE_BYTES - bytes {
            capped = true;
            return Err(ojo::Error::Limit);
        }
        bytes += event.data.len();
        last = Some(event.cursor);
        events.push(event);
        Ok(())
    };
    let result = if from.is_some() || to.is_some() {
        journal.scan_time(from.unwrap_or(i64::MIN), to, after, limit, &mut visit)
    } else {
        journal.scan(after, limit, &mut visit)
    };
    let page = match result {
        Ok(page) => page,
        Err(ojo::Error::Limit) if capped => Page {
            cursor: last,
            has_more: true,
        },
        Err(error) => return Err(error.into()),
    };
    Ok((events, page))
}

pub(super) fn run(command: &str, args: &Arguments, key: Option<&[u8; 32]>) -> Result<()> {
    let path = Path::new(&args.positional[0]);
    match command {
        "init" => {
            let journal = Journal::create(path, key)?;
            println!(
                "{}",
                json!({"created":path,"profile":1,"checkpoint":checkpoint(journal.checkpoint())})
            );
        }
        "append" => {
            let data = value_file(&args.positional[1])?;
            let retained = if let Some(path) = args
                .intent
                .as_deref()
                .filter(|p| p.exists() || p.is_symlink())
            {
                let intent =
                    AppendIntent::from_bytes(private_bytes(path, 80)?.try_into().unwrap())?;
                // Validate existing intent/data against a read-only snapshot before
                // a writable open is allowed to recover an incomplete tail.
                Journal::open(Path::new(&args.positional[0]), key, false)?
                    .check_intent(&intent, &data)?;
                Some(intent)
            } else {
                None
            };
            let mut journal = Journal::open(path, key, true)?;
            let receipt = if let Some(intent) = retained {
                journal.append_prepared(&intent, &data)?
            } else if let Some(path) = args.intent.as_deref() {
                let intent = journal.prepare(&data)?;
                write_new(path, &intent.to_bytes())?;
                journal.append_prepared(&intent, &data)?
            } else {
                journal.append(&data)?
            };
            println!(
                "{}",
                json!({"cursor":hex(&receipt.cursor.to_bytes()),"accepted_at":receipt.accepted_at,
                "checkpoint":checkpoint(journal.checkpoint()),"recovered_tail_bytes":journal.recovered_tail_bytes()})
            );
        }
        "read" => {
            let selected = cursor(&args.positional[1])?;
            let mut journal = Journal::open(path, key, false)?;
            if journal.recovered_tail_bytes() != 0 {
                return Err("incomplete trailing data; inspect then recover before read".into());
            }
            let event = journal.get(selected)?.ok_or("event not found")?;
            drop(journal);
            if let Some(output) = args.output.as_deref() {
                write_new(output, &event.data)?;
            } else {
                let mut output = io::stdout().lock();
                output.write_all(&event.data)?;
                output.flush()?;
            }
        }
        "scan" | "follow" => {
            let mut after = args.after.as_deref().map(cursor).transpose()?;
            let limit = args
                .limit
                .as_deref()
                .map(number::<usize>)
                .transpose()?
                .unwrap_or(100);
            if !(1..=1024).contains(&limit) {
                return Err("page limit must be 1..=1024".into());
            }
            let from = args.from.as_deref().map(number::<i64>).transpose()?;
            let to = args.to.as_deref().map(number::<i64>).transpose()?;
            if to.is_some_and(|end| end <= from.unwrap_or(i64::MIN)) {
                return Err("invalid timestamp range".into());
            }
            let wait = args
                .wait_ms
                .as_deref()
                .map(number::<u64>)
                .transpose()?
                .unwrap_or(1000);
            if wait > 60_000 {
                return Err("follow wait must be 0..=60000 ms".into());
            }
            if args.watch && wait < 25 {
                return Err("--watch requires --wait-ms of at least 25".into());
            }
            loop {
                let deadline = Instant::now() + Duration::from_millis(wait);
                let (events, page, observed, tail) = loop {
                    let opened = Journal::open(path, key, false);
                    let mut journal = match opened {
                        Ok(journal) => journal,
                        Err(ojo::Error::Locked)
                            if command == "follow" && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(25));
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let (events, page) = packet(&mut journal, args, after, limit)?;
                    let observed = journal.checkpoint();
                    let tail = journal.recovered_tail_bytes();
                    drop(journal); // Output and waits must not hold a shared inode lock.
                    if command == "scan" || !events.is_empty() || Instant::now() >= deadline {
                        break (events, page, observed, tail);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                };
                let mut output = io::stdout().lock();
                if args.text {
                    for event in &events {
                        let data = match std::str::from_utf8(&event.data) {
                            Ok(text) => serde_json::to_string(text)?,
                            Err(_) => format!("hex:{}", hex(&event.data)),
                        };
                        writeln!(output, "{} {}", utc(event.accepted_at), data)?;
                    }
                } else {
                    write!(output, "{{\"events\":[")?;
                    for (index, event) in events.iter().enumerate() {
                        if index > 0 {
                            write!(output, ",")?;
                        }
                        serde_json::to_writer(
                            &mut output,
                            &json!({"accepted_at":event.accepted_at,
                    "cursor":hex(&event.cursor.to_bytes()),"data_hex":hex(&event.data)}),
                        )?;
                    }
                    writeln!(
                        output,
                        "],\"page\":{}}}",
                        json!({"profile":1,"cursor":page.cursor.map(|c|hex(&c.to_bytes())),
                "has_more":page.has_more,"checkpoint":checkpoint(observed),"incomplete_tail_bytes":tail})
                    )?;
                }
                output.flush()?;
                if !args.watch {
                    break;
                }
                after = page.cursor;
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

// Gregorian UTC conversion using Euclidean days, including dates before 1970.
// No timezone database or application schema is involved.
fn utc(micros: i64) -> String {
    let seconds = micros.div_euclid(1_000_000);
    let fraction = micros.rem_euclid(1_000_000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:06}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc;
    #[test]
    fn utc_rendering_covers_epoch_negative_times_and_leap_centuries() {
        for (input, expected) in [
            (0, "1970-01-01T00:00:00.000000Z"),
            (-1, "1969-12-31T23:59:59.999999Z"),
            (951_782_400_123_456, "2000-02-29T00:00:00.123456Z"),
            (-2_203_891_200_000_000, "1900-03-01T00:00:00.000000Z"),
            (4_107_542_400_000_000, "2100-03-01T00:00:00.000000Z"),
        ] {
            assert_eq!(utc(input), expected);
        }
    }
}
