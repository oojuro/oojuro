//! Shared wire types and native persistence primitives. No database dependency.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
pub const DIMENSIONS: usize = 3072;
pub const MAX_FRAME: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub key: String,
    pub text: String,
    /// Event/valid time in UNIX seconds (synthetic fixtures may use small seconds).
    pub valid_at: i64,
    /// When evidence became available to the database, in UNIX seconds.
    pub recorded_at: i64,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}
impl Record {
    pub fn new(id: &str, key: &str, text: &str, valid_at: i64) -> Self {
        Self {
            id: id.into(),
            key: key.into(),
            text: text.into(),
            valid_at,
            recorded_at: valid_at,
            verified: true,
            embedding: None,
            metadata: BTreeMap::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.key.is_empty() {
            return Err("record id and key must be nonempty".into());
        }
        if let Some(v) = &self.embedding {
            if v.len() != DIMENSIONS || v.iter().any(|x| !x.is_finite()) {
                return Err("embedding must contain 3072 finite values".into());
            }
        }
        if serde_json::to_vec(self)?.len() > MAX_FRAME {
            return Err("record exceeds frame limit; split into records".into());
        }
        Ok(())
    }
    pub fn vector(&self) -> Vec<f32> {
        self.embedding
            .clone()
            .unwrap_or_else(|| lexical_embedding(&self.text))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub valid_at: i64,
    pub known_at: i64,
    pub k: usize,
    /// Sum of returned text bytes only; use context_packet for a serialized prompt budget.
    pub max_context_bytes: usize,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
}
impl Query {
    pub fn new(text: &str, at: i64) -> Self {
        Self {
            text: text.into(),
            valid_at: at,
            known_at: at,
            k: 5,
            max_context_bytes: 16_384,
            embedding: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.k > 10_000 {
            return Err("k exceeds limit".into());
        }
        if let Some(v) = &self.embedding {
            if v.len() != DIMENSIONS || v.iter().any(|x| !x.is_finite()) {
                return Err("query embedding must contain 3072 finite values".into());
            }
        }
        Ok(())
    }
    pub fn vector(&self) -> Vec<f32> {
        self.embedding
            .clone()
            .unwrap_or_else(|| lexical_embedding(&self.text))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hit {
    pub record: Record,
    pub score: f32,
    pub uncertainty_bits: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub engine: String,
    pub hits: Vec<Hit>,
    pub routing_entropy_bits: f64,
    pub notes: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceSnippet {
    pub id: String,
    pub key: String,
    pub text: String,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub verified: bool,
    /// Exact UTF-8 byte range in the original source; exclusive end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_range: Option<[usize; 2]>,
}
impl SearchResult {
    /// Budget the serialized evidence array; embeddings and private metadata never enter it.
    pub fn context_packet(&self, max_bytes: usize) -> Result<Vec<EvidenceSnippet>> {
        if max_bytes < 2 {
            return Err("serialized evidence budget must allow an empty array (2 bytes)".into());
        }
        let mut out = Vec::new();
        let mut bytes = 2usize;
        for hit in &self.hits {
            let r = &hit.record;
            let snippet = EvidenceSnippet {
                id: r.id.clone(),
                key: r.key.clone(),
                text: r.text.clone(),
                valid_at: r.valid_at,
                recorded_at: r.recorded_at,
                verified: r.verified,
                source_range: None,
            };
            let size = serde_json::to_vec(&snippet)?.len() + usize::from(!out.is_empty());
            if bytes.saturating_add(size) > max_bytes {
                continue;
            }
            bytes += size;
            out.push(snippet);
        }
        Ok(out)
    }
}
pub trait Engine {
    fn name(&self) -> &'static str;
    fn insert(&mut self, record: Record) -> Result<()>;
    fn search(&mut self, query: &Query) -> Result<SearchResult>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A deterministic lexical projection for fixtures; NOT a pretrained semantic model.
/// Supplied vectors permit a real encoder without changing the engine APIs.
pub fn lexical_embedding(text: &str) -> Vec<f32> {
    let mut v = vec![0.0; DIMENSIONS];
    for token in tokens(text) {
        let hash = blake3::hash(token.as_bytes());
        let b = hash.as_bytes();
        let i = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize % DIMENSIONS;
        v[i] += if b[4] & 1 == 0 { 1.0 } else { -1.0 };
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
pub fn entropy(weights: &[f64]) -> f64 {
    let sum: f64 = weights.iter().filter(|x| x.is_finite() && **x > 0.0).sum();
    if sum <= 0.0 {
        return 0.0;
    }
    weights
        .iter()
        .filter(|x| x.is_finite() && **x > 0.0)
        .map(|x| x / sum)
        // A positive weight can still underflow to p = 0 (scores spanning more
        // than ~1074 bits); its limit contribution is 0, not 0 * -inf = NaN.
        .filter(|p| *p > 0.0)
        .map(|p| -p * p.log2())
        .sum()
}
pub fn visible_latest(records: &[Record], query: &Query) -> Vec<usize> {
    let mut latest: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        if r.valid_at > query.valid_at || r.recorded_at > query.known_at {
            continue;
        }
        if latest.get(r.key.as_str()).is_none_or(|j| {
            let old = &records[*j];
            (r.valid_at, r.recorded_at, i) > (old.valid_at, old.recorded_at, *j)
        }) {
            latest.insert(&r.key, i);
        }
    }
    latest.into_values().collect()
}
pub fn pack(
    engine: &str,
    mut hits: Vec<Hit>,
    query: &Query,
    routing_entropy_bits: f64,
    notes: Vec<String>,
) -> SearchResult {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.record.id.cmp(&b.record.id))
    });
    let mut bytes = 0usize;
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for hit in hits {
        if selected.len() >= query.k {
            break;
        }
        if !seen.insert(hit.record.id.clone()) {
            continue;
        }
        let size = hit.record.text.len();
        if bytes.saturating_add(size) > query.max_context_bytes {
            continue;
        }
        bytes += size;
        selected.push(hit);
    }
    SearchResult {
        engine: engine.into(),
        hits: selected,
        routing_entropy_bits,
        notes,
    }
}
pub fn ensure_unique(records: &[Record], record: &Record) -> Result<()> {
    record.validate()?;
    if records.iter().any(|r| r.id == record.id) {
        return Err("duplicate immutable record id".into());
    }
    Ok(())
}

/// Exclusive, single-writer native append log. Engine-specific magic prevents mixing formats.
/// A partial final frame is truncated on recovery; checksum corruption is an error.
pub struct Journal {
    file: File,
    compressed: bool,
    poisoned: bool,
    #[cfg(test)]
    fail_after_bytes: Option<usize>,
}
impl Journal {
    pub fn open<T: DeserializeOwned>(
        path: &Path,
        magic: [u8; 8],
        compressed: bool,
    ) -> Result<(Self, Vec<T>)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock()?;
        if file.metadata()?.len() == 0 {
            file.write_all(&magic)?;
            file.sync_all()?;
        }
        let mut actual = [0u8; 8];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut actual)?;
        if actual != magic {
            return Err("engine log magic/version mismatch".into());
        }
        let mut entries = Vec::new();
        let mut valid = 8u64;
        loop {
            let mut header = [0u8; 13];
            let mut read = 0;
            while read < header.len() {
                let n = file.read(&mut header[read..])?;
                if n == 0 {
                    break;
                }
                read += n;
            }
            if read == 0 {
                break;
            }
            if read < header.len() {
                file.set_len(valid)?;
                file.sync_all()?;
                break;
            }
            let stored = u32::from_le_bytes(header[0..4].try_into()?) as usize;
            let raw = u32::from_le_bytes(header[4..8].try_into()?) as usize;
            let checksum = u32::from_le_bytes(header[8..12].try_into()?);
            if stored > MAX_FRAME || raw > MAX_FRAME || header[12] > 1 {
                return Err("invalid log frame".into());
            }
            let mut payload = vec![0u8; stored];
            if let Err(e) = file.read_exact(&mut payload) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    file.set_len(valid)?;
                    file.sync_all()?;
                    break;
                }
                return Err(e.into());
            }
            if crc32fast::hash(&payload) != checksum {
                return Err("log checksum mismatch".into());
            }
            let decoded = if header[12] == 1 {
                lz4_flex::block::decompress(&payload, raw)?
            } else {
                payload
            };
            if decoded.len() != raw {
                return Err("log length mismatch".into());
            }
            entries.push(serde_json::from_slice(&decoded)?);
            valid += 13 + stored as u64;
        }
        file.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                file,
                compressed,
                poisoned: false,
                #[cfg(test)]
                fail_after_bytes: None,
            },
            entries,
        ))
    }
    /// Copy committed bytes using the owned handle. Reopening a locked journal
    /// for reading is not portable (Windows locks are mandatory). Restore the
    /// append position even if the backup destination fails.
    pub fn copy_committed(&mut self, destination: &mut impl Write) -> Result<u64> {
        if self.poisoned {
            return Err("cannot snapshot a poisoned journal".into());
        }
        self.file.sync_all()?;
        let position = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(0))?;
        let copied = std::io::copy(&mut self.file, destination);
        if let Err(error) = self.file.seek(SeekFrom::Start(position)) {
            self.poisoned = true;
            return Err(error.into());
        }
        Ok(copied?)
    }
    pub fn append<T: Serialize>(&mut self, entry: &T) -> Result<()> {
        if self.poisoned {
            return Err("journal I/O previously failed; close and reopen for recovery".into());
        }
        let raw = serde_json::to_vec(entry)?;
        if raw.len() > MAX_FRAME {
            return Err("log frame too large".into());
        }
        let packed = if self.compressed {
            lz4_flex::block::compress(&raw)
        } else {
            Vec::new()
        };
        let use_compressed = self.compressed && packed.len() < raw.len();
        let payload = if use_compressed { &packed } else { &raw };
        let mut frame = Vec::with_capacity(13 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
        frame.push(u8::from(use_compressed));
        frame.extend_from_slice(payload);
        self.poisoned = true;
        #[cfg(test)]
        if let Some(bytes) = self.fail_after_bytes.take() {
            self.file.write_all(&frame[..bytes.min(frame.len())])?;
            return Err(std::io::Error::other("injected partial write").into());
        }
        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.poisoned = false;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryTrace {
    pub query_digest: String,
    pub valid_at: i64,
    pub known_at: i64,
    pub hit_ids: Vec<String>,
}
pub fn trace(query: &Query, hits: &[Hit]) -> QueryTrace {
    QueryTrace {
        query_digest: blake3::hash(query.text.as_bytes()).to_hex().to_string(),
        valid_at: query.valid_at,
        known_at: query.known_at,
        hit_ids: hits.iter().map(|h| h.record.id.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entropy_and_projection() {
        assert!((entropy(&[1.0, 1.0]) - 1.0).abs() < 1e-12);
        assert_eq!(entropy(&[]), 0.0);
        // Subnormal weights whose share underflows to zero stay finite.
        let spread = [1.0, 1.0, f64::from_bits(1), (-1060f64).exp2()];
        assert!(entropy(&spread).is_finite());
        assert!((entropy(&spread) - 1.0).abs() < 1e-12);
        let v = lexical_embedding("alpha beta");
        assert!((dot(&v, &v) - 1.0).abs() < 1e-5);
    }
    #[test]
    fn bitemporal_visibility() {
        let a = Record::new("a", "x", "old", 1);
        let mut b = Record::new("b", "x", "new", 2);
        b.recorded_at = 5;
        let rows = vec![a, b];
        assert_eq!(visible_latest(&rows, &Query::new("x", 3)), vec![0]);
        assert_eq!(visible_latest(&rows, &Query::new("x", 6)), vec![1]);
    }
    fn temporary(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("oojuro-core-{}-{nonce}-{name}", std::process::id()))
    }
    #[test]
    fn compressed_journal_roundtrip_and_exclusion() {
        let path = temporary("roundtrip");
        let (mut log, entries) = Journal::open::<Record>(&path, *b"OOCOR001", true).unwrap();
        assert!(entries.is_empty());
        let r = Record::new("a", "key", &"repeat ".repeat(1000), 10);
        log.append(&r).unwrap();
        assert!(Journal::open::<Record>(&path, *b"OOCOR001", true).is_err());
        drop(log);
        assert!(std::fs::metadata(&path).unwrap().len() < 1000);
        let (_log, restored) = Journal::open::<Record>(&path, *b"OOCOR001", true).unwrap();
        assert_eq!(restored[0].text, r.text);
    }
    #[test]
    fn partial_tail_recovers_but_checksum_corruption_fails() {
        let path = temporary("tail");
        let (mut log, _) = Journal::open::<Record>(&path, *b"OOCOR002", false).unwrap();
        log.append(&Record::new("a", "x", "evidence", 1)).unwrap();
        drop(log);
        let size = std::fs::metadata(&path).unwrap().len();
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[10, 0, 0]).unwrap();
        }
        let (log, entries) = Journal::open::<Record>(&path, *b"OOCOR002", false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        drop(log);
        {
            let mut f = OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(21)).unwrap();
            f.write_all(&[255]).unwrap();
        }
        assert!(Journal::open::<Record>(&path, *b"OOCOR002", false).is_err());
    }
    #[test]
    fn invalid_vectors_and_duplicate_ids_rejected() {
        let mut r = Record::new("a", "x", "text", 1);
        r.embedding = Some(vec![0.0; 1]);
        assert!(r.validate().is_err());
        r.embedding = Some(vec![f32::NAN; DIMENSIONS]);
        assert!(r.validate().is_err());
        r.embedding = None;
        assert!(ensure_unique(&[r.clone()], &r).is_err());
    }
    #[test]
    fn partial_write_poison_prevents_later_acknowledgments() {
        let path = temporary("poison");
        let (mut log, _) = Journal::open::<Record>(&path, *b"OOCOR003", false).unwrap();
        log.append(&Record::new("a", "x", "committed", 1)).unwrap();
        log.fail_after_bytes = Some(20);
        assert!(log.append(&Record::new("b", "x", "partial", 2)).is_err());
        assert!(
            log.append(&Record::new("c", "x", "must not acknowledge", 3))
                .is_err()
        );
        drop(log);
        let (mut log, rows) = Journal::open::<Record>(&path, *b"OOCOR003", false).unwrap();
        assert_eq!(rows.len(), 1);
        log.append(&Record::new("c", "x", "committed after recovery", 3))
            .unwrap();
        drop(log);
        assert_eq!(
            Journal::open::<Record>(&path, *b"OOCOR003", false)
                .unwrap()
                .1
                .len(),
            2
        );
    }
    #[test]
    fn snapshot_destination_failure_preserves_append_position_and_contents() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected backup destination failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let path = temporary("snapshot");
        let (mut journal, _) = Journal::open::<Record>(&path, *b"OOSNP001", true).unwrap();
        journal.append(&Record::new("a", "a", "before", 1)).unwrap();
        assert!(journal.copy_committed(&mut Broken).is_err());
        journal
            .append(&Record::new("b", "b", "after failed copy", 2))
            .unwrap();
        let mut snapshot = Vec::new();
        let size = journal.copy_committed(&mut snapshot).unwrap();
        assert_eq!(size, snapshot.len() as u64);
        journal
            .append(&Record::new("c", "c", "after successful copy", 3))
            .unwrap();
        drop(journal);
        let (_, rows) = Journal::open::<Record>(&path, *b"OOSNP001", true).unwrap();
        assert_eq!(rows.len(), 3);
        let snapshot_path = temporary("snapshot-copy");
        std::fs::write(&snapshot_path, snapshot).unwrap();
        let (_, rows) = Journal::open::<Record>(&snapshot_path, *b"OOSNP001", true).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].text, "after failed copy");
    }
    #[test]
    fn serialized_context_excludes_embeddings_and_respects_wire_budget() {
        let mut r = Record::new("r", "k", "quoted \" text", 1);
        r.embedding = Some(vec![1.0; DIMENSIONS]);
        r.metadata.insert("private".into(), "excluded".into());
        let result = pack(
            "test",
            vec![Hit {
                record: r,
                score: 1.0,
                uncertainty_bits: None,
            }],
            &Query::new("x", 1),
            0.0,
            vec![],
        );
        let packet = result.context_packet(256).unwrap();
        let wire = serde_json::to_vec(&packet).unwrap();
        assert!(wire.len() <= 256);
        assert_eq!(packet.len(), 1);
        let text = String::from_utf8(wire).unwrap();
        assert!(!text.contains("embedding"));
        assert!(!text.contains("private"));
        assert!(result.context_packet(2).unwrap().is_empty());
        assert!(result.context_packet(1).is_err());
    }
}
