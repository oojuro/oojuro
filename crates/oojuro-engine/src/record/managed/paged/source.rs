//! Held current-control capability for real committed DATA reads.
use super::*;
use oojuro_store::managed_pack::{Catalog, DictionaryRef, Locator, PackRef};
pub(super) struct Paths {
    roots: [PathBuf; 11],
}
impl Paths {
    pub fn new(control: &Path, data: &Path) -> Self {
        let names = [
            schema::RootName::Checkpoint,
            schema::RootName::Prefix,
            schema::RootName::Request,
            schema::RootName::Erased,
            schema::RootName::Record,
            schema::RootName::KeyVersion,
            schema::RootName::Segment,
            schema::RootName::Frame,
            schema::RootName::File,
            schema::RootName::Dictionary,
            schema::RootName::PruneFiles,
        ];
        Self {
            roots: names.map(|n| {
                if n.class() == tree::KeyClass::Data {
                    data.join("trees").join(n.name())
                } else {
                    control.join("paged-trees").join(n.name())
                }
            }),
        }
    }
    pub fn get(&self, name: schema::RootName) -> &Path {
        &self.roots[name as usize]
    }
    pub fn create_data(&self) -> Result<()> {
        for p in &self.roots[4..10] {
            directory_create(p)?;
        }
        Ok(())
    }
    pub fn ctx<'a>(
        &'a self,
        name: schema::RootName,
        cipher: &'a FrameCipher,
        authority: &'a dyn tree::Authority,
        identity: [u8; 32],
    ) -> tree::Context<'a> {
        tree::Context {
            directory: self.get(name),
            cipher,
            authority,
            purpose: name.purpose(),
            identity: schema::root_identity(identity, name),
            class: name.class(),
            sort_of: None,
            cache: None,
        }
    }
    pub fn physical<'a>(
        &'a self,
        cipher: &'a FrameCipher,
        authority: &'a dyn tree::Authority,
        identity: [u8; 32],
    ) -> physical::Contexts<'a> {
        physical::Contexts {
            frame: self.ctx(schema::RootName::Frame, cipher, authority, identity),
            file: self.ctx(schema::RootName::File, cipher, authority, identity),
            dictionary: self.ctx(schema::RootName::Dictionary, cipher, authority, identity),
        }
    }
}
pub(super) struct CachedBirth {
    pin: RetainedFloor,
    class: tree::KeyClass,
    birth: Birth,
}
pub(super) type BirthCache =
    std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<CachedBirth>>>;
