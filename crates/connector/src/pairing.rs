use std::env;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rand::{RngCore, rngs::OsRng};
use subtle::ConstantTimeEq;

const TOKEN_LENGTH: usize = 64;

pub fn default_path() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("locate the home directory")?;
    let directory = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/MiniRemote")
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| PathBuf::from(home).join(".config"))
            .join("miniremote")
    };
    Ok(directory.join("host.token"))
}

pub fn create_or_read(path: &Path, rotate: bool) -> Result<String> {
    if !rotate {
        match fs::symlink_metadata(path) {
            Ok(_) => return read(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    }
    let directory = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;

    let token = random_token();
    let temporary = directory.join(format!(".token-{}", &random_token()[..16]));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> io::Result<()> {
        writeln!(file, "{token}")?;
        file.sync_all()?;
        if rotate {
            fs::rename(&temporary, path)?;
        } else {
            fs::hard_link(&temporary, path)?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    match result {
        Ok(()) => Ok(token),
        Err(error) if !rotate && error.kind() == io::ErrorKind::AlreadyExists => read(path),
        Err(error) => Err(error).with_context(|| format!("write {}", path.display())),
    }
}

pub fn read(path: &Path) -> Result<String> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.len() > 80 {
        bail!(
            "pairing token needs a private regular file: {}",
            path.display()
        );
    }
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let token = contents.trim();
    if token.len() != TOKEN_LENGTH || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid pairing token file: {}", path.display());
    }
    Ok(token.to_owned())
}

pub fn matches(expected: &str, candidate: &str) -> bool {
    bool::from(expected.as_bytes().ct_eq(candidate.as_bytes()))
}

fn random_token() -> String {
    let mut bytes = [0_u8; TOKEN_LENGTH / 2];
    OsRng.fill_bytes(&mut bytes);
    let mut token = String::with_capacity(TOKEN_LENGTH);
    for byte in bytes {
        write!(token, "{byte:02x}").expect("writing into a String succeeds");
    }
    token
}

const INVITE_LIFETIME: u64 = 180;

pub fn create_invite(token_path: &Path) -> Result<(String, u64)> {
    let token = read(token_path)?;
    let directory = invite_directory(token_path);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("pairing invitations need a private directory");
    }
    let now = unix_time();
    for entry in fs::read_dir(&directory)?.flatten() {
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .is_some_and(|modified| {
                std::time::SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default()
                    .as_secs()
                    >= INVITE_LIFETIME
            });
        if expired {
            let _ = fs::remove_file(entry.path());
        }
    }
    let code = random_token()[..32].to_owned();
    let expires = now + INVITE_LIFETIME;
    let path = directory.join(&code);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    writeln!(file, "{expires}\n{token}")?;
    Ok((code, expires))
}

pub fn consume_invite(token_path: &Path, token: &str, code: &str) -> bool {
    if code.len() != 32 || !code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let path = invite_directory(token_path).join(code);
    let Ok(record) = fs::read_to_string(&path) else {
        return false;
    };
    // Only the caller that successfully removes this unique code can redeem it.
    if fs::remove_file(&path).is_err() {
        return false;
    }
    let mut fields = record.lines();
    let expires = fields.next().and_then(|value| value.parse::<u64>().ok());
    expires.is_some_and(|deadline| unix_time() < deadline)
        && fields
            .next()
            .is_some_and(|issued_token| matches(token, issued_token))
}

fn invite_directory(token_path: &Path) -> PathBuf {
    let mut name = token_path.as_os_str().to_owned();
    name.push(".invites");
    PathBuf::from(name)
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_invites_are_single_use_expire_and_respect_token_rotation() {
        let root = std::env::temp_dir().join(format!("miniremote-{}", random_token()));
        let token_path = root.join("host.token");
        let original = create_or_read(&token_path, false).unwrap();
        let (code, expires) = create_invite(&token_path).unwrap();
        assert!(expires > unix_time());
        assert!(consume_invite(&token_path, &original, &code));
        assert!(!consume_invite(&token_path, &original, &code));

        let (expired, _) = create_invite(&token_path).unwrap();
        let path = invite_directory(&token_path).join(&expired);
        fs::write(path, format!("0\n{original}")).unwrap();
        assert!(!consume_invite(&token_path, &original, &expired));

        let (before_rotation, _) = create_invite(&token_path).unwrap();
        let rotated = create_or_read(&token_path, true).unwrap();
        assert!(!consume_invite(&token_path, &rotated, &before_rotation));
        fs::remove_dir_all(root).unwrap();
    }
}
