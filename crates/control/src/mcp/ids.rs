// SPDX-License-Identifier: GPL-3.0-or-later
//! Ids that a batch creates and uses in the same batch.
//!
//! `apply()` allocates ids from one counter, in edit order (protocol
//! `edit.rs`), and an edit cannot name an id that an earlier edit of the same
//! batch made. To keep "add an instrument with its own mixer track and a
//! clip of drums" one undo group, the server predicts those ids: the next id
//! is above every id in the project and above every id this agent has seen
//! created. The prediction is checked against `Applied::created`; the strict
//! `base_revision` rule (17.1) means nothing else can allocate in between.
//! It can still be wrong after the user undid the creation of something
//! (ids are never reused), which the caller detects and repairs.

use protocol::ids::FIRST_ID;
use protocol::model::Project;

pub struct IdGen {
    next: u32,
    /// Every id handed out, in order: the expected start of `created`.
    pub predicted: Vec<u32>,
}

impl IdGen {
    /// `floor` is the lowest id the counter can be at, when known.
    pub fn new(project: &Project, floor: Option<u32>) -> IdGen {
        let next = project
            .max_id()
            .saturating_add(1)
            .max(floor.unwrap_or(0))
            .max(FIRST_ID);
        IdGen {
            next,
            predicted: Vec::new(),
        }
    }

    pub fn alloc(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.predicted.push(id);
        id
    }
}

/// True when `apply()` handed out the ids the batch was built with.
pub fn matches(predicted: &[u32], created: &[u32]) -> bool {
    created.starts_with(predicted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_above_the_project_and_the_floor() {
        let p = Project::empty();
        let mut g = IdGen::new(&p, None);
        assert_eq!(g.alloc(), FIRST_ID);
        let mut g = IdGen::new(&p, Some(40));
        assert_eq!((g.alloc(), g.alloc()), (40, 41));
        assert_eq!(g.predicted, vec![40, 41]);
        assert!(matches(&[40, 41], &[40, 41, 42]));
        assert!(!matches(&[40, 41], &[44, 45]));
    }
}
