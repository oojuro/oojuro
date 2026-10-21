//! The starter dictionary: a zstd dictionary shipped with the code for new
//! and small stores, trained only on public agent traces under permissive
//! licenses (see `dict/README.md`). Run files that use it name it by its
//! BLAKE3 id (`run::SharedDict`) and do not store it; a new version gets a
//! new file and id, and every shipped version stays readable.
use crate::run::SharedDict;
use std::sync::OnceLock;

/// starter-v1: 112 KiB, see `dict/README.md` for sources and licenses.
pub fn v1() -> &'static SharedDict {
    static D: OnceLock<SharedDict> = OnceLock::new();
    D.get_or_init(|| SharedDict::new(include_bytes!("../dict/starter-v1.zdict")))
}
