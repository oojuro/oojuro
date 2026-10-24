//! Policy shared by native capture adapters.
use crate::{Rec, kind};

/// What happens to one parsed record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Store it, indexed for search or stored only.
    Store { indexed: bool },
    /// Do not store it (the reason is counted in the stats).
    Withhold(&'static str),
}

impl Decision {
    /// The default: store, indexed by [`kind::indexed`] unless the record
    /// comes from Oojuro itself ([`Rec::from_oojuro`]).
    pub fn default_for(rec: &Rec) -> Self {
        Decision::Store {
            indexed: kind::indexed(rec.kind) && !rec.from_oojuro(),
        }
    }
}

/// Recording policy: settings applied to every parsed record before it is
/// stored. `refresh` runs at the start of every pass (re-read settings).
pub trait Policy: Send {
    fn refresh(&mut self) {}
    /// `source` is the transcript path the record was read from.
    fn decide(&mut self, rec: &Rec, source: &str) -> Decision;
}
