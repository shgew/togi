use std::io;
use std::path::Path;

/// Identity taken from the current system's specialisation, never a filename generation number.
#[derive(Clone, Debug)]
pub(super) struct Identity {
    pub kernel: String,
    pub initrd: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct LoaderEntry {
    pub id: String,
    pub contents: String,
}

impl LoaderEntry {
    pub(super) fn matches(&self, identity: &Identity) -> bool {
        let mut kernels = Vec::new();
        let mut initrds = Vec::new();
        let mut options = Vec::new();
        for (key, value) in fields(&self.contents) {
            match key {
                "linux" => kernels.push(value),
                "initrd" => initrds.push(value),
                "options" => options.extend(value.split_whitespace()),
                _ => {}
            }
        }
        kernels.len() == 1
            && filename(kernels[0]) == Some(identity.kernel.as_str())
            && initrds
                .iter()
                .any(|path| filename(path) == Some(identity.initrd.as_str()))
            && options
                .iter()
                .copied()
                .eq(identity.options.iter().map(String::as_str))
    }
}

pub(super) fn fields(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let split = line.find(char::is_whitespace)?;
        Some((&line[..split], line[split..].trim()))
    })
}

fn filename(path: &str) -> Option<&str> {
    Path::new(path).file_name()?.to_str()
}

pub(super) fn resolve(entries: &[LoaderEntry], identity: &Identity) -> io::Result<String> {
    let mut matches = entries.iter().filter(|entry| entry.matches(identity));
    let entry = matches.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no loader entry matches the current system specialisation kernel, initrd and options",
        )
    })?;
    if matches.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multiple loader entries match the current system specialisation",
        ));
    }
    Ok(entry.id.clone())
}

/// systemd-boot removes boot-counting suffixes from stable entry identifiers, but keeps `.conf`.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn entry_id(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".conf")?;
    let stable = match stem.rsplit_once('+') {
        Some((base, counter))
            if !counter.is_empty()
                && counter.split('-').count() <= 2
                && counter
                    .split('-')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) =>
        {
            base
        }
        _ => stem,
    };
    Some(format!("{stable}.conf"))
}
/// NixOS's systemd-boot builder names copied artifacts after their canonical store identity.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn copied_filename(path: &Path) -> io::Result<String> {
    let relative = path
        .strip_prefix("/nix/store")
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let store = relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing store artifact identity",
            )
        })?;
    let suffix = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "non-UTF-8 store artifact identity",
            )
        })?;
    Ok(if store == suffix {
        format!("{suffix}.efi")
    } else {
        format!("{store}-{suffix}.efi")
    })
}
