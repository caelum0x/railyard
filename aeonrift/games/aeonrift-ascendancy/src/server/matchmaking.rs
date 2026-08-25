//! Lightweight, headless matchmaking + lobby formation.
//!
//! A deterministic queue of players (id + rating). Matching sorts by `(rating, id)`
//! and greedily pairs adjacent players whose ratings fall within a band — no RNG, no
//! wall-clock, so the same queue always forms the same lobbies. This is the seam a
//! real online service would sit behind; here it is a pure, testable core the ladder
//! ([`crate::modes::ladder`]) hands rating bands to.

/// A player waiting to be matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedPlayer {
    pub id: u64,
    pub rating: i32,
}

impl QueuedPlayer {
    pub fn new(id: u64, rating: i32) -> Self {
        QueuedPlayer { id, rating }
    }
}

/// A formed 1v1 lobby: two player ids, `a` the lower rating (deterministic ordering).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lobby {
    pub a: u64,
    pub b: u64,
}

/// A deterministic matchmaking queue.
#[derive(Clone, Debug, Default)]
pub struct Matchmaker {
    queue: Vec<QueuedPlayer>,
}

impl Matchmaker {
    /// An empty queue.
    pub fn new() -> Self {
        Matchmaker::default()
    }

    /// Add a player to the queue (ignoring a duplicate id already queued).
    pub fn enqueue(&mut self, player: QueuedPlayer) -> bool {
        if self.queue.iter().any(|p| p.id == player.id) {
            return false;
        }
        self.queue.push(player);
        true
    }

    /// Players currently waiting.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// True if nobody is waiting.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Sorted view of the queue by `(rating, id)` — the canonical matching order.
    fn sorted(&self) -> Vec<QueuedPlayer> {
        let mut v = self.queue.clone();
        v.sort_by(|x, y| x.rating.cmp(&y.rating).then(x.id.cmp(&y.id)));
        v
    }

    /// Try to form ONE lobby: the closest adjacent pair (in sorted order) whose rating
    /// gap is within `band`. Removes both from the queue on success.
    pub fn try_match(&mut self, band: i32) -> Option<Lobby> {
        let sorted = self.sorted();
        for w in sorted.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            if (hi.rating - lo.rating).abs() <= band {
                self.queue.retain(|p| p.id != lo.id && p.id != hi.id);
                return Some(Lobby { a: lo.id, b: hi.id });
            }
        }
        None
    }

    /// Greedily form as many lobbies as possible within `band`, pairing adjacent
    /// sorted players. Unmatched players remain queued.
    pub fn form_lobbies(&mut self, band: i32) -> Vec<Lobby> {
        let sorted = self.sorted();
        let mut lobbies = Vec::new();
        let mut matched: Vec<u64> = Vec::new();
        let mut i = 0;
        while i + 1 < sorted.len() {
            let lo = sorted[i];
            let hi = sorted[i + 1];
            if (hi.rating - lo.rating).abs() <= band {
                lobbies.push(Lobby { a: lo.id, b: hi.id });
                matched.push(lo.id);
                matched.push(hi.id);
                i += 2;
            } else {
                i += 1;
            }
        }
        self.queue.retain(|p| !matched.contains(&p.id));
        lobbies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_ids_are_not_requeued() {
        let mut mm = Matchmaker::new();
        assert!(mm.enqueue(QueuedPlayer::new(1, 1000)));
        assert!(!mm.enqueue(QueuedPlayer::new(1, 1200)));
        assert_eq!(mm.len(), 1);
    }

    #[test]
    fn pairs_closest_within_band() {
        let mut mm = Matchmaker::new();
        mm.enqueue(QueuedPlayer::new(1, 1000));
        mm.enqueue(QueuedPlayer::new(2, 1030));
        mm.enqueue(QueuedPlayer::new(3, 1500));
        let lobby = mm.try_match(100).expect("close pair matches");
        assert_eq!(lobby, Lobby { a: 1, b: 2 });
        assert_eq!(mm.len(), 1, "the far player stays queued");
        assert!(mm.try_match(100).is_none());
    }

    #[test]
    fn form_lobbies_is_deterministic() {
        let mut a = Matchmaker::new();
        let mut b = Matchmaker::new();
        for (id, r) in [(3, 1010), (1, 1000), (4, 1005), (2, 1002)] {
            a.enqueue(QueuedPlayer::new(id, r));
            b.enqueue(QueuedPlayer::new(id, r));
        }
        assert_eq!(a.form_lobbies(50), b.form_lobbies(50));
    }
}
