//! `Content-Encoding` of a captured body, undone for the journal only (the
//! agent gets the bytes as sent). A body cut off mid-stream decodes as far
//! as it goes.
use std::io::Read;

/// Decode `body` (encodings applied in the listed order are undone in
/// reverse), at most `max` bytes out. `Err` for an unknown encoding or
/// data that does not decode at all.
pub fn decode(body: Vec<u8>, encoding: Option<&str>, max: usize) -> Result<Vec<u8>, String> {
    let Some(enc) = encoding else { return Ok(body) };
    let mut data = body;
    let codings: Vec<String> = enc.split(',').map(|x| x.trim().to_ascii_lowercase()).collect();
    for c in codings.iter().rev() {
        data = match c.as_str() {
            "" | "identity" => data,
            "gzip" | "x-gzip" => read_lossy(flate2::read::MultiGzDecoder::new(&data[..]), max)?,
            "deflate" => match read_lossy(flate2::read::ZlibDecoder::new(&data[..]), max) {
                Ok(v) if !v.is_empty() => v,
                _ => read_lossy(flate2::read::DeflateDecoder::new(&data[..]), max)?,
            },
            "br" => read_lossy(brotli_decompressor::Decompressor::new(&data[..], 16 << 10), max)?,
            "zstd" => read_lossy(zstd::stream::read::Decoder::new(&data[..]).map_err(|e| e.to_string())?, max)?,
            other => return Err(format!("unknown content-encoding {other:?}")),
        };
    }
    Ok(data)
}

/// Everything the reader yields before its end, an error, or `max` bytes.
fn read_lossy(mut r: impl Read, max: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > max {
                    return Err(format!("decoded body over {max} bytes"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                if out.is_empty() {
                    return Err(e.to_string());
                }
                break; // cut off: keep what decoded
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn gzip_whole_and_cut() {
        let text = "data: hello\n\n".repeat(2000);
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(text.as_bytes()).unwrap();
        let z = e.finish().unwrap();
        assert_eq!(decode(z.clone(), Some("gzip"), 1 << 20).unwrap(), text.as_bytes());
        let cut = decode(z[..z.len() / 2].to_vec(), Some("gzip"), 1 << 20).unwrap();
        assert!(!cut.is_empty() && text.as_bytes().starts_with(&cut));
        assert!(decode(z, Some("gzip"), 100).is_err());
        assert!(decode(b"x".to_vec(), Some("snappy"), 100).is_err());
        assert_eq!(decode(b"x".to_vec(), None, 100).unwrap(), b"x");
    }
}
