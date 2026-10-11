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
