//! Memoization for a recursive lookup that cuts cycles and a depth limit short.
//!
//! A lookup that returns a fixed answer when it re-enters a key under way, or when it reaches a
//! depth limit, depends on where it was entered from: which keys were under way, and how deep it
//! started. Keeping only results that saw no cut is sound but, where keys form cycles, keeps
//! nothing, and every lookup re-walks the cycle to the depth limit, exponentially (#659).
//!
//! [`CycleMemo`] also keeps the results that saw a cut, with what each depended on: per depth,
//! the keys outside it that were under way when it re-entered them, the keys it read afresh, and
//! the keys it cut at the depth limit. Read again at the same depth, a kept result is reused only
//! when each of those is as it was, so the lookup it stands for would have taken the same path
//! and returned the same answer. A result that saw no cut is kept for every depth.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::Rc;

/// What a result that saw a cut found about another key, which a reuse of it must find again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Seen {
    /// Cut at the depth limit: the key was not settled.
    AtLimit,
    /// Read afresh: the key was neither settled nor under way.
    Read,
    /// Re-entered: the key was under way, and not settled.
    UnderWay,
}

/// A result that saw a cut, and what it found about other keys.
#[derive(Debug)]
struct Unsettled<V> {
    found: Rc<V>,
    seen: Vec<(usize, Seen)>,
}

/// A lookup under way: what it has found about other keys, and whether it saw a cut.
#[derive(Debug, Default)]
struct Frame {
    seen: HashMap<usize, Seen>,
    cut: bool,
}

/// What [`CycleMemo::begin`] decided for a key.
#[derive(Debug)]
pub(crate) enum Begin<V> {
    /// The result is known: settled, or kept from a lookup that would take the same path.
    Found(Rc<V>),
    /// The lookup re-enters a key under way, or reaches the depth limit: the caller returns its
    /// fixed answer for a cut.
    Cut,
    /// The key must be read; the caller reads it and passes the result to
    /// [`CycleMemo::finish`] with this token.
    Read(Reading),
}

/// A key being read, for [`CycleMemo::finish`].
#[derive(Debug)]
#[must_use]
pub(crate) struct Reading {
    id: usize,
    depth: usize,
}

/// See the module documentation.
#[derive(Debug)]
pub(crate) struct CycleMemo<K, V> {
    ids: HashMap<K, usize>,
    /// Results that saw no cut, for any depth.
    settled: HashMap<usize, Rc<V>>,
    /// Results that saw a cut, by key and depth.
    unsettled: HashMap<(usize, usize), Vec<Unsettled<V>>>,
    under_way: HashSet<usize>,
    frames: Vec<Frame>,
    /// How many lookups were read afresh, for tests that bound it.
    #[cfg(test)]
    reads: usize,
}

impl<K, V> Default for CycleMemo<K, V> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
            settled: HashMap::new(),
            unsettled: HashMap::new(),
            under_way: HashSet::new(),
            frames: Vec::new(),
            #[cfg(test)]
            reads: 0,
        }
    }
}

impl<K: Eq + Hash, V> CycleMemo<K, V> {
    /// Starts a lookup of `key` at `depth`; `at_limit` says the depth limit cuts it. A settled
    /// result is returned before either cut is checked, as an uncached lookup would find it.
    pub(crate) fn begin(&mut self, key: K, depth: usize, at_limit: bool) -> Begin<V> {
        let next = self.ids.len();
        let id = *self.ids.entry(key).or_insert(next);
        if let Some(found) = self.settled.get(&id) {
            return Begin::Found(Rc::clone(found));
        }
        if at_limit || self.under_way.contains(&id) {
            let seen = if at_limit {
                Seen::AtLimit
            } else {
                Seen::UnderWay
            };
            self.note(id, seen);
            return Begin::Cut;
        }
        let kept = self.unsettled.get(&(id, depth)).and_then(|kept| {
            kept.iter().find(|kept| {
                kept.seen.iter().all(|&(other, seen)| {
                    !self.settled.contains_key(&other)
                        && match seen {
                            Seen::AtLimit => true,
                            Seen::Read => !self.under_way.contains(&other),
                            Seen::UnderWay => self.under_way.contains(&other),
                        }
                })
            })
        });
        if let Some(kept) = kept {
            let (found, seen) = (Rc::clone(&kept.found), kept.seen.clone());
            for (other, seen) in seen {
                self.note(other, seen);
            }
            self.note(id, Seen::Read);
            return Begin::Found(found);
        }
        self.under_way.insert(id);
        self.frames.push(Frame::default());
        #[cfg(test)]
        {
            self.reads += 1;
        }
        Begin::Read(Reading { id, depth })
    }

