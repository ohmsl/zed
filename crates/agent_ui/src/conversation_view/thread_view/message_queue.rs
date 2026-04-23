use std::collections::VecDeque;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueueEntryId(usize);

pub struct QueueEntry {
    pub id: QueueEntryId,
    pub content: Vec<acp::ContentBlock>,
    pub tracked_buffers: Vec<Entity<Buffer>>,
    pub editor: Entity<MessageEditor>,
    pub _subscription: Subscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessingState {
    AutoProcess,
    Paused,
    /// A "Send Now" interrupted the current generation. The cancelled
    /// turn will emit a Stopped event that we need to absorb before
    /// resuming normal auto-processing.
    SkipCancelledStop,
}

pub struct MessageQueue {
    entries: VecDeque<QueueEntry>,
    processing_state: ProcessingState,
    can_fast_track: bool,
    next_id: usize,
}

impl MessageQueue {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            processing_state: ProcessingState::AutoProcess,
            can_fast_track: false,
            next_id: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn first(&self) -> Option<&QueueEntry> {
        self.entries.front()
    }

    pub fn first_id(&self) -> Option<QueueEntryId> {
        self.entries.front().map(|e| e.id)
    }

    pub fn entry_by_id(&self, id: QueueEntryId) -> Option<&QueueEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn entry_by_id_mut(&mut self, id: QueueEntryId) -> Option<&mut QueueEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &QueueEntry> {
        self.entries.iter()
    }

    /// Add a new message to the back of the queue. Returns the stable ID.
    /// Also enables auto-processing and fast-track, since the user is
    /// actively engaging with the queue.
    pub fn enqueue(&mut self, entry: QueueEntry) {
        self.entries.push_back(entry);
        self.processing_state = ProcessingState::AutoProcess;
        self.can_fast_track = true;
    }

    /// Allocate the next stable ID. Call this before constructing a
    /// `QueueEntry` so that the subscription closure can capture the ID.
    pub fn next_id(&mut self) -> QueueEntryId {
        let id = QueueEntryId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Remove a specific entry by ID. Returns `None` if not found.
    pub fn remove(&mut self, id: QueueEntryId) -> Option<QueueEntry> {
        let position = self.entries.iter().position(|e| e.id == id)?;
        self.entries.remove(position)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.can_fast_track = false;
    }

    /// Called when the user presses Enter on an empty main editor.
    /// If a message was recently queued (fast-track enabled) and the queue
    /// has entries, pops the front entry for immediate sending.
    pub fn try_fast_track(&mut self) -> Option<QueueEntry> {
        if !self.can_fast_track {
            return None;
        }
        self.can_fast_track = false;
        // Fast-track is an explicit user action, so resume auto-processing
        // even if the queue was paused.
        self.processing_state = ProcessingState::AutoProcess;
        self.entries.pop_front()
    }

    /// Called when generation finishes (Stopped event). Returns the next
    /// entry to auto-send, or `None` if the queue is paused/empty or a
    /// Stopped event needs to be absorbed.
    ///
    /// `is_first_editor_focused` should be `true` when the user is actively
    /// editing the first queued message — we don't auto-send in that case.
    pub fn on_generation_stopped(&mut self, is_first_editor_focused: bool) -> Option<QueueEntry> {
        match self.processing_state {
            ProcessingState::SkipCancelledStop => {
                self.processing_state = ProcessingState::AutoProcess;
                None
            }
            ProcessingState::Paused => None,
            ProcessingState::AutoProcess => {
                if is_first_editor_focused {
                    return None;
                }
                self.entries.pop_front()
            }
        }
    }

    /// Called when the user clicks "Send Now" on a specific queued message.
    /// Removes the entry and, if the agent is currently generating, marks
    /// the queue to absorb the Stopped event from the cancelled turn.
    pub fn send_now(&mut self, id: QueueEntryId, is_generating: bool) -> Option<QueueEntry> {
        let entry = self.remove(id)?;
        if is_generating {
            self.processing_state = ProcessingState::SkipCancelledStop;
        }
        Some(entry)
    }

    /// Called when the user presses the stop button or Escape.
    /// Pauses auto-processing so queued messages aren't sent.
    pub fn pause(&mut self) {
        self.processing_state = ProcessingState::Paused;
    }

    /// Called when the user explicitly sends a new message (via the main
    /// editor). If the queue was paused, this resumes auto-processing —
    /// the user is actively engaging again.
    pub fn resume(&mut self) {
        if self.processing_state == ProcessingState::Paused {
            self.processing_state = ProcessingState::AutoProcess;
        }
    }
}
