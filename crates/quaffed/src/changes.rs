// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! A changed file held as the changes each edit made to it, not as a whole copy per edit.
//!
//! **An edit is held as its splices**: each stretch of the file as it stood that the edit wrote
//! over, and what it wrote there.  A run holds every changed file until it ends, so that every
//! edit is written or none is, and a whole copy of each would double what it holds; the splices
//! are what the edits wrote, usually a small part of the file.  The file's bytes after any edit
//! are built from its bytes as read and the changes before it, when something needs them.
//!
//! **What an edit says it wrote over is checked, not trusted.**  [`Changes::between`] takes the
//! stretches an edit reports and checks that what lies between them is the same before and after;
//! any that is not is taken as changed too.  So a wrong report can only make the changes larger,
//! never wrong: applying them always gives exactly the bytes the edit made.

use std::ops::Range;

/// One stretch of a file's bytes written over: where, in the bytes before the edit, and what
/// stands there after it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Splice {
    at: Range<usize>,
    with: Box<[u8]>,
}

/// The splices one edit made to the bytes before it, in order and none overlapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    splices: Vec<Splice>,
}

impl Changes {
    /// The changes that make `old` into `new`, from `claimed`: each pair a stretch of `old` an
    /// edit wrote over and the stretch of `new` it wrote, in order.  What lies between two claimed
    /// stretches, and before the first and after the last, is compared in `old` and `new`, and
    /// taken as changed where it differs, so that applying the result to `old` always gives `new`.
    /// A claim out of order, or outside either text, ends the claims there: the rest is compared
    /// as one stretch.
    pub fn between(old: &[u8], new: &[u8], claimed: &[(Range<usize>, Range<usize>)]) -> Changes {
        let mut splices: Vec<Splice> = Vec::new();
        // Where the stretch not yet accounted for starts, in each.
        let (mut at_old, mut at_new) = (0, 0);
        // Where a run of changed stretches started, in each, while one is open: claims with
        // nothing unchanged between them make one splice.
        let mut open: Option<(usize, usize)> = None;
        let mut close = |open: &mut Option<(usize, usize)>, old_end: usize, new_end: usize| {
            if let Some((old_start, new_start)) = open.take() {
                splices.push(Splice {
                    at: old_start..old_end,
                    with: new[new_start..new_end].into(),
                });
            }
        };
        for (was, now) in claimed {
            let in_order = at_old <= was.start
                && was.start <= was.end
                && was.end <= old.len()
                && at_new <= now.start
                && now.start <= now.end
                && now.end <= new.len();
            if !in_order {
                break;
            }
            // The stretch before the claim is unchanged only if it is the same in both; if it is
            // not, it joins the changes either side of it.
            if old[at_old..was.start] == new[at_new..now.start] {
                close(&mut open, at_old, at_new);
                open = Some((was.start, now.start));
            } else {
                open.get_or_insert((at_old, at_new));
            }
            at_old = was.end;
            at_new = now.end;
        }
        if old[at_old..] == new[at_new..] {
            close(&mut open, at_old, at_new);
        } else {
            open.get_or_insert((at_old, at_new));
            close(&mut open, old.len(), new.len());
        }
        // A claimed stretch written back as it was changes nothing, and is not held.
        splices.retain(|splice| old[splice.at.clone()] != *splice.with);
        Changes { splices }
    }

    /// `old`, the bytes these changes were made to, with every splice written in.
    pub fn apply(&self, old: &[u8]) -> Vec<u8> {
        let mut new = Vec::with_capacity(old.len() + self.growth());
        let mut copied = 0;
        for splice in &self.splices {
            new.extend_from_slice(&old[copied..splice.at.start]);
            new.extend_from_slice(&splice.with);
            copied = splice.at.end;
        }
        new.extend_from_slice(&old[copied..]);
        new
    }

    /// How many bytes the splices hold: what an edit costs to keep, besides the file it changed.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.splices.iter().map(|splice| splice.with.len()).sum()
    }

    /// How many bytes longer the changes make what they are applied to, or none if shorter.
    fn growth(&self) -> usize {
        let written: usize = self.splices.iter().map(|splice| splice.with.len()).sum();
        let over: usize = self.splices.iter().map(|splice| splice.at.len()).sum();
        written.saturating_sub(over)
    }
}

