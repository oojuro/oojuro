//! Private index bootstrap rooted in a verified externally retained CURRENT envelope.
use super::*;
use schema::{RootName, root_identity};
/// This constructor is private and receives the non-constructible Verified capability.
struct Index<'a> {
    verified: &'a control::tail::Verified,
    birth: Birth,
}
impl tree::Authority for Index<'_> {
    fn current(&self) -> Result<Vec<u8>> {
        self.verified.assert_current()?;
        self.birth.encode().map_err(io)
    }
    fn validate_birth(
        &self,
        raw: &[u8],
        purpose: tree::Purpose,
        class: tree::KeyClass,
    ) -> Result<()> {
        self.verified.assert_current()?;
        if purpose != tree::Purpose::Prefix || class != tree::KeyClass::RetainedControl {
            return Err(Error::Scope);
        }
        let b = Birth::decode(raw).map_err(io)?;
        if b.stable_origin != self.birth.stable_origin
            || b.identity != self.birth.identity
            || b.key_version != self.birth.key_version
            || b.generation > self.birth.generation
            || (b.generation == self.birth.generation && b != self.birth)
            || (b.generation == 0 && b.parent_checkpoint != [0; 32])
            || (b.generation != 0 && b.parent_checkpoint == [0; 32])
        {
            return Err(Error::Authority);
        }
        // Parent-child reference digest+AEAD reachability is anchored to the exact
        // already-verified CURRENT root pointer. This one index does not prove itself
        // by recursively looking itself up. Other tree purposes use its exact entries.
        if self.verified.checkpoint().body.scope != b.identity {
            return Err(Error::Scope);
        }
        Ok(())
    }
}
pub(in crate::record::managed) fn digest(
    directory: &Path,
    cipher: &FrameCipher,
    verified: &control::tail::Verified,
    sequence: u64,
) -> Result<[u8; 32]> {
    verified.assert_current()?;
    if sequence == 0 || sequence >= verified.checkpoint().body.sequence {
        return Err(Error::Bounds);
    }
    let root = schema::load(directory, cipher, verified.state())?;
    let birth = Birth::decode(&root.birth).map_err(io)?;
    let authority = Index { verified, birth };
    let tree_dir = directory
        .join("paged-trees")
        .join(RootName::Checkpoint.name());
    let ctx = tree::Context {
        directory: &tree_dir,
        cipher,
        authority: &authority,
        purpose: tree::Purpose::Prefix,
        identity: root_identity(verified.state().identity, RootName::Checkpoint),
        class: tree::KeyClass::RetainedControl,
        sort_of: None,
        cache: None,
    };
    let bytes = tree::get_ancestor(ctx, &root.control.checkpoint, &sequence.to_be_bytes())?
        .ok_or(Error::Authority)?;
    bytes.try_into().map_err(|_| Error::Authority)
}

