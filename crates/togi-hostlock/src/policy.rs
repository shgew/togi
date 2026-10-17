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
    use test_case::test_case;

    use super::*;

    const CALLER: u32 = 1001;
    const FOREIGN: u32 = 2002;

    fn mode(bits: RawMode) -> Mode {
        Mode::from_bits_truncate(bits)
    }

    #[test_case(0o750, FOREIGN, Place::NamespaceRoot, None; "mapped foreign namespace root")]
    #[test_case(
        0o750,
        FOREIGN,
        Place::Containing,
        Some(DirectoryProblem::ForeignOwner);
        "foreign namespace root as containing directory"
    )]
    #[test_case(
        0o777,
        FOREIGN,
        Place::NamespaceRoot,
        Some(DirectoryProblem::WritableWithoutSticky);
        "foreign writable nonsticky namespace root"
    )]
    #[test_case(
        0o555,
        FOREIGN,
        Place::Ancestor,
        Some(DirectoryProblem::ForeignOwner);
        "foreign read-only ancestor"
    )]
    #[test_case(
        0o755,
        FOREIGN,
        Place::Ancestor,
        Some(DirectoryProblem::ForeignOwner);
        "foreign owner-writable ancestor"
    )]
    #[test_case(
        0o1777,
        FOREIGN,
        Place::Ancestor,
        Some(DirectoryProblem::ForeignOwner);
        "foreign sticky ancestor"
    )]
    #[test_case(0o1777, CALLER, Place::Ancestor, None; "caller sticky ancestor")]
    #[test_case(0o1777, 0, Place::Ancestor, None; "root sticky ancestor")]
    #[test_case(0o555, CALLER, Place::Ancestor, None; "caller read-only ancestor")]
    #[test_case(0o555, 0, Place::Ancestor, None; "root read-only ancestor")]
    #[test_case(0o700, CALLER, Place::Containing, None; "caller private containing directory")]
    #[test_case(0o755, 0, Place::Containing, None; "root controlled containing directory")]
    #[test_case(
        0o555,
        FOREIGN,
        Place::Containing,
        Some(DirectoryProblem::ForeignOwner);
        "foreign read-only containing directory"
    )]
    #[test_case(
        0o700,
        FOREIGN,
        Place::Containing,
        Some(DirectoryProblem::ForeignOwner);
        "foreign private containing directory"
    )]
    #[test_case(
        0o1777,
        FOREIGN,
        Place::Containing,
        Some(DirectoryProblem::ForeignOwner);
        "foreign sticky containing directory"
    )]
    #[test_case(
        0o575,
        FOREIGN,
        Place::Ancestor,
        Some(DirectoryProblem::WritableWithoutSticky);
        "foreign group-writable nonsticky ancestor"
    )]
    #[test_case(
        0o557,
        FOREIGN,
        Place::Ancestor,
        Some(DirectoryProblem::WritableWithoutSticky);
        "foreign other-writable nonsticky ancestor"
    )]
    #[test_case(
        0o777,
        CALLER,
        Place::Ancestor,
        Some(DirectoryProblem::WritableWithoutSticky);
        "caller writable nonsticky ancestor"
    )]
    #[test_case(
        0o777,
        0,
        Place::Ancestor,
        Some(DirectoryProblem::WritableWithoutSticky);
        "root writable nonsticky ancestor"
    )]
    #[test_case(
        0o777,
        CALLER,
        Place::Containing,
        Some(DirectoryProblem::WritableWithoutSticky);
        "caller writable nonsticky containing directory"
    )]
    #[test_case(
        0o777,
        0,
        Place::Containing,
        Some(DirectoryProblem::WritableWithoutSticky);
        "root writable nonsticky containing directory"
    )]
    fn directory_authorization_matrix(
        bits: RawMode,
        owner: u32,
        place: Place,
        want: Option<DirectoryProblem>,
    ) {
        assert_eq!(directory_problem(mode(bits), owner, CALLER, place), want);
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

    #[test_case(0o644; "world readable")]
    #[test_case(0o444; "read only")]
    #[test_case(0o640; "group readable")]
    #[test_case(0o666; "world writable")]
    #[test_case(0o4600; "setuid")]
    #[test_case(0o1600; "sticky")]
    fn only_the_owner_restricts_legacy_modes(bits: RawMode) {
        let verdict =
            |owner, bits, access| lock_verdict(facts(owner, bits), CALLER, Policy::Private, access);
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

    #[test_case(0o600; "owner read write")]
    #[test_case(0o660; "owner and group read write")]
    fn secure_modes_need_read_write_access(bits: RawMode) {
        let verdict = |access| lock_verdict(facts(CALLER, bits), CALLER, Policy::Private, access);
        assert_eq!(verdict(Access::ReadWrite), Ok(Mend::Keep));
        assert_eq!(verdict(Access::ReadOnly), Err(LockProblem::ReadOnly));
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
