//! Exact bounded binary metadata for disposable layout 2 only. It contains no
//! source authority and does not reinterpret source frames or layout 1 JSON.
use super::*;

pub(super) fn encode(value: &Metadata, compression: Compression) -> Result<Vec<u8>> {
    fn string(bytes: &mut Vec<u8>, value: &str) -> Result<()> {
        let length = u32::try_from(value.len()).map_err(|_| Error::Bounds)?;
        if bytes
            .len()
            .checked_add(4 + value.len())
            .is_none_or(|n| n > MAX_META)
        {
            return Err(Error::Bounds);
        }
        bytes.extend(length.to_le_bytes());
        bytes.extend(value.as_bytes());
        Ok(())
    }
    fn option(bytes: &mut Vec<u8>, value: Option<&str>) -> Result<()> {
        bytes.push(u8::from(value.is_some()));
        if let Some(value) = value {
            string(bytes, value)?;
        }
        Ok(())
    }
    // Exhaustive destructuring makes adding a metadata field require updating
    // this codec, rather than silently dropping a source comparison field.
    let Metadata {
        id,
        key,
        valid_at,
        reported_recorded_at,
        kind: row_kind,
        tool,
        status: row_status,
        agent_id,
        indexed,
        text,
        text_digest,
        length,
        scope,
    } = value;
    let mut bytes = vec![2];
    string(&mut bytes, id)?;
    option(&mut bytes, key.as_deref())?;
    bytes.extend(valid_at.to_le_bytes());
    bytes.extend(reported_recorded_at.to_le_bytes());
    bytes.extend([kind(*row_kind), status(*row_status), u8::from(*indexed)]);
    option(&mut bytes, tool.as_deref())?;
    string(&mut bytes, agent_id)?;
    let Blob {
        file,
        offset,
        stored,
        bytes: text_bytes,
        kind: text_kind,
        codec,
        digest,
    } = text;
    bytes.extend(file.to_le_bytes());
    bytes.extend(offset.to_le_bytes());
    bytes.extend(stored.to_le_bytes());
    bytes.extend(text_bytes.to_le_bytes());
    bytes.extend([*text_kind, *codec]);
    bytes.extend(digest);
    bytes.extend(text_digest);
    bytes.extend(length.to_le_bytes());
    bytes.extend(scope.to_le_bytes());
    if bytes.len() > MAX_META {
        return Err(Error::Bounds);
    }
    pack_bytes(&bytes, compression)
}
struct Input<'a> {
    bytes: &'a [u8],
}
impl<'a> Input<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (value, remaining) = self.bytes.split_at_checked(N).ok_or(Error::Authority)?;
        self.bytes = remaining;
        Ok(value.try_into().unwrap())
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn boolean(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Authority),
        }
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn string(&mut self) -> Result<String> {
        let length = self.u32()? as usize;
        let (value, remaining) = self
            .bytes
            .split_at_checked(length)
            .ok_or(Error::Authority)?;
        let value = std::str::from_utf8(value).map_err(|_| Error::Authority)?;
        self.bytes = remaining;
        Ok(value.to_owned())
    }
    fn option(&mut self) -> Result<Option<String>> {
        if self.boolean()? {
            Ok(Some(self.string()?))
        } else {
            Ok(None)
        }
    }
}
pub(super) fn decode(bytes: &[u8]) -> Result<Metadata> {
    let bytes = unpack_bytes(bytes, MAX_META)?;
    let mut input = Input { bytes: &bytes };
    if input.byte()? != 2 {
        return Err(Error::Authority);
    }
    let id = input.string()?;
    let key = input.option()?;
    if !name(&id, 1024) || key.as_ref().is_some_and(|key| !name(key, 1024)) {
        return Err(Error::Authority);
    }
    let valid_at = i64::from_le_bytes(input.take()?);
    let reported_recorded_at = i64::from_le_bytes(input.take()?);
    let kind = match input.byte()? {
        0 => Kind::Input,
        1 => Kind::Output,
        2 => Kind::Thinking,
        3 => Kind::ToolCall,
        4 => Kind::ToolResult,
        5 => Kind::System,
        6 => Kind::Explicit,
        7 => Kind::Raw,
        _ => return Err(Error::Authority),
    };
    let status = match input.byte()? {
        0 => None,
        1 => Some(Status::Ok),
        2 => Some(Status::Error),
        _ => return Err(Error::Authority),
    };
    let indexed = input.boolean()?;
    let tool = input.option()?;
    let agent_id = input.string()?;
    let file = input.u32()?;
    let offset = u64::from_le_bytes(input.take()?);
    let stored = input.u32()?;
    let text_bytes = input.u32()?;
    let text_kind = input.byte()?;
    let codec = input.byte()?;
    let digest = input.take()?;
    let text_digest = input.take()?;
    let length = input.u32()?;
    let scope = input.u32()?;
    if !input.bytes.is_empty()
        || text_kind != TEXT
        || codec > 1
        || text_bytes as usize > MAX_TEXT_BYTES
        || (stored as usize) < SEAL_OVERHEAD
    {
        return Err(Error::Authority);
    }
    Ok(Metadata {
        id,
        key,
        valid_at,
        reported_recorded_at,
        kind,
        tool,
        status,
        agent_id,
        indexed,
        text: Blob {
            file,
            offset,
            stored,
            bytes: text_bytes,
            kind: text_kind,
            codec,
            digest,
        },
        text_digest,
        length,
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_metadata_preserves_every_field_and_refuses_truncation_and_trailing_bytes() {
        for compression in [Compression::Raw, Compression::Zstd] {
            for row_kind in [
                Kind::Input,
                Kind::Output,
                Kind::Thinking,
                Kind::ToolCall,
                Kind::ToolResult,
                Kind::System,
                Kind::Explicit,
                Kind::Raw,
            ] {
                for row_status in [None, Some(Status::Ok), Some(Status::Error)] {
                    let text = Blob {
                        file: 17,
                        offset: u64::MAX,
                        stored: 1024,
                        bytes: 700,
                        kind: TEXT,
                        codec: 1,
                        digest: [79; 32],
                    };
                    let value = Metadata {
                        id: "id/日本語".into(),
                        key: Some("k/é".into()),
                        valid_at: i64::MIN,
                        reported_recorded_at: i64::MAX,
                        kind: row_kind,
                        tool: Some("\0exact\r\n".into()),
                        status: row_status,
                        agent_id: "".into(),
                        indexed: true,
                        text,
                        text_digest: [171; 32],
                        length: u32::MAX,
                        scope: 17,
                    };
                    let packed = encode(&value, compression).unwrap();
                    let decoded = decode(&packed).unwrap();
                    assert_eq!(
                        serde_json::to_value(&decoded).unwrap(),
                        serde_json::to_value(&value).unwrap()
                    );
                    let raw = unpack_bytes(&packed, MAX_META).unwrap();
                    for length in 0..raw.len() {
                        assert!(decode(&pack_bytes(&raw[..length], compression).unwrap()).is_err());
                    }
                    let mut extra = raw;
                    extra.push(0);
                    assert!(decode(&pack_bytes(&extra, compression).unwrap()).is_err());
                }
            }
        }
    }
}
