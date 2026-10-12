use nix::unistd::User;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdKind {
    User,
    Group,
}

impl std::fmt::Display for IdKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::User => "uid",
            Self::Group => "primary gid",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("the backend account name is empty")]
    EmptyName,
    #[error("no backend account is configured")]
    NotConfigured,
    #[error("look up account {name}")]
    Lookup {
        name: String,
        #[source]
        source: nix::Error,
    },
    #[error("account {name} does not exist")]
    Unknown { name: String },
    #[error("{kind} {value} is reserved: backends run as an unprivileged account")]
    Reserved { kind: IdKind, value: u32 },
}

/// The unprivileged account a backend runs as: uid and primary gid each in `1..=u32::MAX - 1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendIdentity {
    uid: u32,
    gid: u32,
}

impl BackendIdentity {
    pub fn new(uid: u32, gid: u32) -> Result<Self, IdentityError> {
        for (kind, value) in [(IdKind::User, uid), (IdKind::Group, gid)] {
            if value == 0 || value == u32::MAX {
                return Err(IdentityError::Reserved { kind, value });
            }
        }
        Ok(Self { uid, gid })
    }

    pub fn lookup(name: &str) -> Result<Self, IdentityError> {
        if name.is_empty() {
            return Err(IdentityError::EmptyName);
        }
        let user = User::from_name(name)
            .map_err(|source| IdentityError::Lookup {
                name: name.to_owned(),
                source,
            })?
            .ok_or_else(|| IdentityError::Unknown {
                name: name.to_owned(),
            })?;
        Self::new(user.uid.as_raw(), user.gid.as_raw())
    }

    pub const fn uid(self) -> u32 {
        self.uid
    }

    pub const fn gid(self) -> u32 {
        self.gid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_bounded_away_from_root_and_the_unchanged_sentinel() {
        assert!(BackendIdentity::new(997, 996).is_ok());
        assert!(BackendIdentity::new(1, u32::MAX - 1).is_ok());
        assert!(matches!(
            BackendIdentity::new(0, 996),
            Err(IdentityError::Reserved {
                kind: IdKind::User,
                value: 0
            })
        ));
        assert!(matches!(
            BackendIdentity::new(997, 0),
            Err(IdentityError::Reserved {
                kind: IdKind::Group,
                value: 0
            })
        ));
        assert!(matches!(
            BackendIdentity::new(u32::MAX, 996),
            Err(IdentityError::Reserved {
                kind: IdKind::User,
                ..
            })
        ));
        assert!(matches!(
            BackendIdentity::new(997, u32::MAX),
            Err(IdentityError::Reserved {
                kind: IdKind::Group,
                ..
            })
        ));
    }

    #[test]
    fn an_empty_or_unknown_account_is_refused_not_defaulted() {
        assert!(matches!(
            BackendIdentity::lookup(""),
            Err(IdentityError::EmptyName)
        ));
        assert!(matches!(
            BackendIdentity::lookup("togi-no-such-account-4f2a"),
            Err(IdentityError::Unknown { .. })
        ));
    }

    #[test]
    fn root_is_refused_by_name() {
        assert!(matches!(
            BackendIdentity::lookup("root"),
            Err(IdentityError::Reserved { value: 0, .. })
        ));
    }

    #[test]
    fn an_ordinary_account_resolves_to_its_uid_and_primary_gid() {
        let Ok(Some(user)) = User::from_name("nobody") else {
            return;
        };
        let identity = BackendIdentity::lookup("nobody").unwrap();
        assert_eq!(
            (identity.uid(), identity.gid()),
            (user.uid.as_raw(), user.gid.as_raw())
        );
    }
}
