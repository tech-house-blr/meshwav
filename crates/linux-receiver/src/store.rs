//! On-disk state of the receiver under `~/.config/meshwav/receiver/` (or `--dir`): its own identity key
//! and the one agent it is paired with. Files are written with mode 0600 (atomic replace).
use audio_protocol::handshake::{hex, Keypair, PublicKey};
use std::io::{Error, ErrorKind, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// The receiver's folder: `~/.config/meshwav/receiver`, next to the sender's files. Older versions
/// used `~/.config/meshwav-receiver` (or the same under `$XDG_CONFIG_HOME`); that folder is moved
/// across the first time, so an existing pairing and identity key carry over.
pub fn default_dir() -> PathBuf {
    #[cfg(windows)]
    {
        // Windows: `%APPDATA%\meshwav\receiver`; there is no older layout to migrate.
        return PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("meshwav").join("receiver");
    }
    #[cfg(not(windows))]
    default_dir_unix()
}

#[cfg(not(windows))]
fn default_dir_unix() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let dir = home.join(".config/meshwav/receiver");
    let mut old = vec![home.join(".config/meshwav-receiver")];
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        old.insert(0, PathBuf::from(x).join("meshwav-receiver"));
    }
    migrate(&dir, &old);
    dir
}

/// Move the first existing `old` folder to `dir` if `dir` does not exist yet. Never overwrites.
pub fn migrate(dir: &Path, old: &[PathBuf]) {
    if dir.exists() {
        return;
    }
    let Some(from) = old.iter().find(|o| o.is_dir()) else { return };
    if let Some(parent) = dir.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Err(e) = std::fs::rename(from, dir) {
        eprintln!("meshwav: could not move {} to {}: {e}", from.display(), dir.display());
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, msg.into())
}

pub fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(k)
}

/// Create the folder (and parents) and make it private to this user, so even the names of key files
/// are not listable by others. Best effort: a failure to tighten is not an error.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let _ = std::fs::remove_file(&tmp);
    let mut f = opts.open(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(unix)]
fn check_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(bad(format!("{} must not be accessible by others (chmod 600)", path.display())));
    }
    Ok(())
}
#[cfg(not(unix))]
fn check_private(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// The receiver's long-term key; created on first use.
pub fn load_or_create_identity(dir: &Path) -> std::io::Result<Keypair> {
    let path = dir.join("identity");
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            check_private(&path)?;
            let k = unhex32(&s).ok_or_else(|| bad(format!("{} is not a 64-digit hex key", path.display())))?;
            Ok(Keypair::from_private(k))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let kp = Keypair::generate();
            write_private(&path, &format!("{}\n", hex(&kp.private)))?;
            Ok(kp)
        }
        Err(e) => Err(e),
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Paired {
    pub agent: SocketAddr,
    pub key: PublicKey,
}

pub fn save_paired(dir: &Path, p: &Paired) -> std::io::Result<()> {
    write_private(&dir.join("agent"), &format!("addr={}\nkey={}\n", p.agent, hex(&p.key)))
}

pub fn load_paired(dir: &Path) -> std::io::Result<Option<Paired>> {
    let path = dir.join("agent");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('='))).map(str::trim);
    let agent = get("addr").and_then(|a| a.parse().ok()).ok_or_else(|| bad(format!("{}: bad addr", path.display())))?;
    let key = get("key").and_then(unhex32).ok_or_else(|| bad(format!("{}: bad key", path.display())))?;
    Ok(Some(Paired { agent, key }))
}

/// The sender's own name, remembered from its last handshake so lists can show it instead of an address.
pub fn save_agent_name(dir: &Path, name: &str) -> std::io::Result<()> {
    write_private(&dir.join("agent-name"), &format!("{}\n", name.trim()))
}

