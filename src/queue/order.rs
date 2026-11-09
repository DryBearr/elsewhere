//! The queue itself: which entries, in which order they play, and which one is current.
//! Plain logic without the engine or the library, so every rule is tested on its own.
//!
//! - Every entry has its own key (the same item can be queued twice).
//! - Shuffle plays every entry once before any comes again, keeps the current entry
//!   playing, and is repeatable from its seed.
//! - Repeat one replays the current entry when it ends; a skip still moves on.
//! - Repeat all starts over after the last entry (shuffled anew, never with the entry that
//!   just played first); without repeat the queue ends.

/// What happens when the queue's end is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Off,
    All,
    One,
}

/// One queued entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Unique within this queue.
    pub key: u64,
    pub item_id: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct Order {
    /// In the order they were queued (what a list shows).
    slots: Vec<Slot>,
    /// The play order: indexes into `slots`.
    order: Vec<usize>,
    /// Where in `order` playback is.
    at: Option<usize>,
    pub repeat: Repeat,
    shuffle: bool,
    seed: u64,
    next_key: u64,
}

impl Order {
    pub fn new(seed: u64) -> Order {
        Order {
            slots: Vec::new(),
            order: Vec::new(),
            at: None,
            repeat: Repeat::Off,
            shuffle: false,
            seed,
            next_key: 1,
        }
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// The entry playing (or about to).
    pub fn current(&self) -> Option<Slot> {
        self.at.map(|at| self.slots[self.order[at]])
    }

    /// The index in `slots` of the current entry.
    pub fn current_index(&self) -> Option<usize> {
        self.at.map(|at| self.order[at])
    }

    /// Replaces the queue with `items` and makes `start` (an index into them) current.
    /// With shuffle on, `start` plays first and the rest follow shuffled.
    pub fn set(&mut self, items: &[i64], start: usize) -> Option<Slot> {
        self.slots = items
            .iter()
            .map(|&item_id| {
                let key = self.next_key;
                self.next_key += 1;
                Slot { key, item_id }
            })
            .collect();
        if self.slots.is_empty() {
            self.order.clear();
            self.at = None;
            return None;
        }
        let start = start.min(self.slots.len() - 1);
        self.order = (0..self.slots.len()).collect();
        self.at = Some(start);
        if self.shuffle {
            self.shuffle_from_current();
        }
        self.current()
    }

    /// The entry after the current one. `ended` is true when the current one played to its
    /// end (repeat one replays it then); a skip is `ended == false`. None: the queue is over.
    pub fn next(&mut self, ended: bool) -> Option<Slot> {
        let at = self.at?;
        if ended && self.repeat == Repeat::One {
            return self.current();
        }
        if at + 1 < self.order.len() {
            self.at = Some(at + 1);
            return self.current();
        }
        if self.repeat == Repeat::Off {
            return None;
        }
        // Repeat all (or one, when skipping past the end): from the top again.
        if self.shuffle && self.order.len() > 1 {
            let last = self.order[at];
            self.seed = splitmix(self.seed);
            self.order = (0..self.slots.len()).collect();
            shuffle_in_place(&mut self.order, self.seed);
            // The entry that just played does not come first again.
            if self.order[0] == last {
                self.order.swap(0, 1);
            }
        }
        self.at = Some(0);
        self.current()
    }

    /// The entry before the current one; at the first entry, the first one (with repeat
    /// all: the last one).
    pub fn previous(&mut self) -> Option<Slot> {
        let at = self.at?;
        self.at = Some(if at > 0 {
            at - 1
        } else if self.repeat == Repeat::All {
            self.order.len() - 1
        } else {
            0
        });
        self.current()
    }

    /// Makes the entry with `key` current. False when there is none.
    pub fn jump(&mut self, key: u64) -> bool {
        match self
            .order
            .iter()
            .position(|&index| self.slots[index].key == key)
        {
            Some(at) => {
                self.at = Some(at);
                true
            }
            None => false,
        }
    }

    /// Shuffle on: the current entry stays current and the rest follow shuffled. Off: the
    /// queue's own order again, from the current entry.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        if self.slots.is_empty() {
            return;
        }
        if on {
            self.seed = splitmix(self.seed);
            self.shuffle_from_current();
        } else {
            let current = self.current_index();
            self.order = (0..self.slots.len()).collect();
            self.at = current;
        }
    }

    /// Adds entries at the end (with shuffle, after the current one, in random places among
    /// the ones not played yet). Returns their keys.
    pub fn add(&mut self, items: &[i64]) -> Vec<u64> {
        let mut keys = Vec::with_capacity(items.len());
        for &item_id in items {
            let key = self.next_key;
            self.next_key += 1;
            self.slots.push(Slot { key, item_id });
            let index = self.slots.len() - 1;
            if self.shuffle
                && let Some(at) = self.at
            {
                self.seed = splitmix(self.seed);
                let room = self.order.len() - at;
                let place = at + 1 + (self.seed % room as u64) as usize;
                self.order.insert(place.min(self.order.len()), index);
            } else {
                self.order.push(index);
            }
            if self.at.is_none() {
                self.at = Some(0);
            }
            keys.push(key);
        }
        keys
    }

    /// Removes the entry with `key`. Returns whether it was there and whether it was the
    /// current one; then the entry after it (or none) is current.
    pub fn remove(&mut self, key: u64) -> (bool, bool) {
        let Some(index) = self.slots.iter().position(|slot| slot.key == key) else {
            return (false, false);
        };
        let was_current = self.current_index() == Some(index);
        let place = self.order.iter().position(|&i| i == index).unwrap_or(0);
        self.slots.remove(index);
        self.order.remove(place);
        for i in &mut self.order {
            if *i > index {
                *i -= 1;
            }
        }
        self.at = match self.at {
            _ if self.order.is_empty() => None,
            Some(at) if place < at => Some(at - 1),
            // The current one went: the next one takes its place (or the queue is over).
            Some(at) if place == at => (at < self.order.len()).then_some(at),
            other => other,
        };
        (true, was_current)
    }

    /// Moves the entry with `key` to `to` in the queue's own order (without shuffle the
    /// play order follows). False when there is no such entry.
    pub fn move_to(&mut self, key: u64, to: usize) -> bool {
        let Some(from) = self.slots.iter().position(|slot| slot.key == key) else {
            return false;
        };
        let current = self.current().map(|slot| slot.key);
        // The play order by key, so it survives the move.
        let play: Vec<u64> = self.order.iter().map(|&i| self.slots[i].key).collect();
        let slot = self.slots.remove(from);
        let to = to.min(self.slots.len());
        self.slots.insert(to, slot);
        self.order = if self.shuffle {
            play.iter()
                .filter_map(|key| self.slots.iter().position(|slot| slot.key == *key))
                .collect()
        } else {
            (0..self.slots.len()).collect()
        };
        self.at = current.and_then(|current| {
            self.order
                .iter()
                .position(|&i| self.slots[i].key == current)
        });
        true
    }

    /// Whether `next` (a skip) has somewhere to go: a later entry, or round again with repeat.
    pub fn has_next(&self) -> bool {
        match self.at {
            Some(at) => at + 1 < self.order.len() || self.repeat != Repeat::Off,
            None => false,
        }
    }

    /// Whether `previous` can go to another entry (an earlier one, or the last with repeat
    /// all); otherwise it starts the current one over.
    pub fn has_previous(&self) -> bool {
        match self.at {
            Some(at) => at > 0 || (self.repeat == Repeat::All && self.order.len() > 1),
            None => false,
        }
    }

    /// Back to the first entry of the play order.
    pub fn restart(&mut self) {
        if !self.order.is_empty() {
            self.at = Some(0);
        }
    }

    /// Clears the queue.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.at = None;
    }

    /// The current entry first, the rest after it in random order.
    fn shuffle_from_current(&mut self) {
        let current = self.current_index().unwrap_or(0);
        let mut rest: Vec<usize> = (0..self.slots.len()).filter(|&i| i != current).collect();
        shuffle_in_place(&mut rest, self.seed);
        self.order = std::iter::once(current).chain(rest).collect();
        self.at = Some(0);
    }
}

