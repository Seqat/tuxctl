//! Network screen: interface snapshots, selection and details.

use super::*;

impl App {
    pub fn network_count(&self) -> usize {
        self.networks.len()
    }

    pub fn networks(&self) -> &[NetworkInterfaceInfo] {
        &self.networks
    }

    pub fn network_at(&self, index: usize) -> Option<&NetworkInterfaceInfo> {
        self.networks.get(index)
    }

    pub fn selected_network_index(&self) -> Option<usize> {
        let selected = self.selected_network.as_deref()?;
        self.networks.iter().position(|net| net.name == selected)
    }

    pub fn selected_network(&self) -> Option<&NetworkInterfaceInfo> {
        let selected = self.selected_network.as_deref()?;
        self.networks.iter().find(|net| net.name == selected)
    }

    pub fn network_scroll(&self) -> usize {
        self.network_scroll
    }

    pub fn network_detail_visible(&self) -> bool {
        self.overlay == Some(Overlay::NetworkDetail)
    }

    pub fn network_error(&self) -> Option<&str> {
        self.network_error.as_deref()
    }

    pub(super) fn update_networks(&mut self, snapshot: NetworkSnapshot) -> bool {
        let visible = matches!(self.active_tab, Tab::Overview | Tab::Network);
        if let Some(error) = snapshot.error {
            if self.network_error.as_ref() == Some(&error) {
                return false;
            }
            self.network_error = Some(error);
            return visible;
        }

        if self.network_error.is_none() && self.networks == snapshot.interfaces {
            return false;
        }

        let previous_index = self.selected_network_index().unwrap_or(0);
        let previous_selection = self.selected_network.take();
        self.networks = snapshot.interfaces;
        self.network_error = None;

        self.selected_network =
            previous_selection.filter(|name| self.networks.iter().any(|iface| iface.name == *name));

        if self.selected_network.is_none() {
            let replacement = previous_index.min(self.networks.len().saturating_sub(1));
            self.selected_network = self
                .networks
                .get(replacement)
                .map(|iface| iface.name.clone());
            self.close_overlay(&Overlay::NetworkDetail);
        }
        self.reconcile_hovered_network();
        self.ensure_network_visible();
        visible
    }

    pub(super) fn move_network_selection(&mut self, delta: isize) -> bool {
        if self.active_tab != Tab::Network || self.networks.is_empty() {
            return false;
        }

        let current = self.selected_network_index().unwrap_or(0);
        let last = self.networks.len() - 1;
        let next = current.saturating_add_signed(delta).min(last);
        self.select_network_index(next)
    }

    pub fn select_network(&mut self, name: &str) -> bool {
        if self.active_tab != Tab::Network {
            return false;
        }
        if self.selected_network.as_deref() == Some(name) {
            return false;
        }

        if self.networks.iter().any(|iface| iface.name == name) {
            self.selected_network = Some(name.to_owned());
            self.ensure_network_visible();
            true
        } else {
            false
        }
    }

    pub(super) fn select_network_index(&mut self, index: usize) -> bool {
        if self.active_tab != Tab::Network {
            return false;
        }
        let Some(iface) = self.networks.get(index) else {
            return false;
        };
        let name = iface.name.clone();
        if self.selected_network.as_ref() == Some(&name) {
            return false;
        }

        self.selected_network = Some(name);
        self.ensure_network_visible();
        true
    }

    pub(super) fn open_network_details(&mut self) -> bool {
        if self.active_tab == Tab::Network && self.selected_network.is_some() {
            let changed = !self.network_detail_visible();
            self.overlay = Some(Overlay::NetworkDetail);
            self.hovered = None;
            changed
        } else {
            false
        }
    }

    pub(super) fn ensure_network_visible(&mut self) {
        self.network_scroll = calculate_scroll(
            self.networks.len(),
            self.selected_network_index(),
            self.network_scroll,
            self.network_view_height,
        );
    }

    fn reconcile_hovered_network(&mut self) {
        if let Some(MouseTarget::NetworkRow(name)) = &self.hovered {
            if !self
                .networks
                .iter()
                .any(|iface| iface.name == name.as_ref())
            {
                self.hovered = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn network_selection_is_preserved_by_name() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0"), dummy_network("wlan0")],
            error: None,
        }));
        app.update(Action::SelectNetwork("wlan0".into()));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );

        // Update with reordered list
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![
                dummy_network("docker0"),
                dummy_network("eth0"),
                dummy_network("wlan0"),
            ],
            error: None,
        }));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );
    }

    #[test]
    fn disappearing_network_interface_recovers_and_closes_details() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![
                dummy_network("eth0"),
                dummy_network("wlan0"),
                dummy_network("veth1"),
            ],
            error: None,
        }));
        app.update(Action::SelectNetwork("wlan0".into()));
        app.update(Action::OpenNetworkDetails);
        assert!(app.network_detail_visible());

        // wlan0 disappears
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0"), dummy_network("veth1")],
            error: None,
        }));

        // Selection clamped to nearest and details closed
        assert!(!app.network_detail_visible());
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("veth1")
        );
    }

    #[test]
    fn network_keyboard_navigation() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Network));
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![
                dummy_network("eth0"),
                dummy_network("wlan0"),
                dummy_network("veth1"),
            ],
            error: None,
        }));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("eth0")
        );

        app.update(Action::NetworkNext);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );

        app.update(Action::NetworkLast);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("veth1")
        );

        app.update(Action::NetworkPrevious);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("wlan0")
        );

        app.update(Action::NetworkFirst);
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("eth0")
        );
    }

    #[test]
    fn network_selection_and_details_guarded_by_active_tab() {
        let mut app = App::default();
        app.update(Action::NetworkUpdated(NetworkSnapshot {
            interfaces: vec![dummy_network("eth0"), dummy_network("eth1")],
            error: None,
        }));

        // On Overview tab, network selection actions should return false
        assert_eq!(app.active_tab(), Tab::Overview);
        assert!(!app.update(Action::SelectNetwork("eth1".into())));
        assert!(!app.update(Action::NetworkFirst));
        assert!(!app.update(Action::NetworkNext));

        // Switch to Network tab
        app.update(Action::SelectTab(Tab::Network));
        assert!(app.update(Action::SelectNetwork("eth1".into())));
        assert_eq!(
            app.selected_network().map(|n| n.name.as_str()),
            Some("eth1")
        );

        // Open network details should open, not toggle
        assert!(app.update(Action::OpenNetworkDetails));
        assert!(app.network_detail_visible());
        assert!(!app.update(Action::OpenNetworkDetails));
        assert!(app.network_detail_visible());
    }
}
