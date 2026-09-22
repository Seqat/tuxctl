//! Services screen: snapshot handling, search, selection and refresh requests.

use super::*;

impl App {
    pub fn service_count(&self) -> usize {
        self.filtered_services.len()
    }

    pub fn service_at(&self, index: usize) -> Option<&ServiceInfo> {
        self.filtered_services
            .get(index)
            .and_then(|index| self.services.get(*index))
    }

    pub fn selected_service_index(&self) -> Option<usize> {
        let selected = self.selected_service.as_deref()?;
        self.filtered_services.iter().position(|index| {
            self.services
                .get(*index)
                .is_some_and(|service| service.unit == selected)
        })
    }

    pub fn selected_service(&self) -> Option<&ServiceInfo> {
        let selected = self.selected_service.as_deref()?;
        self.services
            .iter()
            .find(|service| service.unit == selected)
    }

    pub fn service_scroll(&self) -> usize {
        self.service_scroll
    }

    pub fn service_search_query(&self) -> &str {
        &self.service_search_query
    }

    pub fn service_searching(&self) -> bool {
        self.service_searching
    }

    pub fn service_detail_visible(&self) -> bool {
        self.overlay == Some(Overlay::ServiceDetail)
    }

    pub fn service_error(&self) -> Option<&str> {
        self.service_error.as_deref()
    }

    pub fn service_refreshing(&self) -> bool {
        self.pending_service_refresh_generation.is_some()
    }

    pub fn take_service_refresh_request(&mut self) -> Option<ServiceRefreshGeneration> {
        self.service_refresh_requested.take()
    }

    pub(super) fn update_services(&mut self, snapshot: ServiceSnapshot) -> bool {
        let visible = self.active_tab == Tab::Services;
        let was_refreshing = self.service_refreshing();
        if self
            .pending_service_refresh_generation
            .is_some_and(|pending| snapshot.completed_refresh_generation >= pending)
        {
            self.pending_service_refresh_generation = None;
        }
        let refresh_state_changed = was_refreshing != self.service_refreshing();
        if let Some(error) = snapshot.error {
            if self.service_error.as_ref() == Some(&error) {
                return refresh_state_changed && visible;
            }
            self.service_error = Some(error);
            return visible;
        }

        if self.service_error.is_none() && self.services == snapshot.services {
            return refresh_state_changed && visible;
        }

        let previous_index = self.selected_service_index().unwrap_or(0);
        let previous_selection = self.selected_service.clone();
        self.services = snapshot.services;
        self.service_error = None;
        self.rebuild_service_filter();

        self.selected_service = previous_selection.filter(|unit| {
            self.filtered_services.iter().any(|index| {
                self.services
                    .get(*index)
                    .is_some_and(|service| service.unit == *unit)
            })
        });
        if self.selected_service.is_none() {
            let replacement = previous_index.min(self.filtered_services.len().saturating_sub(1));
            self.selected_service = self
                .service_at(replacement)
                .map(|service| service.unit.clone());
            self.close_overlay(&Overlay::ServiceDetail);
        }
        self.reconcile_hovered_service();
        self.ensure_service_visible();
        visible
    }

