//! On-disk state of the agent: the long-term identity key and the list of paired devices.
//! Both live under `~/.config/meshwav/` and are written with mode 0600 (atomic replace).
use audio_protocol::handshake::{hex, Keypair, PublicKey};
use audio_protocol::server::Authorized;
use std::io::{Error, ErrorKind, Write};
use std::path::Path;

fn err(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, msg.into())
}

pub fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut k = [0u8; 32];
    for i in 0..32 {
        k[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(k)
}

#[cfg(unix)]
fn check_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(err(format!("{} must not be accessible by others (chmod 600)", path.display())));
    }
    Ok(())
}
#[cfg(not(unix))]
fn check_private(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn write_private(path: &Path, contents: &str, replace: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    drop(f);
    if !replace && path.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::new(ErrorKind::AlreadyExists, "file exists"));
    }
    std::fs::rename(&tmp, path)
}

/// Load the identity key, creating it on first run. Refuses a file readable by others.
pub fn load_or_create_identity(path: &Path) -> std::io::Result<Keypair> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            check_private(path)?;
            let k = unhex32(&s).ok_or_else(|| err("identity file must hold 64 hex characters"))?;
            Ok(Keypair::from_private(k))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let kp = Keypair::generate();
            write_private(path, &format!("{}\n", hex(&kp.private)), false)?;
            Ok(kp)
        }
        Err(e) => Err(e),
    }
}

/// File format: one device per line, `<64 hex public key> <name>`; `#` starts a comment.
pub fn load_authorized(path: &Path) -> std::io::Result<Vec<Authorized>> {
    let s = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    check_private(path)?;
    let mut out: Vec<Authorized> = Vec::new();
    for (i, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, name) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let public: PublicKey = unhex32(k).ok_or_else(|| err(format!("{}:{}: bad key", path.display(), i + 1)))?;
        if !out.iter().any(|a| a.public == public) {
            out.push(Authorized { public, name: name.trim().to_string() });
        }
    }
    Ok(out)
}

pub fn save_authorized(path: &Path, list: &[Authorized]) -> std::io::Result<()> {
    let mut s = String::from("# Meshwav paired devices: <public key> <name>\n");
    for a in list {
        let name: String = a.name.chars().filter(|c| !c.is_control()).collect();
        s.push_str(&format!("{} {}\n", hex(&a.public), name));
    }
    write_private(path, &s, true)
}

/// The device the user chose with `use`; absent or unreadable means automatic selection.
pub fn load_preferred(path: &Path) -> Option<[u8; 32]> {
    unhex32(std::fs::read_to_string(path).ok()?.trim())
}

pub fn save_preferred(path: &Path, key: Option<&[u8; 32]>) -> std::io::Result<()> {
    match key {
        Some(k) => write_private(path, &format!("{}\n", hex(k)), true),
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mw-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn identity_is_created_once_and_reloaded() {
        let d = tmp("id");
        let p = d.join("sub/identity");
        let a = load_or_create_identity(&p).unwrap();
        let b = load_or_create_identity(&p).unwrap();
        assert_eq!(a.public, b.public);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_identity_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("perm");
        let p = d.join("identity");
        load_or_create_identity(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create_identity(&p).is_err());
    }

    #[test]
    fn corrupt_identity_is_an_error_not_a_new_key() {
        let d = tmp("bad");
        let p = d.join("identity");
        write_private(&p, "nothex\n", false).unwrap();
        assert!(load_or_create_identity(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "nothex\n");
    }

    #[test]
    fn authorized_roundtrip_dedup_and_bad_lines() {
        let d = tmp("auth");
        let p = d.join("authorized_peers");
        assert!(load_authorized(&p).unwrap().is_empty());
        let a = Authorized { public: Keypair::generate().public, name: "Pixel 8\n".into() };
        let b = Authorized { public: Keypair::generate().public, name: "".into() };
        save_authorized(&p, &[a.clone(), b.clone()]).unwrap();
        let got = load_authorized(&p).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "Pixel 8");
        assert_eq!(got[1].public, b.public);
        let mut s = std::fs::read_to_string(&p).unwrap();
        s.push_str(&format!("{} again\n", hex(&a.public)));
        std::fs::write(&p, &s).unwrap();
        assert_eq!(load_authorized(&p).unwrap().len(), 2, "duplicates dropped");
        std::fs::write(&p, "zz name\n").unwrap();
        assert!(load_authorized(&p).is_err());
    }

    #[test]
    fn preferred_roundtrip_and_clear() {
        let p = tmp("preferred").join("active_peer");
        assert_eq!(load_preferred(&p), None);
        save_preferred(&p, Some(&[7; 32])).unwrap();
        assert_eq!(load_preferred(&p), Some([7; 32]));
        save_preferred(&p, Some(&[8; 32])).unwrap();
        assert_eq!(load_preferred(&p), Some([8; 32]));
        save_preferred(&p, None).unwrap();
        assert_eq!(load_preferred(&p), None);
        save_preferred(&p, None).unwrap(); // clearing twice is fine
    }
}
