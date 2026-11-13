//! Shared event queue for the push path: producers enqueue out-of-band events;
//! readers drain the ones relevant to them at hook points. Internally
//! synchronized and `Arc`-shared (clones share one queue), mirroring
//! [`crate::buffer::InterjectionBuffer`].

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Occurrence(u64);

impl Occurrence {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

static NEXT_OCCURRENCE: AtomicU64 = AtomicU64::new(1);

/// Host-only process-wide arrival or due order.
pub fn next_occurrence() -> Occurrence {
    Occurrence(
        NEXT_OCCURRENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .expect("host occurrence space exhausted"),
    )
}

#[derive(Debug, Clone)]
struct Slot<E> {
    occ: Occurrence,
    pin: Option<Occurrence>,
    event: E,
}

impl<E> Slot<E> {
    fn new(event: E) -> Self {
        Self {
            occ: next_occurrence(),
            pin: None,
            event,
        }
    }
}

#[derive(Debug)]
pub struct EventQueue<E> {
    events: Arc<Mutex<Vec<Slot<E>>>>,
}

impl<E> Clone for EventQueue<E> {
    fn clone(&self) -> Self {
        Self {
            events: Arc::clone(&self.events),
        }
    }
}

impl<E> Default for EventQueue<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> EventQueue<E> {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Producer hook: record an event for later draining.
    pub fn push(&self, event: E) {
        self.lock().push(Slot::new(event));
    }

