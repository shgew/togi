use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidFileName {
    #[error("file name is empty")]
    Empty,
    #[error("file name is `.`")]
    CurrentDirectory,
    #[error("file name is `..`")]
    ParentDirectory,
    #[error("file name contains `/`")]
    Separator,
    #[error("file name contains NUL")]
    Nul,
}

const fn validate(name: &str) -> Result<(), InvalidFileName> {
    match name.as_bytes() {
        [] => Err(InvalidFileName::Empty),
        b"." => Err(InvalidFileName::CurrentDirectory),
        b".." => Err(InvalidFileName::ParentDirectory),
        bytes => {
            let mut index = 0;
            while index < bytes.len() {
                match bytes[index] {
                    b'/' => return Err(InvalidFileName::Separator),
                    0 => return Err(InvalidFileName::Nul),
                    _ => index += 1,
                }
            }
            Ok(())
        }
    }
}

/// One path component naming a file inside a trial's work directory.
///
/// The trial opens it relative to a directory descriptor, so it can never leave that directory.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileName(Cow<'static, str>);

impl FileName {
    /// Names a file from a literal; used in a `const`, an invalid literal fails the build.
    pub const fn from_static(name: &'static str) -> Self {
        assert!(validate(name).is_ok(), "invalid file name literal");
        Self(Cow::Borrowed(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FileName {
    type Error = InvalidFileName;

    fn try_from(name: String) -> Result<Self, InvalidFileName> {
        validate(&name)?;
        Ok(Self(Cow::Owned(name)))
    }
}

impl FromStr for FileName {
    type Err = InvalidFileName;

    fn from_str(name: &str) -> Result<Self, InvalidFileName> {
        name.to_owned().try_into()
    }
}

impl AsRef<str> for FileName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<OsStr> for FileName {
    fn as_ref(&self) -> &OsStr {
        OsStr::new(self.as_str())
    }
}

impl AsRef<Path> for FileName {
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl fmt::Display for FileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