/// `original` with every one of `edits` applied in turn: the file after the last of them.
pub fn applied(original: &[u8], edits: &[Changes]) -> Vec<u8> {
    let Some((first, rest)) = edits.split_first() else {
        return original.to_vec();
    };
    rest.iter().fold(first.apply(original), |bytes, changes| {
        changes.apply(&bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn claims_that_hold_keep_only_what_was_written() {
        let old = b"one two three two one";
        let new = b"one 2 three 2 one";
        let changes = Changes::between(old, new, &[(4..7, 4..5), (14..17, 12..13)]);
        assert_eq!(changes.apply(old), new);
        assert_eq!(changes.held(), 2);
        assert_eq!(changes.splices.len(), 2);
    }

    #[test]
    fn a_stretch_claimed_unchanged_that_changed_is_taken_as_changed() {
        let old = b"aaaa bbbb cccc";
        let new = b"aaaa XXXX cccQ";
        // Only the middle is claimed; the end changed too.
        let changes = Changes::between(old, new, &[(5..9, 5..9)]);
        assert_eq!(changes.apply(old), new);
        assert_eq!(changes.splices.len(), 1, "{changes:?}");
    }

    #[test]
    fn no_claims_take_the_whole_difference_as_one_change() {
        let changes = Changes::between(b"before", b"after", &[]);
        assert_eq!(changes.apply(b"before"), b"after");
        assert_eq!(changes.held(), 5);
    }

    #[test]
    fn a_claim_out_of_order_ends_the_claims_and_the_rest_still_applies() {
        let old = b"0123456789";
        let new = b"0X23456Y89";
        let changes = Changes::between(old, new, &[(7..8, 7..8), (1..2, 1..2)]);
        assert_eq!(changes.apply(old), new);
    }

    #[test]
    fn writing_a_stretch_back_as_it_was_holds_nothing() {
        let changes = Changes::between(b"same", b"same", &[(0..4, 0..4)]);
        assert_eq!(changes, Changes::default());
        assert_eq!(changes.apply(b"same"), b"same");
    }

    #[test]
    fn edits_in_turn_give_the_last_version() {
        let first = Changes::between(b"a b c", b"a B c", &[(2..3, 2..3)]);
        let second = Changes::between(b"a B c", b"a B", &[(3..5, 3..3)]);
        assert_eq!(applied(b"a b c", &[first, second]), b"a B");
        assert_eq!(applied(b"a b c", &[]), b"a b c");
    }

    /// A claim drawn from anywhere, in or out of range or order.
    fn claim() -> impl Strategy<Value = (Range<usize>, Range<usize>)> {
        (0usize..40, 0usize..8, 0usize..40, 0usize..8)
            .prop_map(|(a, al, b, bl)| (a..a + al, b..b + bl))
    }

    proptest! {
        /// Whatever an edit claims, the changes give exactly the bytes it made.
        #[test]
        fn any_claims_give_the_new_bytes(
            old in prop::collection::vec(0u8..4, 0..32),
            new in prop::collection::vec(0u8..4, 0..32),
            claims in prop::collection::vec(claim(), 0..6),
        ) {
            let changes = Changes::between(&old, &new, &claims);
            prop_assert_eq!(changes.apply(&old), new);
        }

        /// Claims that are true hold no more than what was written over them.
        #[test]
        fn true_claims_hold_only_what_was_written(
            old in prop::collection::vec(0u8..4, 0..32),
            edits in prop::collection::vec(
                (0usize..32, 0usize..4, prop::collection::vec(4u8..8, 0..4)),
                0..5,
            ),
        ) {
            // Non-overlapping stretches of `old`, in order, each written over with bytes that
            // cannot occur in it.
            let mut stretches: Vec<(usize, usize, Vec<u8>)> = Vec::new();
            let mut from = 0;
            for (start, len, with) in edits {
                let start = from + start % (old.len() - from + 1);
                let end = (start + len).min(old.len());
                stretches.push((start, end, with));
                from = end;
                if from == old.len() {
                    break;
                }
            }
            let mut new = Vec::new();
            let mut claims = Vec::new();
            let mut copied = 0;
            for (start, end, with) in &stretches {
                new.extend_from_slice(&old[copied..*start]);
                let base = new.len();
                new.extend_from_slice(with);
                claims.push((*start..*end, base..new.len()));
                copied = *end;
            }
            new.extend_from_slice(&old[copied..]);
            let changes = Changes::between(&old, &new, &claims);
            prop_assert_eq!(changes.apply(&old), new);
            let written: usize = stretches.iter().map(|(_, _, with)| with.len()).sum();
            prop_assert!(changes.held() <= written, "{:?} holds more than {}", changes, written);
        }
    }
}
