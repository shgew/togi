use rustix::fs::{FileType, Mode};

use crate::{DirectoryProblem, LockProblem, Policy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    NamespaceRoot,
    Ancestor,
    Containing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    ReadWrite,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mend {
    Keep,
    Restrict,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LockFacts {
    pub(crate) file_type: FileType,
    pub(crate) single_link: bool,
    pub(crate) owner: u32,
    pub(crate) mode: Mode,
}

pub(crate) fn directory_problem(
    mode: Mode,
    owner: u32,
    caller: u32,
    place: Place,
) -> Option<DirectoryProblem> {
    if mode.intersects(Mode::WGRP | Mode::WOTH) && !mode.contains(Mode::SVTX) {
        return Some(DirectoryProblem::WritableWithoutSticky);
    }
    let authorized = owner == 0 || owner == caller || place == Place::NamespaceRoot;
    (!authorized).then_some(DirectoryProblem::ForeignOwner)
}

pub(crate) fn lock_verdict(
    facts: LockFacts,
    caller: u32,
    policy: Policy,
    access: Access,
) -> Result<Mend, LockProblem> {
    if facts.file_type != FileType::RegularFile {
        return Err(LockProblem::NotRegularFile);
    }
    if !facts.single_link {
        return Err(LockProblem::MultipleLinks);
    }
    let owner_allowed = match policy {
        Policy::Host => facts.owner == 0,
        Policy::Private => facts.owner == 0 || facts.owner == caller,
    };
    if !owner_allowed {
        return Err(LockProblem::ForeignOwner);
    }
    let private = Mode::RUSR | Mode::WUSR;
    if facts.mode != private && facts.mode != private | Mode::RGRP | Mode::WGRP {
        if facts.owner != caller {
            return Err(LockProblem::LegacyModeNotOwner);
        }
        return Ok(Mend::Restrict);
    }
    match access {
        Access::ReadWrite => Ok(Mend::Keep),
        Access::ReadOnly => Err(LockProblem::ReadOnly),
    }
}

#[cfg(test)]
mod tests {
    use rustix::fs::RawMode;

    use super::*;

    const CALLER: u32 = 1001;
    const FOREIGN: u32 = 2002;

    fn mode(bits: RawMode) -> Mode {
        Mode::from_bits_truncate(bits)
    }

    #[test]
    fn directory_authorization_matrix() {
        use DirectoryProblem::{ForeignOwner, WritableWithoutSticky};
        use Place::{Ancestor, Containing, NamespaceRoot};
        let cases = [
            (
                "mapped foreign namespace root",
                0o750,
                FOREIGN,
                NamespaceRoot,
                None,
            ),
            (
                "foreign namespace root as containing directory",
                0o750,
                FOREIGN,
                Containing,
                Some(ForeignOwner),
            ),
            (
                "foreign writable nonsticky namespace root",
                0o777,
                FOREIGN,
                NamespaceRoot,
                Some(WritableWithoutSticky),
            ),
            (
                "foreign read-only ancestor",
                0o555,
                FOREIGN,
                Ancestor,
                Some(ForeignOwner),
            ),
            (
                "foreign owner-writable ancestor",
                0o755,
                FOREIGN,
                Ancestor,
                Some(ForeignOwner),
            ),
            (
                "foreign sticky ancestor",
                0o1777,
                FOREIGN,
                Ancestor,
                Some(ForeignOwner),
            ),
            ("caller sticky ancestor", 0o1777, CALLER, Ancestor, None),
            ("root sticky ancestor", 0o1777, 0, Ancestor, None),
            ("caller read-only ancestor", 0o555, CALLER, Ancestor, None),
            ("root read-only ancestor", 0o555, 0, Ancestor, None),
            (
                "caller private containing directory",
                0o700,
                CALLER,
                Containing,
                None,
            ),
            (
                "root controlled containing directory",
                0o755,
                0,
                Containing,
                None,
            ),
            (
                "foreign read-only containing directory",
                0o555,
                FOREIGN,
                Containing,
                Some(ForeignOwner),
            ),
            (
                "foreign private containing directory",
                0o700,
                FOREIGN,
                Containing,
                Some(ForeignOwner),
            ),
            (
                "foreign sticky containing directory",
                0o1777,
                FOREIGN,
                Containing,
                Some(ForeignOwner),
            ),
            (
                "foreign group-writable nonsticky ancestor",
                0o575,
                FOREIGN,
                Ancestor,
                Some(WritableWithoutSticky),
            ),
            (
                "foreign other-writable nonsticky ancestor",
                0o557,
                FOREIGN,
                Ancestor,
                Some(WritableWithoutSticky),
            ),
            (
                "caller writable nonsticky ancestor",
                0o777,
                CALLER,
                Ancestor,
                Some(WritableWithoutSticky),
            ),
            (
                "root writable nonsticky ancestor",
                0o777,
                0,
                Ancestor,
                Some(WritableWithoutSticky),
            ),
            (
                "caller writable nonsticky containing directory",
                0o777,
                CALLER,
                Containing,
                Some(WritableWithoutSticky),
            ),
            (
                "root writable nonsticky containing directory",
                0o777,
                0,
                Containing,
                Some(WritableWithoutSticky),
            ),
        ];
        for (name, bits, owner, place, want) in cases {
            assert_eq!(
                directory_problem(mode(bits), owner, CALLER, place),
                want,
                "{name}"
            );
        }
    }

    fn facts(owner: u32, bits: RawMode) -> LockFacts {
        LockFacts {
            file_type: FileType::RegularFile,
            single_link: true,
            owner,
            mode: mode(bits),
        }
    }

    #[test]
    fn lock_ownership_follows_the_policy() {
        let verdict =
            |policy, owner| lock_verdict(facts(owner, 0o600), CALLER, policy, Access::ReadWrite);
        assert_eq!(verdict(Policy::Host, 0), Ok(Mend::Keep));
        assert_eq!(
            verdict(Policy::Host, CALLER),
            Err(LockProblem::ForeignOwner)
        );
        assert_eq!(verdict(Policy::Private, 0), Ok(Mend::Keep));
        assert_eq!(verdict(Policy::Private, CALLER), Ok(Mend::Keep));
        assert_eq!(
            verdict(Policy::Private, FOREIGN),
            Err(LockProblem::ForeignOwner)
        );
    }

    #[test]
    fn only_the_owner_restricts_legacy_modes() {
        let verdict =
            |owner, bits, access| lock_verdict(facts(owner, bits), CALLER, Policy::Private, access);
        for bits in [0o644, 0o444, 0o640, 0o666, 0o4600, 0o1600] {
            assert_eq!(
                verdict(CALLER, bits, Access::ReadWrite),
                Ok(Mend::Restrict),
                "{bits:o}"
            );
            assert_eq!(
                verdict(CALLER, bits, Access::ReadOnly),
                Ok(Mend::Restrict),
                "{bits:o}"
            );
            assert_eq!(
                verdict(0, bits, Access::ReadWrite),
                Err(LockProblem::LegacyModeNotOwner),
                "{bits:o}"
            );
        }
    }

    #[test]
    fn secure_modes_need_read_write_access() {
        for bits in [0o600, 0o660] {
            let verdict =
                |access| lock_verdict(facts(CALLER, bits), CALLER, Policy::Private, access);
            assert_eq!(verdict(Access::ReadWrite), Ok(Mend::Keep));
            assert_eq!(verdict(Access::ReadOnly), Err(LockProblem::ReadOnly));
        }
    }

    #[test]
    fn irregular_or_linked_objects_are_refused_before_ownership() {
        let mut irregular = facts(FOREIGN, 0o644);
        irregular.file_type = FileType::Fifo;
        assert_eq!(
            lock_verdict(irregular, CALLER, Policy::Host, Access::ReadWrite),
            Err(LockProblem::NotRegularFile)
        );
        let mut linked = facts(FOREIGN, 0o644);
        linked.single_link = false;
        assert_eq!(
            lock_verdict(linked, CALLER, Policy::Host, Access::ReadWrite),
            Err(LockProblem::MultipleLinks)
        );
    }
}
