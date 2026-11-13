//! Kernel socket-table inspection used by tests to measure the REAL number of client sockets.
//!
//! Linux: netlink `SOCK_DIAG` dumps (microseconds per query, unlike parsing /proc/net/tcp whose
//! cost grows with TIME_WAIT entries). Other OSes: [`SockDiag::open`] returns `None`.

/// All TCP states are requested (bit n = TCP state n). Ownership, not state, decides what is
/// counted (see [`SockDiag::client_sockets_to_port`]): FIN_WAIT1/2 occur after
/// shutdown(SHUT_WR) while the fd is still open (hyper shuts down before dropping), so a state
/// filter would miss sockets we still hold (ADR-0011).
///
/// Only TIME_WAIT (state 6) is left out of the request: it never has an owning fd (inode 0), so
/// it can never be counted, and thousands of them (a test run leaves many) would make every
/// dump slow and the sampler coarse.
pub const ALL_TCP_STATES: u32 = 0xFFF & !(1 << 6);

pub struct SockDiag {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(target_os = "linux")]
    seq: u32,
    /// inode -> is it an fd of this process? Rescanned only for inodes not yet classified.
    #[cfg(target_os = "linux")]
    owned: std::collections::HashMap<u64, bool>,
}

#[cfg(target_os = "linux")]
impl SockDiag {
    pub fn open() -> Option<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: plain syscall with constant arguments.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_SOCK_DIAG,
            )
        };
        // SAFETY: fd is a freshly created descriptor owned only here.
        (fd >= 0).then(|| Self {
            fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
            seq: 0,
            owned: std::collections::HashMap::new(),
        })
    }

    /// Number of TCP sockets (IPv4 + IPv6) whose REMOTE port is `port` and whose inode is an
    /// open fd of THIS process (matched against the `socket:[inode]` links in /proc/self/fd).
    /// Counted by ownership regardless of state: orphaned sockets and TIME_WAIT have inode 0 /
    /// no fd and drop out by themselves. Server-side sockets have local port == `port` and are
    /// not counted. `None` on any error or sequence mismatch: the caller must then reopen the
    /// socket, since unread dump data may remain queued.
    pub fn client_sockets_to_port(&mut self, port: u16) -> Option<usize> {
        // a set: a socket seen twice by a non-atomic dump must still count once
        let mut inodes = self.dump(libc::AF_INET as u8, port)?;
        inodes.extend(self.dump(libc::AF_INET6 as u8, port)?);
        if inodes.is_empty() {
            return Some(0);
        }
        if inodes.iter().any(|i| !self.owned.contains_key(i)) {
            let ours = own_socket_inodes();
            for i in &inodes {
                self.owned.entry(*i).or_insert_with(|| ours.contains(i));
            }
            if self.owned.len() > 100_000 {
                self.owned.clear(); // bounded; inodes of long-gone sockets are never queried again
            }
        }
        Some(
            inodes
                .iter()
                .filter(|i| self.owned.get(i) == Some(&true))
                .count(),
        )
    }

    /// Inodes of client-side sockets (remote port == `port`, inode != 0) in any state.
    fn dump(&mut self, family: u8, port: u16) -> Option<std::collections::HashSet<u64>> {
        use std::os::fd::AsRawFd;
        const SOCK_DIAG_BY_FAMILY: u16 = 20;
        const NLM_F_REQUEST_DUMP: u16 = 0x1 | 0x300;
        const MASK: u32 = ALL_TCP_STATES;
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let fd = self.fd.as_raw_fd();
        let mut req = Vec::with_capacity(72);
        req.extend_from_slice(&72u32.to_ne_bytes()); // nlmsg_len
        req.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        req.extend_from_slice(&NLM_F_REQUEST_DUMP.to_ne_bytes());
        req.extend_from_slice(&seq.to_ne_bytes());
        req.extend_from_slice(&0u32.to_ne_bytes()); // pid
        req.extend_from_slice(&[family, libc::IPPROTO_TCP as u8, 0, 0]);
        req.extend_from_slice(&MASK.to_ne_bytes());
        req.extend_from_slice(&[0u8; 48]); // inet_diag_sockid: wildcard
                                           // SAFETY: valid buffer and length.
        let sent = unsafe { libc::send(fd, req.as_ptr().cast(), req.len(), 0) };
        if sent != req.len() as isize {
            return None;
        }
        let mut found = std::collections::HashSet::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            // SAFETY: valid buffer and length.
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return None;
            }
            let n = n as usize;
            let mut off = 0usize;
            while off + 16 <= n {
                let len = u32::from_ne_bytes(buf[off..off + 4].try_into().ok()?) as usize;
                let ty = u16::from_ne_bytes(buf[off + 4..off + 6].try_into().ok()?);
                let mseq = u32::from_ne_bytes(buf[off + 8..off + 12].try_into().ok()?);
                if len < 16 || off + len > n || mseq != seq {
                    return None;
                }
                match ty {
                    3 => return Some(found), // NLMSG_DONE
                    2 => return None,        // NLMSG_ERROR
                    SOCK_DIAG_BY_FAMILY if len >= 16 + 72 => {
                        // inet_diag_msg: family,state,timer,retrans (4) then sport/dport (be16)
                        let dport = u16::from_be_bytes([buf[off + 16 + 6], buf[off + 16 + 7]]);
                        // inet_diag_msg: id(48) at 4, then expires, rqueue, wqueue, uid, inode
                        let io = off + 16 + 68;
                        let inode = u32::from_ne_bytes(buf[io..io + 4].try_into().ok()?);
                        if dport == port && inode != 0 {
                            found.insert(u64::from(inode));
                        }
                    }
                    _ => {}
                }
                off += (len + 3) & !3;
            }
        }
    }
}

/// Inodes of the sockets this process currently has open (`/proc/self/fd/* -> socket:[N]`).
#[cfg(target_os = "linux")]
fn own_socket_inodes() -> std::collections::HashSet<u64> {
    let mut set = std::collections::HashSet::new();
    if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
        for e in dir.flatten() {
            if let Ok(t) = std::fs::read_link(e.path()) {
                let t = t.to_string_lossy();
                if let Some(n) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')) {
                    if let Ok(n) = n.parse() {
                        set.insert(n);
                    }
                }
            }
        }
    }
    set
}

#[cfg(not(target_os = "linux"))]
impl SockDiag {
    pub fn open() -> Option<Self> {
        None
    }
    pub fn client_sockets_to_port(&mut self, _port: u16) -> Option<usize> {
        None
    }
}
