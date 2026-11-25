//! The local control channel between `meshwav devices | use | ...` and the running agent.
//!
//! Unix: an owner-only Unix socket. Windows has no Unix sockets in the standard library, so the
//! agent listens on a loopback TCP port picked at start and writes `port token` to the control
//! file in the user's own `%APPDATA%\meshwav` folder; a client must send the token as its first
//! line, so another local user or program that merely finds the port cannot give commands.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    pub type Stream = UnixStream;
    pub struct Listener(UnixListener);

    pub fn bind(path: &Path) -> io::Result<Listener> {
        let _ = std::fs::remove_file(path); // a stale socket from a crashed run; the UDP bind already proved we are the only agent
        let old_umask = unsafe { libc::umask(0o177) };
        let listener = UnixListener::bind(path);
        unsafe { libc::umask(old_umask) };
        listener.map(Listener)
    }

    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            self.0.accept().map(|(s, _)| s)
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    pub type Stream = TcpStream;
    pub struct Listener {
        l: TcpListener,
        token: String,
    }

    pub fn bind(path: &Path) -> io::Result<Listener> {
        let mut raw = [0u8; 16];
        getrandom::getrandom(&mut raw).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = l.local_addr()?.port();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, format!("{port} {token}"))?;
        Ok(Listener { l, token })
    }

    pub fn connect(path: &Path) -> io::Result<Stream> {
        let info = std::fs::read_to_string(path)?;
        let (port, token) = info.trim().split_once(' ').ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "control file is malformed"))?;
        let port: u16 = port.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "control file has a bad port"))?;
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        writeln!(s, "{token}")?;
        Ok(s)
    }

    impl Listener {
        /// Next connection whose first line is the right token; others are dropped.
        pub fn accept(&self) -> io::Result<Stream> {
            loop {
                let (s, _) = self.l.accept()?;
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                // Read the token a byte at a time: a buffered reader could swallow the request line behind it.
                let mut first = Vec::new();
                let mut b = [0u8; 1];
                while first.len() < 128 && (&s).read(&mut b).map(|n| n == 1).unwrap_or(false) && b[0] != b'\n' {
                    first.push(b[0]);
                }
                if first.strip_suffix(b"\r").unwrap_or(&first) == self.token.as_bytes() {
                    return Ok(s);
                }
            }
        }
    }
}

pub use imp::{bind, connect};