    pub(super) fn begin_service_search(&mut self) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        self.service_search_query.clear();
        self.service_searching = true;
        self.rebuild_service_filter();
        true
    }

    pub(super) fn append_service_search(&mut self, character: char) -> bool {
        if !self.service_searching || character.is_control() {
            return false;
        }
        self.service_search_query.push(character);
        self.rebuild_service_filter();
        true
    }

    pub(super) fn backspace_service_search(&mut self) -> bool {
        if self.service_searching && self.service_search_query.pop().is_some() {
            self.rebuild_service_filter();
            true
        } else {
            false
        }
    }

    pub(super) fn rebuild_service_filter(&mut self) {
        let previous_index = self.selected_service_index().unwrap_or(0);
        let previous_selection = self.selected_service.clone();
        let query = self.service_search_query.to_lowercase();

        self.filtered_services = self
            .services
            .iter()
            .enumerate()
            .filter(|(_, service)| service_matches(service, &query))
            .map(|(index, _)| index)
            .collect();

        self.selected_service = previous_selection.filter(|unit| {
            self.filtered_services.iter().any(|index| {
                self.services
                    .get(*index)
                    .is_some_and(|service| service.unit == *unit)
            })
        });
        if self.selected_service.is_none() {
            let replacement = previous_index.min(self.filtered_services.len().saturating_sub(1));
            self.selected_service = self
                .service_at(replacement)
                .map(|service| service.unit.clone());
        }
        self.reconcile_hovered_service();
        self.ensure_service_visible();
    }

    pub(super) fn move_service_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Services || self.filtered_services.is_empty() {
            return false;
        }
        let current = self.selected_service_index().unwrap_or(0);
        let last = self.filtered_services.len() - 1;
        self.select_service_index(current.saturating_add_signed(delta).min(last))
    }

    pub(super) fn select_service_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        let Some(unit) = self.service_at(index).map(|service| service.unit.clone()) else {
            return false;
        };
        let previous = (self.selected_service.clone(), self.service_scroll);
        self.selected_service = Some(unit);
        self.ensure_service_visible();
        previous != (self.selected_service.clone(), self.service_scroll)
    }

    pub(super) fn select_service(&mut self, unit: &str) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        let Some(index) = self.filtered_services.iter().position(|index| {
            self.services
                .get(*index)
                .is_some_and(|service| service.unit == unit)
        }) else {
            return false;
        };
        self.select_service_index(index)
    }

    pub(super) fn open_service_details(&mut self) -> bool {
        if self.active_tab == Tab::Services && self.selected_service().is_some() {
            self.service_searching = false;
            self.overlay = Some(Overlay::ServiceDetail);
            self.hovered = None;
            true
        } else {
            false
        }
    }

    pub(super) fn request_service_refresh(&mut self) -> bool {
        if self.active_tab != Tab::Services {
            return false;
        }
        self.service_refresh_generation = self.service_refresh_generation.saturating_add(1);
        let generation = self.service_refresh_generation;
        self.service_refresh_requested = Some(generation);
        let changed = !self.service_refreshing();
        self.pending_service_refresh_generation = Some(generation);
        changed
    }

    pub(super) fn ensure_service_visible(&mut self) {
        self.service_scroll = calculate_scroll(
            self.filtered_services.len(),
            self.selected_service_index(),
            self.service_scroll,
            self.service_view_height,
        );
    }

    fn reconcile_hovered_service(&mut self) {
        if let Some(MouseTarget::ServiceRow(unit)) = &self.hovered {
            let remains_visible = self.filtered_services.iter().any(|index| {
                self.services
                    .get(*index)
                    .is_some_and(|service| service.unit == unit.as_ref())
            });
            if !remains_visible {
                self.hovered = None;
            }
        }
    }
}

