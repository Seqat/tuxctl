//! Logs screen: journal batches, search, follow/pause and selection.

use super::*;

impl App {
    pub fn log_count(&self) -> usize {
        self.filtered_logs.len()
    }

    pub fn log_at(&self, index: usize) -> Option<&JournalEntry> {
        self.filtered_logs
            .get(index)
            .and_then(|index| self.logs.get(*index))
    }

    pub fn selected_log_index(&self) -> Option<usize> {
        let selected = self.selected_log?;
        self.filtered_logs.iter().position(|index| {
            self.logs
                .get(*index)
                .is_some_and(|entry| entry.id == selected)
        })
    }

    pub fn selected_log(&self) -> Option<&JournalEntry> {
        let selected = self.selected_log?;
        self.logs.iter().find(|entry| entry.id == selected)
    }

    pub fn log_scroll(&self) -> usize {
        self.log_scroll
    }

    pub fn log_search_query(&self) -> &str {
        &self.log_search_query
    }

    pub fn log_searching(&self) -> bool {
        self.log_searching
    }

    pub fn log_detail_visible(&self) -> bool {
        self.overlay == Some(Overlay::LogDetail)
    }

    pub fn log_following(&self) -> bool {
        self.log_following
    }

    pub fn log_paused(&self) -> bool {
        self.log_paused
    }

    pub fn log_dropped(&self) -> usize {
        self.log_dropped
    }

    pub fn log_error(&self) -> Option<&str> {
        self.log_error.as_deref()
    }

    pub(super) fn update_logs(&mut self, batch: JournalBatch) -> bool {
        let visible = self.active_tab == Tab::Logs;
        let mut changed = false;
        if batch.dropped > 0 {
            self.log_dropped = self.log_dropped.saturating_add(batch.dropped);
            changed = true;
        }
        if let Some(error) = batch.error {
            if self.log_error.as_ref() != Some(&error) {
                self.log_error = Some(error);
                changed = true;
            }
        }
        if batch.entries.is_empty() {
            return changed && visible;
        }

        let previous_index = self.selected_log_index().unwrap_or(0);
        let previous_selection = self.selected_log;
        for entry in batch.entries {
            if self.logs.len() == LOG_BUFFER_CAPACITY {
                self.logs.pop_front();
            }
            self.logs.push_back(entry);
        }
        self.rebuild_log_indices();
        self.restore_log_selection(previous_selection, previous_index);
        self.reconcile_hovered_log();
        self.ensure_log_visible();
        visible
    }

    pub(super) fn begin_log_search(&mut self) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        self.log_search_query.clear();
        self.log_searching = true;
        self.rebuild_log_filter();
        true
    }

    pub(super) fn append_log_search(&mut self, character: char) -> bool {
        if !self.log_searching || character.is_control() {
            return false;
        }
        self.log_search_query.push(character);
        self.rebuild_log_filter();
        true
    }

    pub(super) fn backspace_log_search(&mut self) -> bool {
        if self.log_searching && self.log_search_query.pop().is_some() {
            self.rebuild_log_filter();
            true
        } else {
            false
        }
    }

    pub(super) fn rebuild_log_filter(&mut self) {
        let previous_index = self.selected_log_index().unwrap_or(0);
        let previous_selection = self.selected_log;
        self.rebuild_log_indices();
        self.restore_log_selection(previous_selection, previous_index);
        self.reconcile_hovered_log();
        self.ensure_log_visible();
    }

    fn rebuild_log_indices(&mut self) {
        let query = self.log_search_query.to_lowercase();
        self.filtered_logs = self
            .logs
            .iter()
            .enumerate()
            .filter(|(_, entry)| log_matches(entry, &query))
            .map(|(index, _)| index)
            .collect();
    }

    fn restore_log_selection(&mut self, previous: Option<u64>, previous_index: usize) {
        if self.log_following && !self.log_paused {
            self.selected_log = self
                .log_at(self.filtered_logs.len().saturating_sub(1))
                .map(|entry| entry.id);
            return;
        }

        self.selected_log = previous.filter(|id| {
            self.filtered_logs
                .iter()
                .any(|index| self.logs.get(*index).is_some_and(|entry| entry.id == *id))
        });
        if self.selected_log.is_none() {
            let replacement = previous_index.min(self.filtered_logs.len().saturating_sub(1));
            self.selected_log = self.log_at(replacement).map(|entry| entry.id);
            self.close_overlay(&Overlay::LogDetail);
        }
    }

    pub(super) fn move_log_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Logs || self.filtered_logs.is_empty() {
            return false;
        }
        let was_following = self.log_following;
        self.log_following = false;
        let current = self.selected_log_index().unwrap_or(0);
        let last = self.filtered_logs.len() - 1;
        self.set_log_index(current.saturating_add_signed(delta).min(last)) || was_following
    }

    pub(super) fn select_log_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        let was_following = self.log_following;
        self.log_following = false;
        self.set_log_index(index) || was_following
    }

    fn set_log_index(&mut self, index: usize) -> bool {
        let Some(id) = self.log_at(index).map(|entry| entry.id) else {
            return false;
        };
        let previous = (self.selected_log, self.log_scroll);
        self.selected_log = Some(id);
        self.ensure_log_visible();
        previous != (self.selected_log, self.log_scroll)
    }

    pub(super) fn select_log(&mut self, id: u64) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        let Some(index) = self
            .filtered_logs
            .iter()
            .position(|index| self.logs.get(*index).is_some_and(|entry| entry.id == id))
        else {
            return false;
        };
        let was_following = self.log_following;
        self.log_following = false;
        self.set_log_index(index) || was_following
    }

    pub(super) fn open_log_details(&mut self) -> bool {
        if self.active_tab == Tab::Logs && self.selected_log().is_some() {
            self.log_searching = false;
            self.overlay = Some(Overlay::LogDetail);
            self.hovered = None;
            true
        } else {
            false
        }
    }

    pub(super) fn toggle_log_follow(&mut self) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        self.log_following = !self.log_following;
        if self.log_following && !self.log_paused {
            let last = self.filtered_logs.len().saturating_sub(1);
            self.set_log_index(last);
        }
        true
    }

    pub(super) fn toggle_log_pause(&mut self) -> bool {
        if self.active_tab != Tab::Logs {
            return false;
        }
        self.log_paused = !self.log_paused;
        if !self.log_paused && self.log_following {
            let last = self.filtered_logs.len().saturating_sub(1);
            self.set_log_index(last);
        }
        true
    }

    pub(super) fn ensure_log_visible(&mut self) {
        self.log_scroll = calculate_scroll(
            self.filtered_logs.len(),
            self.selected_log_index(),
            self.log_scroll,
            self.log_view_height,
        );
    }

    fn reconcile_hovered_log(&mut self) {
        if let Some(MouseTarget::LogRow(id)) = &self.hovered {
            if !self
                .filtered_logs
                .iter()
                .any(|index| self.logs.get(*index).is_some_and(|entry| entry.id == *id))
            {
                self.hovered = None;
            }
        }
    }
}