pub(super) struct Authority<'a> {
    pub owner: &'a control::Inner,
    pub control_directory: &'a Path,
    pub data_directory: &'a Path,
    pub enrollment_digest: [u8; 32],
    pub root: &'a schema::CommitRoot,
    pub class: tree::KeyClass,
    pub anchor: RetainedFloor,
    pub cache: BirthCache,
}
impl Authority<'_> {
    fn check(&self) -> Result<()> {
        let verified = self.owner.paged_verified()?;
        verified.assert_current()?;
        if verified
            .checkpoint()
            .floor()
            .map_err(|_| Error::Authority)?
            != self.anchor
        {
            return Err(Error::Authority);
        }
        if self.owner.state.status != Status::Active
            || *blake3::hash(&read_regular(
                &self.data_directory.join("ENROLLMENT"),
                8192,
            )?)
            .as_bytes()
                != self.enrollment_digest
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
}
impl tree::Authority for Authority<'_> {
    fn current(&self) -> Result<Vec<u8>> {
        self.check()?;
        let mut b = Birth::decode(&self.root.birth).map_err(io)?;
        if self.class == tree::KeyClass::Data {
            b.key_version = self.root.data_key_version.ok_or(Error::Authority)?;
        }
        b.encode().map_err(io)
    }
    fn validate_birth(
        &self,
        raw: &[u8],
        purpose: tree::Purpose,
        class: tree::KeyClass,
    ) -> Result<()> {
        self.check()?;
        if class != self.class {
            return Err(Error::Scope);
        }
        if class == tree::KeyClass::RetainedControl
            && !matches!(purpose, tree::Purpose::Prefix | tree::Purpose::Request)
        {
            return Err(Error::Scope);
        }
        let birth = Birth::decode(raw).map_err(io)?;
        {
            let mut cache = self.cache.borrow_mut();
            if let Some(i) = cache
                .iter()
                .position(|e| e.pin == self.anchor && e.class == class && e.birth == birth)
            {
                let b = cache.remove(i).ok_or(Error::Authority)?;
                cache.push_back(b);
                return Ok(());
            }
        }
        checkpoint::validate_ancestor(
            self.control_directory,
            self.owner.control_cipher(),
            self.owner.paged_verified()?,
            &birth,
            purpose,
            class,
        )?;
        let mut cache = self.cache.borrow_mut();
        cache.push_back(CachedBirth {
            pin: self.anchor,
            class,
            birth,
        });
        if cache.len() > 16 {
            cache.pop_front();
        }
        Ok(())
    }
}
pub(super) struct Source<'a> {
    pub paths: Paths,
    pub root: &'a schema::CommitRoot,
    pub cipher: Arc<FrameCipher>,
    pub data: Authority<'a>,
    pub control: Authority<'a>,
}
impl Source<'_> {
    pub fn ctx(&self, n: schema::RootName) -> tree::Context<'_> {
        if n.class() == tree::KeyClass::Data {
            self.paths
                .ctx(n, &self.cipher, &self.data, self.data.owner.state.identity)
        } else {
            self.paths.ctx(
                n,
                self.data.owner.control_cipher(),
                &self.control,
                self.data.owner.state.identity,
            )
        }
    }
    pub fn physical(&self) -> physical::Contexts<'_> {
        self.paths
            .physical(&self.cipher, &self.data, self.data.owner.state.identity)
    }
    pub fn lookup(
        &self,
        n: schema::RootName,
        root: &tree::Root,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        tree::get_ancestor(self.ctx(n), root, key)
    }
}
impl Catalog for &Source<'_> {
    fn current(&self) -> oojuro_store::Result<Vec<u8>> {
        tree::Authority::current(&self.data)
            .map_err(|_| oojuro_store::Error::Corrupt("managed current authority unavailable"))
    }
    fn validate_birth(&self, b: &Birth) -> oojuro_store::Result<()> {
        tree::Authority::validate_birth(
            &self.data,
            &b.encode()?,
            tree::Purpose::Frame,
            tree::KeyClass::Data,
        )
        .map_err(|_| oojuro_store::Error::Corrupt("managed pack birth unavailable"))
    }
    fn locate(&self, seq: u64) -> oojuro_store::Result<Locator> {
        self.root
            .physical
            .as_ref()
            .ok_or(oojuro_store::Error::Corrupt(
                "missing managed physical root",
            ))?
            .locate(&self.physical(), seq)
            .map_err(|_| oojuro_store::Error::Corrupt("invalid managed locator"))
    }
    fn pack(&self, id: u64) -> oojuro_store::Result<PackRef> {
        self.root
            .physical
            .as_ref()
            .ok_or(oojuro_store::Error::Corrupt(
                "missing managed physical root",
            ))?
            .pack(&self.physical(), id)
            .map_err(|_| oojuro_store::Error::Corrupt("invalid managed pack"))
    }
    fn dictionary(&self, id: u32) -> oojuro_store::Result<Option<DictionaryRef>> {
        self.root
            .physical
            .as_ref()
            .ok_or(oojuro_store::Error::Corrupt(
                "missing managed physical root",
            ))?
            .dictionary(&self.physical(), id)
            .map_err(|_| oojuro_store::Error::Corrupt("invalid managed dictionary"))
    }
    fn cipher(&self, key: &[u8; 32]) -> oojuro_store::Result<Arc<FrameCipher>> {
        self.data
            .check()
            .map_err(|_| oojuro_store::Error::Corrupt("managed key unavailable"))?;
        if self.root.data_key_version.as_ref() != Some(key) {
            return Err(oojuro_store::Error::Corrupt("managed key version mismatch"));
        }
        Ok(self.cipher.clone())
    }
}