pub fn load_agent_name(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("agent-name")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Writes the name only when it is new.
pub fn remember_agent_name(dir: &Path, name: &str) {
    if !name.trim().is_empty() && load_agent_name(dir).as_deref() != Some(name.trim()) {
        let _ = save_agent_name(dir, name);
    }
}

pub fn forget(dir: &Path) -> std::io::Result<bool> {
    let _ = std::fs::remove_file(dir.join("agent-name"));
    match std::fs::remove_file(dir.join("agent")) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// What the agent prints: `meshwav://pair?host=H&port=P&pk=HEX&code=123456`.
#[derive(Debug, PartialEq, Eq)]
pub struct PairLink {
    pub host: String,
    pub port: u16,
    pub key: PublicKey,
    pub code: String,
}

pub fn parse_pair_link(s: &str) -> Result<PairLink, String> {
    // Accept a link pasted with surrounding text, or wrapped over several lines by a phone screen or chat app.
    let compact: String = s.split_whitespace().collect();
    let at = compact.find("meshwav://pair?").ok_or("not a meshwav://pair?... link")?;
    let q = &compact[at + "meshwav://pair?".len()..];
    // Whitespace was removed, so words after the link stick to its last value; keep only the characters each field can hold.
    let get = |k: &str| {
        let v = q.split('&').find_map(|p| p.strip_prefix(k).and_then(|r| r.strip_prefix('='))).ok_or(format!("the link has no {k}"))?;
        Ok::<&str, String>(match k {
            "pk" => &v[..v.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(v.len())],
            "code" | "port" => &v[..v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len())],
            _ => v,
        })
    };
    let port = match get("port") {
        Ok(p) => p.parse().map_err(|_| format!("bad port {p:?}"))?,
        Err(_) => 47900,
    };
    let key = unhex32(get("pk")?).ok_or("the agent key (pk) is not 64 hex digits")?;
    let code = get("code")?.to_string();
    if !audio_protocol::handshake::valid_code(&code) {
        return Err("the pairing code must be 6 digits".into());
    }
    Ok(PairLink { host: get("host")?.to_string(), port, key, code })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mw-rx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pair_link_survives_wrapping_and_surrounding_text() {
        let pk = "ab".repeat(32);
        let plain = format!("meshwav://pair?host=100.64.0.1&port=47901&pk={pk}&code=123456");
        let want = parse_pair_link(&plain).unwrap();
        let wrapped = format!("Pair me:\n  meshwav://pair?host=100.64.0.1&port=\n47901&pk={}\n{}&code=123456 \nthanks", &pk[..20], &pk[20..]);
        let got = parse_pair_link(&wrapped).unwrap();
        assert_eq!((got.host, got.port, got.key, got.code), (want.host, want.port, want.key, want.code));
        assert!(parse_pair_link("hello").is_err());
    }

    #[test]
    fn identity_is_created_once_and_private() {
        let d = tmp("id");
        let a = load_or_create_identity(&d).unwrap();
        let b = load_or_create_identity(&d).unwrap();
        assert_eq!(a.public, b.public);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("identity")).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700, "the folder is private too");
            std::fs::set_permissions(d.join("identity"), std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load_or_create_identity(&d).is_err(), "a world-readable key is refused");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn paired_agent_round_trips_and_can_be_forgotten() {
        let d = tmp("paired");
        assert_eq!(load_agent_name(&d), None);
        remember_agent_name(&d, "liberation");
        assert_eq!(load_agent_name(&d).as_deref(), Some("liberation"));
        remember_agent_name(&d, "  ");
        assert_eq!(load_agent_name(&d).as_deref(), Some("liberation"));
        assert_eq!(load_paired(&d).unwrap(), None);
        let p = Paired { agent: "100.72.29.9:47900".parse().unwrap(), key: [7; 32] };
        save_paired(&d, &p).unwrap();
        assert_eq!(load_paired(&d).unwrap(), Some(p));
        assert!(forget(&d).unwrap());
        assert!(!forget(&d).unwrap());
        assert_eq!(load_paired(&d).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn old_folder_moves_across_once_and_never_overwrites() {
        let base = std::env::temp_dir().join(format!("mw-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join("meshwav-receiver");
        let new = base.join("meshwav/receiver");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("identity"), b"key").unwrap();
        migrate(&new, &[old.clone()]);
        assert_eq!(std::fs::read(new.join("identity")).unwrap(), b"key");
        assert!(!old.exists());
        // a second old folder must not replace the existing one
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("identity"), b"other").unwrap();
        migrate(&new, &[old.clone()]);
        assert_eq!(std::fs::read(new.join("identity")).unwrap(), b"key");
        assert!(old.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pair_link_parses_and_rejects_bad_input() {
        let k = hex(&[0xab; 32]);
        let l = parse_pair_link(&format!("meshwav://pair?host=100.72.29.9&port=47901&pk={k}&code=123456")).unwrap();
        assert_eq!((l.host.as_str(), l.port, l.key, l.code.as_str()), ("100.72.29.9", 47901, [0xab; 32], "123456"));
        assert_eq!(parse_pair_link(&format!("meshwav://pair?host=h&pk={k}&code=000001")).unwrap().port, 47900);
        assert!(parse_pair_link("https://example.com").is_err());
        assert!(parse_pair_link(&format!("meshwav://pair?host=h&pk={k}&code=12345")).is_err());
        assert!(parse_pair_link("meshwav://pair?host=h&pk=zz&code=123456").is_err());
        assert!(parse_pair_link(&format!("meshwav://pair?pk={k}&code=123456")).is_err());
    }
}

/// This computer's host name, for the name shown in the agent's device list.
pub fn host_name() -> String {
    std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "linux".into())
}

/// The agent's address from a pair link.
pub fn resolve(l: &PairLink) -> Result<SocketAddr, String> {
    use std::net::ToSocketAddrs;
    (l.host.as_str(), l.port).to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or_else(|| format!("cannot resolve {}", l.host))
}
