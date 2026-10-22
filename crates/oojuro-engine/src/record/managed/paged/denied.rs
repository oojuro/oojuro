//! DATA-key-free cleanup after independently retained logical denial.
use super::*;
use crate::audit::EventKind;
pub struct DeniedCleanup {
    pub payload_namespaces_removed: bool,
    pub permanent: PermanentErasure,
    pub signed_receipt: LifecycleReceipt,
}
impl RetainedControl {
    /// Only an already denied enrolled paged authority may retire its local DATA
    /// namespaces. This does not use/recover DATA keys and is resumable after crash.
    pub fn cleanup_paged_data(
        &self,
        directory: &Path,
        limits: prune::PruneLimits,
    ) -> Result<DeniedCleanup> {
        let m = fs::symlink_metadata(directory).map_err(io)?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(Error::Authority);
        }
        let directory = directory.canonicalize().map_err(io)?;
        if overlaps(&directory, &self.directory) || overlaps(&directory, &self.witness_directory) {
            return Err(Error::Scope);
        }
        let _lock = owner_lock(&directory)?;
        let mut owner = self.lock()?;
        if !owner.is_paged() || owner.state.status != Status::DeniedPendingPermanent {
            return Err(Error::Authority);
        }
        let root = schema::load(&self.directory, owner.control_cipher(), &owner.state)?;
        let expected = root.enrollment_digest.ok_or(Error::Authority)?;
        let enrollment = read_regular(&directory.join("ENROLLMENT"), 8192)?;
        if *blake3::hash(&enrollment).as_bytes() != expected
            || read_regular(&directory.join("MANAGED_FORMAT"), 128)?
                != b"oojuro-managed-record-paged-3\n"
        {
            return Err(Error::Authority);
        }
        let mut work = proof::Work::new(limits.bytes, limits.rows)?;
        let mut fence = || {
            owner.paged_verified()?.assert_current()?;
            if owner.state.status != Status::DeniedPendingPermanent
                || *blake3::hash(&read_regular(&directory.join("ENROLLMENT"), 8192)?).as_bytes()
                    != expected
                || read_regular(&directory.join("MANAGED_FORMAT"), 128)?
                    != b"oojuro-managed-record-paged-3\n"
            {
                return Err(Error::Authority);
            }
            Ok(())
        };
        let mut removed = 0u64;
        inventory::owned_namespaces(&directory, &mut |p| {
            work.charge_rows(inventory::NAMESPACE_WORK)?;
            fence()?;
            inventory::remove_namespace(p, &mut fence, &mut work)?;
            removed = removed.checked_add(1).ok_or(Error::Bounds)?;
            sync_dir(&directory)?;
            Ok(())
        })?;
        inventory::owned_namespaces(&directory, &mut |_| {
            work.charge_rows(inventory::NAMESPACE_WORK)?;
            Err(Error::Authority)
        })?;
        fence()?;
        if !owner.state.pruned || removed != 0 {
            let mut state = owner.state.clone();
            state.pruned = true;
            state.audit = Some(owner.event(
                EventKind::LogicalDenialPendingPermanent,
                state.live_records,
                state.live_bytes,
                state.physical,
                state.physical,
            )?);
            owner.advance(state)?;
        }
        owner.recheck()?;
        Ok(DeniedCleanup {
            payload_namespaces_removed: true,
            permanent: PermanentErasure::PendingUnsupportedLocalKeyAuthority,
            signed_receipt: owner.receipt()?,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::managed::paged::driver::tests::{control, log};
    use crate::record::managed::tests::{Fixture, identity, input};
    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let target = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_tree(&e.path(), &target);
            } else {
                fs::copy(e.path(), target).unwrap();
            }
        }
    }
    #[test]
    fn denied_cleanup_reopens_control_without_payload_key_and_fences_restored_old_backup() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c.clone(), true);
        l.append("one", &[input("private", "must remain logically denied")])
            .unwrap();
        let saved = f.root.join("old-data-backup");
        copy_tree(&f.root.join("paged-data"), &saved);
        let expected_scope = c.status().unwrap().identity;
        drop(l);
        c.deny().unwrap();
        drop(c);
        let c = Arc::new(
            RetainedControl::open_paged(
                &f.root.join("paged-control"),
                &[52; 32],
                f.signer(),
                [53; 32],
                FileWitness::open(&f.root.join("paged-witness")).unwrap(),
                expected_scope,
            )
            .unwrap(),
        );
        let report = c
            .cleanup_paged_data(&f.root.join("paged-data"), prune::PruneLimits::default())
            .unwrap();
        assert!(report.payload_namespaces_removed);
        assert_eq!(
            report.permanent,
            PermanentErasure::PendingUnsupportedLocalKeyAuthority
        );
        assert!(
            ManagedPagedLog::open(&f.root.join("paged-data"), &[51; 32], identity(), c.clone())
                .is_err()
        );
        for entry in fs::read_dir(&saved).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                copy_tree(
                    &entry.path(),
                    &f.root.join("paged-data").join(entry.file_name()),
                );
            }
        }
        assert!(
            ManagedPagedLog::open(&f.root.join("paged-data"), &[51; 32], identity(), c.clone())
                .is_err()
        );
        c.cleanup_paged_data(&f.root.join("paged-data"), prune::PruneLimits::default())
            .unwrap();
        assert!(c.status().unwrap().local_cleanup_completed);
    }
    #[test]
    fn empty_owned_namespaces_consume_cleanup_work_before_any_completion() {
        let f = Fixture::new();
        let c = control(&f, true);
        let l = log(&f, c.clone(), true);
        for n in 1..=32 {
            fs::create_dir(l.directory.join(format!("data-{n:064x}"))).unwrap();
        }
        drop(l);
        c.deny().unwrap();
        let before = c.current_floor().unwrap();
        assert!(
            c.cleanup_paged_data(
                &f.root.join("paged-data"),
                prune::PruneLimits {
                    bytes: 1 << 30,
                    rows: 1
                }
            )
            .is_err()
        );
        assert_eq!(c.current_floor().unwrap(), before);
        assert!(!c.status().unwrap().local_cleanup_completed);
        c.cleanup_paged_data(&f.root.join("paged-data"), prune::PruneLimits::default())
            .unwrap();
        assert!(c.status().unwrap().local_cleanup_completed);
    }
}
