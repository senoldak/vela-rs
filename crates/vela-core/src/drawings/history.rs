//! Snapshot-based undo/redo for the drawing model.

use super::document::DrawingsDocument;

/// Default cap of the undo stack.
pub const HISTORY_CAP: usize = 100;

/// Holds full-document snapshots on an undo/redo stack pair. A controller
/// drives it; the store stays a dumb source of truth. Two recording modes:
///
/// - [`record`](Self::record): atomic single-step. Push the pre-mutation
///   snapshot, then the caller mutates. One CRUD call = one undo entry.
/// - [`begin`](Self::begin) / [`mark_dirty`](Self::mark_dirty) /
///   [`commit`](Self::commit): a TRANSACTION that coalesces many store
///   mutations (a multi-drag, multi-delete, duplicate, paste) into ONE undo
///   entry. The pre-mutation snapshot is captured at `begin`; the first
///   `mark_dirty` inside the transaction promotes it onto the undo stack;
///   further ones are no-ops.
///
/// A new mutation clears the redo stack. The undo stack is capped (oldest dropped).
#[derive(Clone, Debug)]
pub struct DrawingHistory {
    undo_stack: Vec<DrawingsDocument>,
    redo_stack: Vec<DrawingsDocument>,
    /// Pre-mutation snapshot of the open transaction.
    pending: Option<DrawingsDocument>,
    /// Promoted-once guard within a transaction.
    dirtied: bool,
    /// Transaction nesting (re-entrancy).
    depth: usize,
    cap: usize,
}

impl Default for DrawingHistory {
    fn default() -> Self {
        Self::new(HISTORY_CAP)
    }
}

impl DrawingHistory {
    pub fn new(cap: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending: None,
            dirtied: false,
            depth: 0,
            cap,
        }
    }

    /// Open a transaction; `snapshot` is the model state BEFORE any mutation in it.
    pub fn begin(&mut self, snapshot: &DrawingsDocument) {
        self.depth += 1;
        if self.depth == 1 {
            self.pending = Some(snapshot.clone());
            self.dirtied = false;
        }
    }

    /// The first mutation inside the open transaction pushes the pre-snapshot once.
    pub fn mark_dirty(&mut self) {
        if self.depth == 0 || self.dirtied {
            return;
        }
        let Some(pending) = self.pending.clone() else {
            return;
        };
        self.push(pending);
        self.dirtied = true;
    }

    /// Close the transaction; if nothing mutated, the snapshot is dropped (no entry).
    pub fn commit(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0 {
            self.pending = None;
            self.dirtied = false;
        }
    }

    /// Abort the open transaction (the caller rolled its mutation back); no
    /// entry. Same teardown as a commit: restoring state is the caller's job.
    pub fn abort(&mut self) {
        self.commit();
    }

    /// Atomic step: record the pre-mutation snapshot as one undo entry (or fold into an open transaction).
    pub fn record(&mut self, before: &DrawingsDocument) {
        if self.depth > 0 {
            self.mark_dirty();
            return;
        }
        self.push(before.clone());
    }

    /// Undo: returns the document to restore (`current` goes onto redo), or `None` if empty.
    pub fn undo(&mut self, current: &DrawingsDocument) -> Option<DrawingsDocument> {
        let prev = self.undo_stack.pop()?;
        self.redo_stack.push(current.clone());
        Some(prev)
    }

    /// Redo: returns the document to restore (`current` goes onto undo), or `None` if empty.
    pub fn redo(&mut self, current: &DrawingsDocument) -> Option<DrawingsDocument> {
        let next = self.redo_stack.pop()?;
        self.undo_stack.push(current.clone());
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Drop all history (a context switch, e.g. loading another document).
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.pending = None;
        self.dirtied = false;
        self.depth = 0;
    }

    fn push(&mut self, doc: DrawingsDocument) {
        self.undo_stack.push(doc);
        if self.undo_stack.len() > self.cap {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear(); // a fresh mutation invalidates redo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(version: u32) -> DrawingsDocument {
        // The version field doubles as a marker telling snapshots apart.
        DrawingsDocument {
            version,
            drawings: Vec::new(),
        }
    }

    #[test]
    fn record_then_undo_and_redo() {
        let mut h = DrawingHistory::default();
        assert!(!h.can_undo());
        h.record(&doc(1));
        assert_eq!(h.undo(&doc(2)), Some(doc(1)));
        assert!(h.can_redo());
        assert_eq!(h.redo(&doc(1)), Some(doc(2)));
        assert_eq!(h.undo(&doc(2)), Some(doc(1)));
        h.record(&doc(5)); // a fresh mutation invalidates redo
        assert!(!h.can_redo());
    }

    #[test]
    fn a_transaction_coalesces_into_one_entry() {
        let mut h = DrawingHistory::default();
        h.begin(&doc(1));
        h.begin(&doc(9)); // nested: the outer snapshot stays
        h.mark_dirty();
        h.record(&doc(7)); // folds into the open transaction
        h.mark_dirty();
        h.commit();
        h.commit();
        assert_eq!(h.undo(&doc(3)), Some(doc(1)));
        assert!(!h.can_undo());
    }

    #[test]
    fn an_untouched_transaction_leaves_no_entry() {
        let mut h = DrawingHistory::default();
        h.begin(&doc(1));
        h.abort();
        assert!(!h.can_undo());
        h.mark_dirty(); // outside a transaction: ignored
        assert!(!h.can_undo());
    }

    #[test]
    fn the_undo_stack_is_capped() {
        let mut h = DrawingHistory::new(2);
        for v in 1..=3 {
            h.record(&doc(v));
        }
        assert_eq!(h.undo(&doc(0)), Some(doc(3)));
        assert_eq!(h.undo(&doc(0)), Some(doc(2)));
        assert_eq!(h.undo(&doc(0)), None);
    }
}