fn log_matches(entry: &JournalEntry, query: &str) -> bool {
    query.is_empty()
        || entry.source.to_lowercase().contains(query)
        || entry.message.to_lowercase().contains(query)
        || entry.priority.is_some_and(|priority| {
            priority.to_string().contains(query)
                || entry.priority_label().contains(query)
                || (priority == 4 && "warning".contains(query))
        })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn logs_follow_latest_entry_until_manual_navigation() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![
            log_entry(1, "kernel", 6, "one"),
            log_entry(2, "sshd.service", 4, "two"),
        ])));

        assert_eq!(app.selected_log().map(|entry| entry.id), Some(2));
        assert!(app.log_following());

        app.update(Action::LogPrevious);
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(1));
        assert!(!app.log_following());

        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            3,
            "dbus.service",
            6,
            "three",
        )])));
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(1));
    }

    #[test]
    fn pausing_preserves_position_and_resuming_catches_up_when_following() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            1, "kernel", 6, "one",
        )])));
        app.update(Action::ToggleLogPause);

        app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            2, "kernel", 6, "two",
        )])));
        assert!(app.log_paused());
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(1));

        app.update(Action::ToggleLogPause);
        assert!(!app.log_paused());
        assert_eq!(app.selected_log().map(|entry| entry.id), Some(2));
    }

    #[test]
    fn log_search_is_case_insensitive() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(log_batch(vec![
            log_entry(1, "kernel", 6, "device ready"),
            log_entry(2, "SSHD.service", 4, "Login Failed"),
        ])));
        app.update(Action::BeginLogSearch);
        for character in "login FAILED".chars() {
            app.update(Action::AppendLogSearch(character));
        }

        assert_eq!(visible_log_ids(&app), vec![2]);
    }

    #[test]
    fn log_buffer_evicts_oldest_entries_at_capacity() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        let entries = (1..=LOG_BUFFER_CAPACITY as u64 + 1)
            .map(|id| log_entry(id, "test", 6, "entry"))
            .collect();

        app.update(Action::LogsUpdated(log_batch(entries)));

        assert_eq!(app.log_count(), LOG_BUFFER_CAPACITY);
        assert_eq!(app.log_at(0).map(|entry| entry.id), Some(2));
        assert_eq!(
            app.log_at(LOG_BUFFER_CAPACITY - 1).map(|entry| entry.id),
            Some(LOG_BUFFER_CAPACITY as u64 + 1)
        );
    }

    #[test]
    fn log_drop_and_error_metadata_is_nonfatal() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Logs));
        app.update(Action::LogsUpdated(JournalBatch {
            entries: vec![log_entry(1, "kernel", 3, "failure")],
            dropped: 7,
            error: Some("journal stream ended".into()),
        }));

        assert_eq!(app.log_count(), 1);
        assert_eq!(app.log_dropped(), 7);
        assert_eq!(app.log_error(), Some("journal stream ended"));
    }

    #[test]
    fn journal_ingestion_does_not_redraw_an_inactive_screen() {
        let mut app = App::default();

        assert!(!app.update(Action::LogsUpdated(log_batch(vec![log_entry(
            1,
            "kernel",
            6,
            "background entry",
        )]))));
        assert_eq!(app.log_count(), 1);
    }

    #[test]
    fn log_matches_matches_priority_names_and_numbers() {
        let error_entry = JournalEntry {
            id: 1,
            timestamp_micros: None,
            local_time: None,
            source: "app".into(),
            priority: Some(3),
            message: "something happened".into(),
        };
        let warn_entry = JournalEntry {
            id: 2,
            timestamp_micros: None,
            local_time: None,
            source: "app".into(),
            priority: Some(4),
            message: "look out".into(),
        };

        assert!(log_matches(&error_entry, "error"));
        assert!(log_matches(&error_entry, "err"));
        assert!(log_matches(&error_entry, "3"));
        assert!(!log_matches(&error_entry, "warn"));

        assert!(log_matches(&warn_entry, "warn"));
        assert!(log_matches(&warn_entry, "warning"));
        assert!(log_matches(&warn_entry, "4"));
        assert!(!log_matches(&warn_entry, "crit"));
    }
}