    /// Ends the lookup `reading` with what it found, and returns it.
    pub(crate) fn finish(&mut self, reading: Reading, found: V) -> Rc<V> {
        let Reading { id, depth } = reading;
        let mut frame = self.frames.pop().unwrap_or_default();
        self.under_way.remove(&id);
        let found = Rc::new(found);
        // What it found about itself holds for any reuse, which finds it neither settled nor
        // under way.
        frame.seen.remove(&id);
        if !frame.cut {
            self.settled.insert(id, Rc::clone(&found));
            return found;
        }
        let seen = frame.seen.into_iter().collect::<Vec<_>>();
        for &(other, kind) in &seen {
            self.note(other, kind);
        }
        self.note(id, Seen::Read);
        self.unsettled
            .entry((id, depth))
            .or_default()
            .push(Unsettled {
                found: Rc::clone(&found),
                seen,
            });
        found
    }

    /// How many lookups were read afresh.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> usize {
        self.reads
    }

    /// Records, for the lookup under way, what it found about `id`, which makes it a lookup
    /// that saw a cut.
    fn note(&mut self, id: usize, seen: Seen) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        frame.cut = true;
        let entry = frame.seen.entry(id).or_insert(seen);
        // A key cut at the limit may also be re-entered or read elsewhere; both say more.
        *entry = (*entry).max(seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup over a graph of keys whose answer depends on where cuts fall: a key's weight plus
    /// what each key after it reads, with a cut reading as 100.
    fn walk(
        memo: &mut CycleMemo<usize, u64>,
        edges: &[Vec<usize>],
        key: usize,
        depth: usize,
        limit: usize,
    ) -> u64 {
        let reading = match memo.begin(key, depth, depth >= limit) {
            Begin::Found(found) => return *found,
            Begin::Cut => return 100,
            Begin::Read(reading) => reading,
        };
        let mut total = key as u64 + 1;
        for &next in &edges[key] {
            total = total
                .wrapping_mul(3)
                .wrapping_add(walk(memo, edges, next, depth + 1, limit));
        }
        *memo.finish(reading, total)
    }

    /// The lookup as it was memoized before: only results that saw no cut are kept, for any
    /// depth. Returns the answer and whether a cut was seen.
    fn walk_settled_only(
        settled: &mut HashMap<usize, u64>,
        under_way: &mut Vec<usize>,
        edges: &[Vec<usize>],
        (key, depth, limit): (usize, usize, usize),
    ) -> (u64, bool) {
        if let Some(found) = settled.get(&key) {
            return (*found, false);
        }
        if depth >= limit || under_way.contains(&key) {
            return (100, true);
        }
        under_way.push(key);
        let (mut total, mut cut) = (key as u64 + 1, false);
        for &next in &edges[key] {
            let (below, below_cut) =
                walk_settled_only(settled, under_way, edges, (next, depth + 1, limit));
            total = total.wrapping_mul(3).wrapping_add(below);
            cut |= below_cut;
        }
        under_way.pop();
        if !cut {
            settled.insert(key, total);
        }
        (total, cut)
    }

    /// A small deterministic generator, so the graphs are the same on every run.
    fn next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    #[test]
    fn kept_results_read_as_the_settled_only_memo_reads_them() {
        let mut state = 7;
        for _ in 0..400 {
            let keys = 2 + (next(&mut state) % 7) as usize;
            let limit = 1 + (next(&mut state) % 6) as usize;
            let edges = (0..keys)
                .map(|_| {
                    (0..next(&mut state) % 4)
                        .map(|_| (next(&mut state) % keys as u64) as usize)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut memo = CycleMemo::default();
            let mut settled = HashMap::new();
            for _ in 0..12 {
                let key = (next(&mut state) % keys as u64) as usize;
                let depth = (next(&mut state) % (limit as u64 + 1)) as usize;
                let expected =
                    walk_settled_only(&mut settled, &mut Vec::new(), &edges, (key, depth, limit)).0;
                assert_eq!(
                    walk(&mut memo, &edges, key, depth, limit),
                    expected,
                    "{edges:?} key {key} depth {depth} limit {limit}"
                );
            }
        }
    }

    #[test]
    fn a_tree_is_read_once_per_key() {
        let edges = vec![vec![1, 2], vec![3], vec![3], vec![]];
        let mut memo = CycleMemo::default();
        walk(&mut memo, &edges, 0, 0, 8);
        assert_eq!(memo.reads(), 4);
    }

    #[test]
    fn a_cycle_is_not_walked_once_per_path() {
        // Every key reaches every other: the settled-only memo keeps nothing and reads
        // (keys - 1) ^ limit paths, about 390,000 here.
        let (keys, limit) = (6, 8);
        let edges = (0..keys)
            .map(|key| (0..keys).filter(|other| *other != key).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let mut memo = CycleMemo::default();
        for key in 0..keys {
            walk(&mut memo, &edges, key, 0, limit);
        }
        // At most one read per key, depth and set of keys under way.
        assert!(
            memo.reads() <= keys * limit * (1 << keys),
            "{}",
            memo.reads()
        );
    }
}