/// splitmix64: a small, good mixer, so shuffles need no random number crate.
fn splitmix(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Fisher–Yates with a seed: every order equally likely, the same seed the same order.
fn shuffle_in_place(items: &mut [usize], seed: u64) {
    let mut state = seed;
    for i in (1..items.len()).rev() {
        state = splitmix(state);
        let j = (state % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn items(order: &mut Order, auto: bool, steps: usize) -> Vec<i64> {
        let mut played = vec![order.current().unwrap().item_id];
        for _ in 0..steps {
            match order.next(auto) {
                Some(slot) => played.push(slot.item_id),
                None => break,
            }
        }
        played
    }

    #[test]
    fn plays_in_order_and_ends() {
        let mut order = Order::new(1);
        assert_eq!(order.set(&[10, 20, 30], 0).unwrap().item_id, 10);
        assert_eq!(items(&mut order, true, 10), [10, 20, 30]);
        assert_eq!(order.next(true), None, "stays over");
        let mut order = Order::new(1);
        order.set(&[10, 20, 30], 1);
        assert_eq!(items(&mut order, true, 10), [20, 30]);
        assert_eq!(
            order.set(&[10, 20], 99).unwrap().item_id,
            20,
            "a start past the end is the last"
        );
        assert_eq!(order.set(&[], 0), None);
        assert_eq!(order.next(true), None);
        assert_eq!(order.previous(), None);
    }

    #[test]
    fn repeat_one_replays_but_skips_move_on() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 0);
        order.repeat = Repeat::One;
        assert_eq!(items(&mut order, true, 3), [1, 1, 1, 1]);
        assert_eq!(order.next(false).unwrap().item_id, 2, "skipping moves on");
        order.next(false);
        assert_eq!(
            order.next(false).unwrap().item_id,
            1,
            "skipping past the end goes round"
        );
    }

    #[test]
    fn repeat_all_goes_round() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 0);
        order.repeat = Repeat::All;
        assert_eq!(items(&mut order, true, 6), [1, 2, 3, 1, 2, 3, 1]);
        assert_eq!(
            order.previous().unwrap().item_id,
            3,
            "back from the first: the last"
        );
    }

    #[test]
    fn previous_stops_at_the_first_without_repeat() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 1);
        assert_eq!(order.previous().unwrap().item_id, 1);
        assert_eq!(order.previous().unwrap().item_id, 1);
    }

    #[test]
    fn shuffle_plays_everything_once_starting_with_the_current() {
        for seed in 0..200 {
            let mut order = Order::new(seed);
            order.set_shuffle(true);
            let queue: Vec<i64> = (1..=25).collect();
            assert_eq!(
                order.set(&queue, 7).unwrap().item_id,
                8,
                "seed {seed}: the chosen one first"
            );
            let played = items(&mut order, true, 100);
            assert_eq!(played.len(), 25, "seed {seed}");
            assert_eq!(
                played.iter().collect::<HashSet<_>>().len(),
                25,
                "seed {seed}: none twice"
            );
        }
        // The same seed, the same order; another seed, (nearly always) another.
        let run = |seed| {
            let mut order = Order::new(seed);
            order.set_shuffle(true);
            order.set(&(1..=20).collect::<Vec<_>>(), 0);
            items(&mut order, true, 30)
        };
        assert_eq!(run(42), run(42));
        assert_ne!(run(42), run(43));
    }

    #[test]
    fn shuffle_with_repeat_all_reshuffles_and_never_repeats_at_the_seam() {
        for seed in 0..100 {
            let mut order = Order::new(seed);
            order.set_shuffle(true);
            order.repeat = Repeat::All;
            order.set(&[1, 2, 3, 4], 0);
            let played = items(&mut order, true, 4 * 6 - 1);
            for round in played.chunks(4) {
                assert_eq!(
                    round.iter().collect::<HashSet<_>>().len(),
                    round.len(),
                    "seed {seed}: {played:?}"
                );
            }
            assert!(
                played.windows(2).all(|pair| pair[0] != pair[1]),
                "seed {seed}: {played:?}"
            );
        }
        // Two entries: still alternating, one entry: itself again.
        let mut order = Order::new(5);
        order.set_shuffle(true);
        order.repeat = Repeat::All;
        order.set(&[1], 0);
        assert_eq!(items(&mut order, true, 3), [1, 1, 1, 1]);
    }

    #[test]
    fn turning_shuffle_on_and_off_keeps_the_current_entry() {
        let mut order = Order::new(9);
        order.set(&[1, 2, 3, 4, 5, 6], 2);
        order.set_shuffle(true);
        assert_eq!(order.current().unwrap().item_id, 3);
        let rest = items(&mut order, true, 10);
        assert_eq!(rest[0], 3);
        assert_eq!(
            rest.iter().collect::<HashSet<_>>().len(),
            6,
            "all of them once"
        );
        // Off: the queue's own order from where it is.
        let mut order = Order::new(9);
        order.set(&[1, 2, 3, 4, 5, 6], 0);
        order.set_shuffle(true);
        let now = order.next(true).unwrap().item_id;
        order.set_shuffle(false);
        assert_eq!(order.current().unwrap().item_id, now);
        let after: Vec<i64> = items(&mut order, true, 10);
        let expected: Vec<i64> = (now..=6).collect();
        assert_eq!(after, expected);
    }

    #[test]
    fn the_same_item_twice_has_two_keys() {
        let mut order = Order::new(1);
        order.set(&[7, 7, 7], 0);
        let keys: HashSet<u64> = order.slots().iter().map(|slot| slot.key).collect();
        assert_eq!(keys.len(), 3);
        let more = order.add(&[7]);
        assert!(!keys.contains(&more[0]));
    }

    #[test]
    fn adding_appends_or_lands_after_the_current_when_shuffled() {
        let mut order = Order::new(1);
        order.add(&[1, 2]);
        assert_eq!(
            order.current().unwrap().item_id,
            1,
            "adding to an empty queue starts it"
        );
        order.add(&[3]);
        assert_eq!(items(&mut order, true, 5), [1, 2, 3]);
        for seed in 0..50 {
            let mut order = Order::new(seed);
            order.set_shuffle(true);
            order.set(&[1, 2, 3, 4], 0);
            order.next(true);
            order.add(&[99]);
            let rest = items(&mut order, true, 10);
            assert!(
                rest.contains(&99),
                "seed {seed}: the new one comes before the end"
            );
            assert_eq!(rest.len(), 4, "seed {seed}: {rest:?}");
        }
    }

    #[test]
    fn removing_the_current_entry_makes_the_next_current() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 1);
        let key = order.current().unwrap().key;
        assert_eq!(order.remove(key), (true, true));
        assert_eq!(order.current().unwrap().item_id, 3);
        let first = order.slots()[0].key;
        assert_eq!(order.remove(first), (true, false));
        assert_eq!(
            order.current().unwrap().item_id,
            3,
            "removing an earlier one keeps the current"
        );
        let last = order.current().unwrap().key;
        assert_eq!(order.remove(last), (true, true));
        assert_eq!(order.current(), None, "nothing left");
        assert_eq!(order.remove(12345), (false, false));
        // The last one current, removed: the queue is over, earlier ones stay.
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 2);
        let key = order.current().unwrap().key;
        order.remove(key);
        assert_eq!((order.current(), order.slots().len()), (None, 2));
    }

    #[test]
    fn moving_keeps_what_is_playing() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3, 4], 1);
        let last = order.slots()[3].key;
        assert!(order.move_to(last, 0));
        let ids: Vec<i64> = order.slots().iter().map(|slot| slot.item_id).collect();
        assert_eq!(ids, [4, 1, 2, 3]);
        assert_eq!(order.current().unwrap().item_id, 2);
        assert_eq!(items(&mut order, true, 5), [2, 3]);
        assert!(!order.move_to(999, 0));
        // Shuffled: the play order and the current entry stay; only the list changes.
        let mut order = Order::new(3);
        order.set_shuffle(true);
        order.set(&[1, 2, 3, 4, 5], 0);
        let before: Vec<i64> = {
            let mut copy = order.clone();
            items(&mut copy, true, 10)
        };
        let key = order.slots()[4].key;
        order.move_to(key, 1);
        assert_eq!(items(&mut order, true, 10), before);
    }

    #[test]
    fn jump_goes_to_an_entry() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 0);
        let key = order.slots()[2].key;
        assert!(order.jump(key));
        assert_eq!(order.current().unwrap().item_id, 3);
        assert!(!order.jump(999));
    }

    /// Every entry exactly once from where playback is to the end.
    fn rest_once(order: &mut Order) -> Vec<i64> {
        let mut copy = order.clone();
        copy.repeat = Repeat::Off;
        items(&mut copy, true, 10_000)
    }

    #[test]
    fn shuffle_toggled_again_and_again_never_loses_an_entry() {
        let mut order = Order::new(77);
        order.set(&(1..=12).collect::<Vec<_>>(), 4);
        for flip in 0..40 {
            let current = order.current().unwrap().item_id;
            order.set_shuffle(flip % 2 == 0);
            assert_eq!(
                order.current().unwrap().item_id,
                current,
                "flip {flip}: the current one stays"
            );
            assert_eq!(order.slots().len(), 12);
            let mut all: Vec<usize> = order.order.clone();
            all.sort_unstable();
            assert_eq!(
                all,
                (0..12).collect::<Vec<_>>(),
                "flip {flip}: a full play order"
            );
            if flip % 3 == 0 {
                order.next(false);
            }
        }
    }

    #[test]
    fn removing_while_shuffled_keeps_the_current_and_plays_the_rest_once() {
        for seed in 0..60 {
            let mut order = Order::new(seed);
            order.set_shuffle(true);
            order.set(&(1..=8).collect::<Vec<_>>(), 0);
            order.next(true);
            order.next(true);
            let current = order.current().unwrap();
            // Remove one already played and one still to come.
            let played = order.slots()[order.order[0]].key;
            let ahead = order.slots()[order.order[5]].key;
            let ahead_item = order.slots()[order.order[5]].item_id;
            assert_eq!(order.remove(played), (true, false));
            assert_eq!(order.remove(ahead), (true, false));
            assert_eq!(order.current(), Some(current), "seed {seed}");
            let rest = rest_once(&mut order);
            assert!(
                !rest.contains(&ahead_item),
                "seed {seed}: the removed one does not play"
            );
            assert_eq!(rest.len(), 5, "seed {seed}: {rest:?}");
            assert_eq!(rest.iter().collect::<HashSet<_>>().len(), 5);
        }
    }

    #[test]
    fn adding_while_shuffled_at_any_point() {
        // An empty shuffled queue: what is added plays.
        let mut order = Order::new(3);
        order.set_shuffle(true);
        order.add(&[5, 6]);
        assert!(order.current().is_some());
        assert_eq!(rest_once(&mut order).len(), 2);
        // The current entry is the last in the play order: the new one comes next.
        let mut order = Order::new(3);
        order.set_shuffle(true);
        order.set(&[1, 2, 3], 0);
        order.next(true);
        order.next(true);
        order.add(&[99]);
        assert_eq!(order.next(true).unwrap().item_id, 99);
    }

    #[test]
    fn moving_to_odd_places() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 0);
        let key = order.slots()[0].key;
        assert!(order.move_to(key, 0), "to where it is");
        assert!(order.move_to(key, 1000), "past the end: to the end");
        let ids: Vec<i64> = order.slots().iter().map(|slot| slot.item_id).collect();
        assert_eq!(ids, [2, 3, 1]);
        assert_eq!(
            order.current().unwrap().item_id,
            1,
            "the current one moved with it"
        );
        assert_eq!(
            rest_once(&mut order),
            [1],
            "and plays on from its new place"
        );
    }

    #[test]
    fn jumping_while_shuffled_follows_the_play_order() {
        let mut order = Order::new(8);
        order.set_shuffle(true);
        order.set(&(1..=6).collect::<Vec<_>>(), 0);
        let target = order.slots()[order.order[3]];
        assert!(order.jump(target.key));
        assert_eq!(order.current(), Some(target));
        let after = rest_once(&mut order);
        assert_eq!(
            after.len(),
            3,
            "the play order goes on from there: {after:?}"
        );
    }

    #[test]
    fn restart_and_previous_with_repeat_one() {
        let mut order = Order::new(1);
        order.set(&[1, 2, 3], 2);
        order.restart();
        assert_eq!(order.current().unwrap().item_id, 1);
        order.repeat = Repeat::One;
        order.next(false);
        assert_eq!(
            order.previous().unwrap().item_id,
            1,
            "previous ignores repeat one"
        );
        let mut empty = Order::new(1);
        empty.restart();
        assert_eq!(empty.current(), None);
    }

    #[test]
    fn next_and_previous_say_where_they_can_go() {
        let mut order = Order::new(1);
        assert!(!order.has_next() && !order.has_previous(), "empty");
        order.set(&[1, 2, 3], 0);
        assert!(order.has_next() && !order.has_previous());
        order.next(false);
        assert!(order.has_next() && order.has_previous());
        order.next(false);
        assert!(
            !order.has_next() && order.has_previous(),
            "the last, no repeat"
        );
        order.repeat = Repeat::All;
        assert!(order.has_next(), "repeat all goes round");
        order.repeat = Repeat::One;
        assert!(
            order.has_next(),
            "a skip with repeat one still moves on (round)"
        );
        order.restart();
        assert!(
            !order.has_previous(),
            "the first, repeat one: previous starts it over"
        );
        order.repeat = Repeat::All;
        assert!(order.has_previous(), "the first, repeat all: to the last");
        let mut single = Order::new(1);
        single.set(&[1], 0);
        single.repeat = Repeat::All;
        assert!(!single.has_previous(), "one entry: only itself");
        assert!(single.has_next());
    }
}