struct Fresh<'a> {
    birth: Birth,
    witness: &'a Path,
}
impl tree::Authority for Fresh<'_> {
    fn current(&self) -> Result<Vec<u8>> {
        match fs::symlink_metadata(self.witness.join("CURRENT")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Ok(_) => return Err(Error::Authority),
            Err(_) => return Err(Error::Unavailable),
        }
        self.birth.encode().map_err(io)
    }
    fn validate_birth(&self, raw: &[u8], _: tree::Purpose, class: tree::KeyClass) -> Result<()> {
        if class != tree::KeyClass::RetainedControl || raw != self.current()? {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
pub(in crate::record::managed) fn genesis(
    directory: &Path,
    cipher: &FrameCipher,
    witness: &Path,
    origin: [u8; 32],
    identity: [u8; 32],
) -> Result<schema::CommitRoot> {
    let birth = Birth {
        format: 1,
        stable_origin: origin,
        identity,
        epoch: oojuro_store::crypto::random_key(),
        generation: 0,
        predecessor_logical: [0; 32],
        parent_checkpoint: [0; 32],
        key_version: oojuro_store::crypto::random_key(),
    };
    let authority = Fresh {
        birth: birth.clone(),
        witness,
    };
    directory_create(&directory.join("paged-trees"))?;
    let build = |name: RootName| {
        let path = directory.join("paged-trees").join(name.name());
        directory_create(&path)?;
        tree::Builder::new(tree::Context {
            directory: &path,
            cipher,
            authority: &authority,
            purpose: name.purpose(),
            identity: root_identity(identity, name),
            class: name.class(),
            sort_of: None,
            cache: None,
        })?
        .finish()
    };
    Ok(schema::CommitRoot {
        format: 1,
        intent: oojuro_store::crypto::random_key(),
        birth: birth.encode().map_err(io)?,
        data_key_version: None,
        next_id: 0,
        last_at: None,
        append_sequence: 0,
        next_file: 1,
        next_physical: 0,
        logical: [0; 32],
        original_records: 0,
        original_bytes: 0,
        live_records: 0,
        live_bytes: 0,
        physical: None,
        data: None,
        control: schema::ControlRoots {
            checkpoint: build(RootName::Checkpoint)?,
            prefix: build(RootName::Prefix)?,
            request: build(RootName::Request)?,
            erased: build(RootName::Erased)?,
        },
        prune: None,
    })
}
/// Candidate writes are private and fenced to the current externally retained predecessor.
pub(super) struct Stage<'a> {
    pub verified: &'a control::tail::Verified,
    pub birth: Birth,
    pub directory: &'a Path,
    pub cipher: &'a FrameCipher,
    pub class: tree::KeyClass,
}
impl tree::Authority for Stage<'_> {
    fn current(&self) -> Result<Vec<u8>> {
        self.verified.assert_current()?;
        self.birth.encode().map_err(io)
    }
    fn validate_birth(
        &self,
        raw: &[u8],
        purpose: tree::Purpose,
        class: tree::KeyClass,
    ) -> Result<()> {
        self.verified.assert_current()?;
        let b = Birth::decode(raw).map_err(io)?;
        if class != self.class {
            return Err(Error::Scope);
        }
        if b == self.birth {
            return Ok(());
        }
        validate_ancestor(
            self.directory,
            self.cipher,
            self.verified,
            &b,
            purpose,
            class,
        )
    }
}
pub(super) fn validate_ancestor(
    directory: &Path,
    cipher: &FrameCipher,
    verified: &control::tail::Verified,
    birth: &Birth,
    purpose: tree::Purpose,
    class: tree::KeyClass,
) -> Result<()> {
    verified.assert_current()?;
    let root = schema::load(directory, cipher, verified.state())?;
    let current = Birth::decode(&root.birth).map_err(io)?;
    let expected_key = match class {
        tree::KeyClass::RetainedControl => {
            if !matches!(purpose, tree::Purpose::Prefix | tree::Purpose::Request) {
                return Err(Error::Scope);
            }
            current.key_version
        }
        tree::KeyClass::Data => root.data_key_version.ok_or(Error::Scope)?,
    };
    if birth.stable_origin != current.stable_origin
        || birth.identity != current.identity
        || birth.key_version != expected_key
        || birth.generation > current.generation
    {
        return Err(Error::Authority);
    }
    if birth.generation == current.generation {
        // DATA and CONTROL share authority epoch; key class selects its separate key version.
        let mut same = current.clone();
        same.key_version = expected_key;
        if *birth != same {
            return Err(Error::Authority);
        }
        return Ok(());
    }
    // Exact published generation, including its unique epoch, must be authenticated.
    // Merely proving the predecessor exists also admits abandoned sibling candidates.
    let sequence = birth.generation.checked_add(1).ok_or(Error::Bounds)?;
    let cp_digest = digest(directory, cipher, verified, sequence)?;
    let historical = verified.historical_state(directory, cipher, sequence, cp_digest)?;
    let old = schema::load(directory, cipher, &historical)?;
    let mut published = Birth::decode(&old.birth).map_err(io)?;
    if class == tree::KeyClass::Data {
        published.key_version = old.data_key_version.ok_or(Error::Authority)?;
    }
    if *birth != published {
        return Err(Error::Authority);
    }
    verified.assert_current()?;
    Ok(())
}
pub(in crate::record::managed) fn advance_root(
    directory: &Path,
    cipher: &FrameCipher,
    verified: &control::tail::Verified,
    state: &mut control::State,
) -> Result<()> {
    verified.assert_current()?;
    let mut root = schema::load(directory, cipher, verified.state())?;
    let mut birth = Birth::decode(&root.birth).map_err(io)?;
    birth.generation = verified
        .state()
        .generation
        .checked_add(1)
        .ok_or(Error::Bounds)?;
    birth.epoch = oojuro_store::crypto::random_key();
    birth.parent_checkpoint = verified
        .checkpoint()
        .digest()
        .map_err(|_| Error::Authority)?;
    birth.predecessor_logical = verified.state().logical;
    let stage = Stage {
        verified,
        birth: birth.clone(),
        directory,
        cipher,
        class: tree::KeyClass::RetainedControl,
    };
    let path = directory
        .join("paged-trees")
        .join(RootName::Checkpoint.name());
    let ctx = tree::Context {
        directory: &path,
        cipher,
        authority: &stage,
        purpose: tree::Purpose::Prefix,
        identity: root_identity(state.identity, RootName::Checkpoint),
        class: tree::KeyClass::RetainedControl,
        sort_of: None,
        cache: None,
    };
    let mut editor = tree::Editor::new(ctx, root.control.checkpoint)?;
    editor.insert(tree::Entry::new(
        verified.checkpoint().body.sequence.to_be_bytes().to_vec(),
        verified
            .checkpoint()
            .digest()
            .map_err(|_| Error::Authority)?
            .to_vec(),
    )?)?;
    root.control.checkpoint = editor.finish()?;
    root.birth = birth.encode().map_err(io)?;
    root.logical = state.logical;
    root.live_records = state.live_records;
    root.live_bytes = state.live_bytes;
    if state.status == control::Status::DeniedPendingPermanent {
        root.prune = None;
    }
    let anchor = root.stage(directory, cipher)?;
    state.generation = birth.generation;
    state.physical = anchor.ciphertext_digest;
    state.paged = Some(anchor);
    Ok(())
}
pub(in crate::record::managed) fn check_prepared(
    directory: &Path,
    cipher: &FrameCipher,
    verified: &control::tail::Verified,
    next: &control::State,
) -> Result<()> {
    verified.assert_current()?;
    let root = schema::load(directory, cipher, next)?;
    let birth = Birth::decode(&root.birth).map_err(io)?;
    if birth.generation
        != verified
            .state()
            .generation
            .checked_add(1)
            .ok_or(Error::Bounds)?
        || birth.parent_checkpoint
            != verified
                .checkpoint()
                .digest()
                .map_err(|_| Error::Authority)?
        || birth.predecessor_logical != verified.state().logical
        || next.origin != verified.state().origin
        || next.identity != verified.state().identity
    {
        return Err(Error::Authority);
    }
    // Stage authority admits exactly the prepared birth, while inherited pages remain
    // chained to the verified predecessor. Validate the checkpoint just inserted.
    let stage = Stage {
        verified,
        birth,
        directory,
        cipher,
        class: tree::KeyClass::RetainedControl,
    };
    let path = directory
        .join("paged-trees")
        .join(RootName::Checkpoint.name());
    let ctx = tree::Context {
        directory: &path,
        cipher,
        authority: &stage,
        purpose: tree::Purpose::Prefix,
        identity: root_identity(next.identity, RootName::Checkpoint),
        class: tree::KeyClass::RetainedControl,
        sort_of: None,
        cache: None,
    };
    let value = tree::get_ancestor(
        ctx,
        &root.control.checkpoint,
        &verified.checkpoint().body.sequence.to_be_bytes(),
    )?
    .ok_or(Error::Authority)?;
    if value
        != verified
            .checkpoint()
            .digest()
            .map_err(|_| Error::Authority)?
            .to_vec()
    {
        return Err(Error::Authority);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::managed::tests::{Fixture, identity};
    use tree::Authority;
    const DATA: [u8; 32] = [51; 32];
    const CONTROL: [u8; 32] = [52; 32];
    const AUTH: [u8; 32] = [53; 32];
    fn create(f: &Fixture) -> RetainedControl {
        RetainedControl::create_paged(
            &f.root.join("paged-control"),
            &CONTROL,
            f.signer(),
            AUTH,
            FileWitness::open(&f.root.join("paged-witness")).unwrap(),
            identity_binding(&DATA, &identity()).unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn exact_published_birth_refuses_abandoned_sibling_and_wrong_root_namespace() {
        let f = Fixture::new();
        let c = create(&f);
        let mut owner = c.lock().unwrap();
        let original = schema::load(&c.directory, owner.control_cipher(), &owner.state).unwrap();
        let original_birth = Birth::decode(&original.birth).unwrap();
        let first = owner.checkpoint.digest().unwrap();
        for _ in 0..3 {
            let next = owner.state.clone();
            owner.advance(next).unwrap();
        }
        let v = owner.paged_verified().unwrap();
        validate_ancestor(
            &c.directory,
            owner.control_cipher(),
            v,
            &original_birth,
            tree::Purpose::Request,
            tree::KeyClass::RetainedControl,
        )
        .unwrap();
        let mut sibling = original_birth.clone();
        sibling.epoch = [77; 32];
        assert!(
            validate_ancestor(
                &c.directory,
                owner.control_cipher(),
                v,
                &sibling,
                tree::Purpose::Prefix,
                tree::KeyClass::RetainedControl
            )
            .is_err()
        );
        let root = schema::load(&c.directory, owner.control_cipher(), &owner.state).unwrap();
        let authority = Index {
            verified: v,
            birth: Birth::decode(&root.birth).unwrap(),
        };
        let path = c
            .directory
            .join("paged-trees")
            .join(RootName::Checkpoint.name());
        let ctx = tree::Context {
            directory: &path,
            cipher: owner.control_cipher(),
            authority: &authority,
            purpose: tree::Purpose::Prefix,
            identity: root_identity(owner.state.identity, RootName::Prefix),
            class: tree::KeyClass::RetainedControl,
            sort_of: None,
            cache: None,
        };
        // Prefix shares the generic Purpose byte, but is an authentic DIFFERENT root namespace.
        assert!(tree::get_ancestor(ctx, &root.control.checkpoint, &1u64.to_be_bytes()).is_err());
        let mut same_generation = Birth::decode(&root.birth).unwrap();
        same_generation.epoch = [78; 32];
        assert!(
            authority
                .validate_birth(
                    &same_generation.encode().unwrap(),
                    tree::Purpose::Prefix,
                    tree::KeyClass::RetainedControl
                )
                .is_err()
        );
        // Bootstrap's ancestor allowance is safe only through its pinned CURRENT root.
        // It is not usable as a generic authorization; exact ancestor validation above refuses.
        assert_eq!(
            digest(&c.directory, owner.control_cipher(), v, 1).unwrap(),
            first
        );
    }
    #[test]
    fn direct_current_open_does_not_replay_genesis_and_missing_current_refuses() {
        let f = Fixture::new();
        let c = create(&f);
        let first = c.current_floor().unwrap();
        {
            let mut owner = c.lock().unwrap();
            let next = owner.state.clone();
            owner.advance(next).unwrap();
        }
        let current = c.current_floor().unwrap();
        drop(c);
        let first_path = f
            .root
            .join("paged-control/checkpoints")
            .join(format!("cp-{}.bin", schema::token_hex(&first.digest)));
        fs::remove_file(first_path).unwrap();
        let c = RetainedControl::open_paged(
            &f.root.join("paged-control"),
            &CONTROL,
            f.signer(),
            AUTH,
            FileWitness::open(&f.root.join("paged-witness")).unwrap(),
            identity_binding(&DATA, &identity()).unwrap(),
        )
        .unwrap();
        assert_eq!(c.current_floor().unwrap(), current);
        drop(c);
        let current_path = f
            .root
            .join("paged-control/checkpoints")
            .join(format!("cp-{}.bin", schema::token_hex(&current.digest)));
        fs::remove_file(current_path).unwrap();
        assert!(
            RetainedControl::open_paged(
                &f.root.join("paged-control"),
                &CONTROL,
                f.signer(),
                AUTH,
                FileWitness::open(&f.root.join("paged-witness")).unwrap(),
                identity_binding(&DATA, &identity()).unwrap()
            )
            .is_err()
        );
        assert!(
            RetainedControl::open(
                &f.root.join("paged-control"),
                &CONTROL,
                f.signer(),
                AUTH,
                FileWitness::open(&f.root.join("paged-witness")).unwrap(),
                identity_binding(&DATA, &identity()).unwrap()
            )
            .is_err()
        );
    }
}
