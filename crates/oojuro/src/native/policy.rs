use super::{Error, Result};
use oojuro::exclude::{Exclude, tool_call_paths};
use oojuro_capture::{
    Rec,
    policy::{Decision, Policy},
    kind,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    record: bool,
    paused: bool,
    record_thinking: bool,
    index_thinking: bool,
    exclude: Vec<String>,
    denied_tools: Vec<String>,
}
pub(super) struct FrozenPolicy {
    snapshot: Snapshot,
    exclude: Exclude,
    pub id: String,
}
impl FrozenPolicy {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| Error::Policy)?;
        if snapshot.version != 1
            || snapshot.exclude.len() > 64
            || snapshot.denied_tools.len() > 64
            || snapshot
                .exclude
                .iter()
                .chain(&snapshot.denied_tools)
                .any(|s| {
                    s.is_empty()
                        || s.len() > 1024
                        || s.chars().any(char::is_control)
                        || s.starts_with("~/")
                })
        {
            return Err(Error::Policy);
        }
        let exclude = Exclude::new(&snapshot.exclude, None).map_err(|_| Error::Policy)?;
        Ok(Self {
            snapshot,
            exclude,
            id: format!("native-policy-1:{}", blake3::hash(bytes).to_hex()),
        })
    }
    pub fn enabled(&self) -> bool {
        self.snapshot.record && !self.snapshot.paused
    }
}
impl Policy for FrozenPolicy {
    fn decide(&mut self, rec: &Rec, source: &str) -> Decision {
        if self.snapshot.paused {
            return Decision::Withhold("paused");
        }
        if !self.snapshot.record {
            return Decision::Withhold("record_off");
        }
        if rec.kind == kind::THINKING && !self.snapshot.record_thinking {
            return Decision::Withhold("thinking_off");
        }
        if self.exclude.matches(source, None)
            || (rec.kind == kind::TOOL_CALL
                && (self.snapshot.denied_tools.contains(&rec.tool)
                    || tool_call_paths(&rec.text)
                        .iter()
                        .any(|path| self.exclude.matches(path, Some(&rec.cwd)))))
        {
            return Decision::Withhold("excluded");
        }
        Decision::Store {
            indexed: kind::indexed(rec.kind)
                && !(rec.kind == kind::THINKING && !self.snapshot.index_thinking)
                && !rec.from_oojuro(),
        }
    }
}
pub(super) struct ReadOnly;
impl Policy for ReadOnly {
    fn decide(&mut self, _: &Rec, _: &str) -> Decision {
        Decision::Withhold("read_only")
    }
}
