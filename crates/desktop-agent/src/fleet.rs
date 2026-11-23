//! Which paired receiver gets the audio. Pure policy (no sockets, no crypto), so it is unit-testable.
//!
//! Several paired receivers may be connected at once. Exactly one is *active* and gets audio; the
//! others stay on keepalives and become active instantly when chosen.

pub type Key = [u8; 32];

/// Pick the active receiver.
/// 1. The user's preferred device, whenever it is online.
/// 2. Otherwise the current one, while it stays online (a newcomer never steals the stream).
/// 3. Otherwise the longest-connected online device (`online` is ordered oldest first).
pub fn choose(preferred: Option<Key>, current: Option<Key>, online: &[Key]) -> Option<Key> {
    if let Some(p) = preferred.filter(|p| online.contains(p)) {
        return Some(p);
    }
    if let Some(c) = current.filter(|c| online.contains(c)) {
        return Some(c);
    }
    online.first().copied()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    One(usize),
    None,
    Ambiguous,
}

/// Find a device by exact name (case-insensitive), else by unique name prefix, else by unique
/// hex key prefix (at least 4 characters). `devices` is `(key, name)` in any stable order.
pub fn resolve(query: &str, devices: &[(Key, String)]) -> Resolved {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Resolved::None;
    }
    let pick = |hits: Vec<usize>| match hits.len() {
        0 => None,
        1 => Some(Resolved::One(hits[0])),
        _ => Some(Resolved::Ambiguous),
    };
    let exact: Vec<usize> = (0..devices.len()).filter(|&i| devices[i].1.to_lowercase() == q).collect();
    if let Some(r) = pick(exact) {
        return r;
    }
    let by_name: Vec<usize> = (0..devices.len()).filter(|&i| devices[i].1.to_lowercase().starts_with(&q)).collect();
    if let Some(r) = pick(by_name) {
        return r;
    }
    if q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit()) {
        let by_key: Vec<usize> = (0..devices.len()).filter(|&i| audio_protocol::handshake::hex(&devices[i].0).starts_with(&q)).collect();
        if let Some(r) = pick(by_key) {
            return r;
        }
    }
    Resolved::None
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Key = [1; 32];
    const B: Key = [2; 32];
    const C: Key = [3; 32];

    #[test]
    fn nobody_online_means_nobody_active() {
        assert_eq!(choose(Some(A), Some(B), &[]), None);
    }

    #[test]
    fn newcomer_does_not_steal_the_stream() {
        assert_eq!(choose(None, Some(A), &[A, B]), Some(A));
    }

    #[test]
    fn first_device_online_becomes_active() {
        assert_eq!(choose(None, None, &[B]), Some(B));
        assert_eq!(choose(None, None, &[B, A]), Some(B)); // oldest connection wins
    }

    #[test]
    fn preferred_device_wins_whenever_online() {
        assert_eq!(choose(Some(B), Some(A), &[A, B]), Some(B));
        assert_eq!(choose(Some(B), None, &[A, B]), Some(B));
    }

    #[test]
    fn offline_preferred_falls_back_and_returns_when_back() {
        assert_eq!(choose(Some(B), Some(A), &[A]), Some(A));
        assert_eq!(choose(Some(B), None, &[A, C]), Some(A));
        assert_eq!(choose(Some(B), Some(A), &[A, B]), Some(B));
    }

    #[test]
    fn active_dropping_hands_over_to_the_oldest_remaining() {
        assert_eq!(choose(None, Some(A), &[C, B]), Some(C));
    }

    fn devs() -> Vec<(Key, String)> {
        vec![(A, "Galaxy".into()), (B, "Desk PC".into()), (C, "Desk Laptop".into())]
    }

    #[test]
    fn resolve_by_exact_prefix_and_key() {
        assert_eq!(resolve("galaxy", &devs()), Resolved::One(0));
        assert_eq!(resolve("GAL", &devs()), Resolved::One(0));
        assert_eq!(resolve("desk pc", &devs()), Resolved::One(1));
        assert_eq!(resolve("desk", &devs()), Resolved::Ambiguous);
        assert_eq!(resolve("0202", &devs()), Resolved::One(1));
        assert_eq!(resolve("nope", &devs()), Resolved::None);
        assert_eq!(resolve("  ", &devs()), Resolved::None);
        assert_eq!(resolve("02", &devs()), Resolved::None); // too short to be a key prefix
    }

    #[test]
    fn exact_name_beats_longer_prefix_matches() {
        let d = vec![(A, "pc".to_string()), (B, "pc two".to_string())];
        assert_eq!(resolve("pc", &d), Resolved::One(0));
    }
}