fn service_matches(service: &ServiceInfo, query: &str) -> bool {
    query.is_empty()
        || service.unit.to_lowercase().contains(query)
        || service.description.to_lowercase().contains(query)
        || service.load_state.to_lowercase().contains(query)
        || service.active_state.to_lowercase().contains(query)
        || service.sub_state.to_lowercase().contains(query)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn service_filter_is_case_insensitive_across_unit_and_description() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![
            service("alpha.service", "active", "Worker"),
            service("postgresql.service", "active", "Database Server"),
        ])));

        app.update(Action::BeginServiceSearch);
        for character in "DATABASE".chars() {
            app.update(Action::AppendServiceSearch(character));
        }

        assert_eq!(visible_units(&app), vec!["postgresql.service"]);
        assert_eq!(
            app.selected_service().map(|service| service.unit.as_str()),
            Some("postgresql.service")
        );
    }

    #[test]
    fn service_navigation_keeps_selection_visible() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(
            (0..6)
                .map(|index| service(&format!("service-{index}.service"), "active", ""))
                .collect(),
        )));
        app.update(Action::ServiceViewportChanged {
            start: 0,
            height: 2,
        });

        app.update(Action::ServiceNextPage);
        assert_eq!(app.selected_service_index(), Some(2));
        assert_eq!(app.service_scroll(), 1);

        app.update(Action::ServiceLast);
        assert_eq!(app.selected_service_index(), Some(5));
        assert_eq!(app.service_scroll(), 4);
    }

    #[test]
    fn service_refresh_preserves_selection_by_unit_name() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![
            service("alpha.service", "inactive", "Alpha"),
            service("beta.service", "active", "Beta"),
        ])));
        app.update(Action::SelectService("beta.service".into()));

        app.update(Action::ServicesUpdated(services(vec![
            service("beta.service", "inactive", "Beta changed"),
            service("alpha.service", "active", "Alpha"),
        ])));

        assert_eq!(
            app.selected_service().map(|service| service.unit.as_str()),
            Some("beta.service")
        );
    }

    #[test]
    fn disappearing_service_is_nonfatal_and_closes_details() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![
            service("alpha.service", "active", "Alpha"),
            service("beta.service", "active", "Beta"),
        ])));
        app.update(Action::SelectService("beta.service".into()));
        app.update(Action::OpenServiceDetails);

        app.update(Action::ServicesUpdated(services(vec![service(
            "alpha.service",
            "inactive",
            "Alpha",
        )])));

        assert_eq!(
            app.selected_service().map(|service| service.unit.as_str()),
            Some("alpha.service")
        );
        assert!(!app.service_detail_visible());
    }

    #[test]
    fn service_refresh_is_an_explicit_background_request() {
        let mut app = services_tab_after_entry_refresh();

        assert!(app.update(Action::RefreshServices));
        assert!(app.service_refreshing());
        assert_eq!(app.take_service_refresh_request(), Some(2));
        assert_eq!(app.take_service_refresh_request(), None);

        app.update(Action::ServicesUpdated(services(vec![service(
            "alpha.service",
            "active",
            "Alpha",
        )])));
        assert!(app.service_refreshing());

        assert!(app.update(Action::ServicesUpdated(services_completed(
            vec![service("alpha.service", "active", "Alpha")],
            2,
        ))));
        assert!(!app.service_refreshing());
    }

    #[test]
    fn service_refresh_waits_for_the_latest_coalesced_generation() {
        let mut app = services_tab_after_entry_refresh();

        assert!(app.update(Action::RefreshServices));
        assert!(!app.update(Action::RefreshServices));
        assert!(!app.update(Action::RefreshServices));
        assert_eq!(app.take_service_refresh_request(), Some(4));

        app.update(Action::ServicesUpdated(services_completed(Vec::new(), 3)));
        assert!(app.service_refreshing());

        assert!(app.update(Action::ServicesUpdated(services_completed(Vec::new(), 4,))));
        assert!(!app.service_refreshing());
    }

    /// Selects Services and completes the refresh that entering the tab requests.
    fn services_tab_after_entry_refresh() -> App {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        assert_eq!(app.take_service_refresh_request(), Some(1));
        app.update(Action::ServicesUpdated(services_completed(Vec::new(), 1)));
        assert!(!app.service_refreshing());
        app
    }

    #[test]
    fn entering_services_requests_exactly_one_refresh_per_entry() {
        let mut app = App::default();
        assert!(!app.services_visible());
        assert_eq!(app.take_service_refresh_request(), None);

        app.update(Action::SelectTab(Tab::Services));
        assert!(app.services_visible());
        assert!(app.service_refreshing());
        assert_eq!(app.take_service_refresh_request(), Some(1));

        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::NextTab);
        app.update(Action::PreviousTab);
        assert_eq!(
            app.take_service_refresh_request(),
            Some(2),
            "leaving and re-entering requests one new refresh"
        );
        assert_eq!(app.take_service_refresh_request(), None);

        app.update(Action::SelectTab(Tab::Logs));
        assert!(!app.services_visible());
        assert_eq!(app.take_service_refresh_request(), None);
    }

    #[test]
    fn service_collection_failure_preserves_the_last_good_snapshot() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        app.update(Action::ServicesUpdated(services(vec![service(
            "alpha.service",
            "active",
            "Alpha",
        )])));

        app.update(Action::ServicesUpdated(ServiceSnapshot {
            services: Vec::new(),
            error: Some("system bus unavailable".into()),
            completed_refresh_generation: 0,
        }));

        assert_eq!(visible_units(&app), vec!["alpha.service"]);
        assert_eq!(app.service_error(), Some("system bus unavailable"));
    }
}