    /// Push, then retain at most `max` unpinned events; pins are never victims.
    pub fn push_capped(&self, event: E, max: usize) {
        let mut q = self.lock();
        q.push(Slot::new(event));
        let mut excess = q
            .iter()
            .filter(|slot| slot.pin.is_none())
            .count()
            .saturating_sub(max);
        q.retain(|slot| {
            if excess > 0 && slot.pin.is_none() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Remove and return events matching `take`, retaining the rest. FIFO order
    /// is preserved in both the returned and retained sets.
    pub fn drain_matching(&self, take: impl Fn(&E) -> bool) -> Vec<E> {
        let mut q = self.lock();
        let (matched, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *q)
            .into_iter()
            .partition(|slot| take(&slot.event));
        *q = kept;
        matched.into_iter().map(|slot| slot.event).collect()
    }

    /// Remove and return all events, leaving the queue empty (FIFO order).
    pub fn drain_all(&self) -> Vec<E> {
        std::mem::take(&mut *self.lock())
            .into_iter()
            .map(|slot| slot.event)
            .collect()
    }

    /// Splice `events` back in ahead of everything currently queued, preserving their
    /// order. For cancel-safety: a reader whose async processing was aborted mid-drain
    /// restores the unprocessed remainder, which arrived before anything pushed since.
    pub fn restore_front(&self, events: Vec<E>) {
        if events.is_empty() {
            return;
        }
        let mut q = self.lock();
        let restored = events.into_iter().map(Slot::new).collect();
        let tail = std::mem::replace(&mut *q, restored);
        q.extend(tail);
    }

    /// Discard all events.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Slot<E>>> {
        self.events.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<E: Clone> EventQueue<E> {
    /// Clone of the current events, for inspection without draining.
    pub fn snapshot(&self) -> Vec<E> {
        self.lock().iter().map(|slot| slot.event.clone()).collect()
    }
}

impl<E> EventQueue<E> {
    pub fn drain_all_with_occurrences(&self) -> Vec<(Occurrence, E)> {
        self.drain_slots(|_| true)
    }

    pub fn restore_front_with_occurrences(&self, events: Vec<(Occurrence, E)>) {
        if events.is_empty() {
            return;
        }
        let mut q = self.lock();
        let restored = events
            .into_iter()
            .map(|(occ, event)| Slot {
                occ,
                pin: None,
                event,
            })
            .collect();
        let tail = std::mem::replace(&mut *q, restored);
        q.extend(tail);
    }

    pub fn pin_matching(
        &self,
        owner: Occurrence,
        take: impl Fn(&E) -> bool,
        mut admit: impl FnMut(Occurrence, &E) -> bool,
    ) -> Vec<Occurrence> {
        let mut q = self.lock();
        let mut pinned = Vec::new();
        for slot in &mut *q {
            if slot.pin.is_none() && take(&slot.event) && admit(slot.occ, &slot.event) {
                slot.pin = Some(owner);
                pinned.push(slot.occ);
            }
        }
        pinned
    }

    pub fn drain_unpinned_matching(&self, take: impl Fn(&E) -> bool) -> Vec<E> {
        self.drain_slots(|slot| slot.pin.is_none() && take(&slot.event))
            .into_iter()
            .map(|(_, event)| event)
            .collect()
    }

    pub fn take_for_release(
        &self,
        owner: Occurrence,
        take: impl Fn(&E) -> bool,
    ) -> Vec<(Occurrence, E)> {
        self.drain_slots(|slot| {
            slot.pin == Some(owner) || (slot.pin.is_none() && take(&slot.event))
        })
    }

    pub fn discard_pinned(
        &self,
        owner: Occurrence,
        take: impl Fn(&E) -> bool,
    ) -> Vec<(Occurrence, E)> {
        self.drain_slots(|slot| slot.pin == Some(owner) && take(&slot.event))
    }

    pub fn unpin_all(&self, owner: Occurrence) -> usize {
        let mut q = self.lock();
        let mut count = 0;
        for slot in &mut *q {
            if slot.pin == Some(owner) {
                slot.pin = None;
                count += 1;
            }
        }
        count
    }

    /// Holds queue authority until the caller commits only submitted keys.
    pub fn release_view(
        &self,
        owner: Occurrence,
        take: impl Fn(&E) -> bool,
    ) -> EventReleaseView<'_, E> {
        let queue = self.lock();
        let selected = queue
            .iter()
            .filter(|slot| slot.pin == Some(owner) || (slot.pin.is_none() && take(&slot.event)))
            .map(|slot| slot.occ)
            .collect();
        EventReleaseView { queue, selected }
    }

    fn drain_slots(&self, take: impl Fn(&Slot<E>) -> bool) -> Vec<(Occurrence, E)> {
        let mut q = self.lock();
        let (matched, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *q).into_iter().partition(take);
        *q = kept;
        matched
            .into_iter()
            .map(|slot| (slot.occ, slot.event))
            .collect()
    }
}

pub struct EventReleaseView<'a, E> {
    queue: MutexGuard<'a, Vec<Slot<E>>>,
    selected: Vec<Occurrence>,
}

impl<E> EventReleaseView<'_, E> {
    pub fn iter(&self) -> impl Iterator<Item = (Occurrence, &E)> {
        self.queue
            .iter()
            .filter(|slot| self.selected.contains(&slot.occ))
            .map(|slot| (slot.occ, &slot.event))
    }

    pub fn commit_occurrences(&mut self, submitted: &[Occurrence]) {
        self.queue
            .retain(|slot| !(self.selected.contains(&slot.occ) && submitted.contains(&slot.occ)));
        self.selected.retain(|key| !submitted.contains(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_len() {
        let q: EventQueue<u32> = EventQueue::new();
        assert!(q.is_empty());
        q.push(1);
        q.push(2);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn clones_share_one_queue() {
        let q: EventQueue<u32> = EventQueue::new();
        let q2 = q.clone();
        q.push(7);
        assert_eq!(q2.len(), 1);
    }

    #[test]
    fn push_capped_drops_oldest() {
        let q: EventQueue<u32> = EventQueue::new();
        for i in 0..5 {
            q.push_capped(i, 3);
        }
        assert_eq!(q.drain_matching(|_| true), vec![2, 3, 4]);
        assert!(q.is_empty());
    }

    #[test]
    fn drain_matching_returns_matched_retains_rest_fifo() {
        let q: EventQueue<u32> = EventQueue::new();
        for i in 0..6 {
            q.push(i);
        }
        let evens = q.drain_matching(|n| n % 2 == 0);
        assert_eq!(evens, vec![0, 2, 4]);
        assert_eq!(q.drain_matching(|_| true), vec![1, 3, 5]);
    }

    #[test]
    fn push_capped_under_limit_keeps_all() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push_capped(1, 5);
        q.push_capped(2, 5);
        assert_eq!(q.drain_matching(|_| true), vec![1, 2]);
    }

    #[test]
    fn drain_matching_none_match_retains_all() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.push(2);
        assert!(q.drain_matching(|n| *n > 10).is_empty());
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn drain_matching_on_empty_is_empty() {
        let q: EventQueue<u32> = EventQueue::new();
        assert!(q.drain_matching(|_| true).is_empty());
    }

    #[test]
    fn drain_all_empties_in_fifo_order() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.push(2);
        assert_eq!(q.drain_all(), vec![1, 2]);
        assert!(q.is_empty());
    }

    #[test]
    fn restore_front_reinserts_ahead_of_later_pushes() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.push(2);
        let drained = q.drain_all();
        q.push(3);
        q.restore_front(drained);
        assert_eq!(q.drain_all(), vec![1, 2, 3]);
    }

    #[test]
    fn clear_discards_all() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(1);
        q.clear();
        assert!(q.is_empty());
    }

    #[test]
    fn snapshot_reads_without_draining() {
        let q: EventQueue<u32> = EventQueue::new();
        q.push(9);
        assert_eq!(q.snapshot(), vec![9]);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn occurrences_unique_and_ordered_across_queues() {
        let first = EventQueue::new();
        let second = EventQueue::new();
        first.push(7);
        second.push(7);
        first.push(7);
        let a = first.drain_all_with_occurrences();
        let b = second.drain_all_with_occurrences();
        assert!(
            a[0].0 < b[0].0 && b[0].0 < a[1].0,
            "U61_GLOBAL_OCCURRENCE_ORDER"
        );
        assert_ne!(a[0].0, a[1].0, "U61_EQUAL_PAYLOAD_DISTINCT_KEYS");
        assert_eq!(
            a.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec![7, 7]
        );
        assert_eq!(b[0].1, 7);
    }

    #[test]
    fn restore_with_occurrences_preserves_keys_and_order() {
        let q = EventQueue::new();
        q.push(1);
        q.push(2);
        let drained = q.drain_all_with_occurrences();
        q.push(3);
        let tail = {
            let view = q.release_view(next_occurrence(), |_| true);
            view.iter()
                .map(|(key, value)| (key, *value))
                .collect::<Vec<_>>()
        };
        q.restore_front_with_occurrences(drained.clone());
        let mut expected = drained;
        expected.extend(tail);
        assert_eq!(
            q.drain_all_with_occurrences(),
            expected,
            "U61_KEYED_RESTORE"
        );
    }

    #[test]
    fn push_capped_never_evicts_pinned() {
        let q = EventQueue::new();
        q.push(1);
        let owner = next_occurrence();
        let pinned = q.pin_matching(owner, |_| true, |_, _| true);
        q.push_capped(2, 1);
        q.push_capped(3, 1);
        let view = q.release_view(owner, |_| true);
        assert!(
            view.iter().any(|(key, _)| key == pinned[0]),
            "U61_PIN_AUTHORITY_EVICTED"
        );
        assert_eq!(
            view.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn push_capped_counts_only_unpinned() {
        let q = EventQueue::new();
        for value in 0..50 {
            q.push(value);
        }
        let owner = next_occurrence();
        assert_eq!(q.pin_matching(owner, |_| true, |_, _| true).len(), 50);
        for value in 50..100 {
            q.push_capped(value, 50);
        }
        assert_eq!(q.len(), 100, "U61_UNPINNED_HEADROOM");
        q.push_capped(100, 50);
        let mut expected: Vec<_> = (0..50).collect();
        expected.extend(51..101);
        assert_eq!(q.snapshot(), expected, "U61_ONLY_OLDEST_UNPINNED_EVICTED");
    }

    #[test]
    fn drain_matching_still_removes_pinned() {
        let q = EventQueue::new();
        q.push(7);
        q.push(8);
        let owner = next_occurrence();
        q.pin_matching(owner, |value| *value == 7, |_, _| true);
        assert_eq!(
            q.drain_matching(|value| *value == 7),
            vec![7],
            "U61_EXPLICIT_DROP_AUTHORITY"
        );
        assert_eq!(q.snapshot(), vec![8]);
        assert_eq!(q.unpin_all(owner), 0);
    }

    #[test]
    fn unpinned_drain_skips_pinned() {
        let q = EventQueue::new();
        q.push(7);
        q.push(8);
        let owner = next_occurrence();
        let keys = q.pin_matching(owner, |value| *value == 7, |_, _| true);
        assert_eq!(
            q.drain_unpinned_matching(|_| true),
            vec![8],
            "U61_UNPINNED_DRAIN_AUTHORITY"
        );
        let mut view = q.release_view(owner, |_| true);
        assert_eq!(
            view.iter()
                .map(|(key, value)| (key, *value))
                .collect::<Vec<_>>(),
            vec![(keys[0], 7)]
        );
        view.commit_occurrences(&keys);
        assert_eq!(view.iter().count(), 0);
        drop(view);
        assert!(q.is_empty());
    }

    #[test]
    fn pins_are_owner_scoped() {
        for operation in ["take", "unpin", "discard"] {
            let q = EventQueue::new();
            q.push(7);
            q.push(8);
            let owner = next_occurrence();
            let foreign = next_occurrence();
            let keys = q.pin_matching(owner, |_| true, |_, _| true);
            {
                let mut foreign_view = q.release_view(foreign, |_| true);
                assert_eq!(foreign_view.iter().count(), 0, "U61_FOREIGN_RELEASE_VIEW");
                foreign_view.commit_occurrences(&keys);
            }
            match operation {
                "take" => assert!(
                    q.take_for_release(foreign, |_| true).is_empty(),
                    "U61_FOREIGN_TAKE"
                ),
                "unpin" => assert_eq!(q.unpin_all(foreign), 0, "U61_FOREIGN_UNPIN"),
                "discard" => assert!(
                    q.discard_pinned(foreign, |_| true).is_empty(),
                    "U61_FOREIGN_DISCARD"
                ),
                _ => unreachable!(),
            }
            assert_eq!(q.snapshot(), vec![7, 8], "U61_FOREIGN_AUTHORITY_UNCHANGED");
            {
                let view = q.release_view(owner, |_| false);
                assert_eq!(view.iter().count(), 2, "U61_OWNER_RELEASE_VIEW");
            }
            assert_eq!(q.snapshot(), vec![7, 8], "U61_RELEASE_VIEW_NONDESTRUCTIVE");
            {
                let mut view = q.release_view(owner, |_| false);
                view.commit_occurrences(&keys[..1]);
                assert_eq!(
                    view.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
                    vec![8]
                );
            }
            assert_eq!(q.unpin_all(owner), 1, "U61_OWNER_UNPIN");
            assert_eq!(q.drain_all_with_occurrences(), vec![(keys[1], 8)]);
        }
    }
}
