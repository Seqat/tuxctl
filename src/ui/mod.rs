mod hardware;
mod hardware_cpu;
mod hardware_network_summary;
mod layout;
mod logs;
mod network;
mod overview;
mod processes;
mod services;
mod status;

use std::sync::Arc;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    action::{InputMode, MouseTarget, ProcessSortField, Tab},
    app::{App, Collector},
    linux::ByteUsage,
};

#[derive(Debug, Default)]
pub struct UiRegions {
    tabs: Vec<TabRegion>,
    process_rows: Vec<ProcessRowRegion>,
    process_headers: Vec<ProcessHeaderRegion>,
    process_scroll_area: Option<Rect>,
    process_viewport: Option<(usize, usize)>,
    service_rows: Vec<ServiceRowRegion>,
    service_scroll_area: Option<Rect>,
    service_viewport: Option<(usize, usize)>,
    log_rows: Vec<LogRowRegion>,
    log_scroll_area: Option<Rect>,
    log_viewport: Option<(usize, usize)>,
    network_rows: Vec<NetworkRowRegion>,
    network_scroll_area: Option<Rect>,
    network_viewport: Option<(usize, usize)>,
    process_signal_cancel: Option<Rect>,
    process_signal_confirm: Option<Rect>,
    input_mode: InputMode,
}

#[derive(Debug)]
struct TabRegion {
    tab: Tab,
    area: Rect,
}

#[derive(Debug)]
struct ProcessRowRegion {
    identity: crate::linux::ProcessIdentity,
    area: Rect,
}

#[derive(Debug)]
struct ProcessHeaderRegion {
    field: ProcessSortField,
    area: Rect,
}

#[derive(Debug)]
struct ServiceRowRegion {
    unit: Arc<str>,
    area: Rect,
}

#[derive(Debug)]
struct LogRowRegion {
    id: u64,
    area: Rect,
}

#[derive(Debug)]
struct NetworkRowRegion {
    name: Arc<str>,
    area: Rect,
}

enum ContentRender {
    None,
    Processes(processes::ProcessRender),
    Services(services::ServiceRender),
    Logs(logs::LogRender),
    Network(network::NetworkRender),
}

impl UiRegions {
    pub(crate) fn from_tabs(tabs: impl IntoIterator<Item = (Tab, Rect)>) -> Self {
        Self {
            tabs: tabs
                .into_iter()
                .map(|(tab, area)| TabRegion { tab, area })
                .collect(),
            ..Self::default()
        }
    }

    fn suppress_background_interaction(&mut self) {
        self.tabs.clear();
        self.process_rows.clear();
        self.process_headers.clear();
        self.process_scroll_area = None;
        self.service_rows.clear();
        self.service_scroll_area = None;
        self.log_rows.clear();
        self.log_scroll_area = None;
        self.network_rows.clear();
        self.network_scroll_area = None;
    }

    pub fn target_at(&self, column: u16, row: u16) -> Option<MouseTarget> {
        if self
            .process_signal_cancel
            .is_some_and(|area| contains(area, column, row))
        {
            return Some(MouseTarget::ProcessSignalCancel);
        }
        if self
            .process_signal_confirm
            .is_some_and(|area| contains(area, column, row))
        {
            return Some(MouseTarget::ProcessSignalConfirm);
        }

        self.tabs
            .iter()
            .find(|region| contains(region.area, column, row))
            .map(|region| MouseTarget::Tab(region.tab))
            .or_else(|| {
                self.process_headers
                    .iter()
                    .find(|region| contains(region.area, column, row))
                    .map(|region| MouseTarget::ProcessSortHeader(region.field))
            })
            .or_else(|| {
                self.process_rows
                    .iter()
                    .find(|region| contains(region.area, column, row))
                    .map(|region| MouseTarget::ProcessRow(region.identity))
            })
            .or_else(|| {
                self.service_rows
                    .iter()
                    .find(|region| contains(region.area, column, row))
                    .map(|region| MouseTarget::ServiceRow(region.unit.clone()))
            })
            .or_else(|| {
                self.log_rows
                    .iter()
                    .find(|region| contains(region.area, column, row))
                    .map(|region| MouseTarget::LogRow(region.id))
            })
            .or_else(|| {
                self.network_rows
                    .iter()
                    .find(|region| contains(region.area, column, row))
                    .map(|region| MouseTarget::NetworkRow(region.name.clone()))
            })
    }

    pub fn process_viewport(&self) -> Option<(usize, usize)> {
        self.process_viewport
    }

    pub fn process_scroll_at(&self, column: u16, row: u16) -> bool {
        self.process_scroll_area
            .is_some_and(|area| contains(area, column, row))
            && matches!(
                self.input_mode,
                InputMode::Normal | InputMode::ProcessSearch
            )
    }

    pub fn service_viewport(&self) -> Option<(usize, usize)> {
        self.service_viewport
    }

    pub fn service_scroll_at(&self, column: u16, row: u16) -> bool {
        self.service_scroll_area
            .is_some_and(|area| contains(area, column, row))
            && matches!(
                self.input_mode,
                InputMode::Services | InputMode::ServiceSearch
            )
    }

    pub fn log_viewport(&self) -> Option<(usize, usize)> {
        self.log_viewport
    }

    pub fn log_scroll_at(&self, column: u16, row: u16) -> bool {
        self.log_scroll_area
            .is_some_and(|area| contains(area, column, row))
            && matches!(self.input_mode, InputMode::Logs | InputMode::LogSearch)
    }

    pub fn network_viewport(&self) -> Option<(usize, usize)> {
        self.network_viewport
    }

    pub fn network_scroll_at(&self, column: u16, row: u16) -> bool {
        self.network_scroll_area
            .is_some_and(|area| contains(area, column, row))
            && self.input_mode == InputMode::Network
    }

    pub fn input_mode(&self) -> InputMode {
        self.input_mode
    }

    #[cfg(test)]
    pub(crate) fn from_process_rows(
        rows: impl IntoIterator<Item = (crate::linux::ProcessIdentity, Rect)>,
        input_mode: InputMode,
    ) -> Self {
        let process_rows: Vec<_> = rows
            .into_iter()
            .map(|(identity, area)| ProcessRowRegion { identity, area })
            .collect();
        let process_scroll_area = process_rows.first().map(|region| region.area);

        Self {
            process_rows,
            process_viewport: Some((0, 1)),
            process_scroll_area,
            input_mode,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn from_process_headers(
        headers: impl IntoIterator<Item = (ProcessSortField, Rect)>,
    ) -> Self {
        Self {
            process_headers: headers
                .into_iter()
                .map(|(field, area)| ProcessHeaderRegion { field, area })
                .collect(),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn from_service_rows(
        rows: impl IntoIterator<Item = (Arc<str>, Rect)>,
        input_mode: InputMode,
    ) -> Self {
        let service_rows: Vec<_> = rows
            .into_iter()
            .map(|(unit, area)| ServiceRowRegion { unit, area })
            .collect();
        let service_scroll_area = service_rows.first().map(|region| region.area);

        Self {
            service_rows,
            service_viewport: Some((0, 1)),
            service_scroll_area,
            input_mode,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn from_log_rows(
        rows: impl IntoIterator<Item = (u64, Rect)>,
        input_mode: InputMode,
    ) -> Self {
        let log_rows: Vec<_> = rows
            .into_iter()
            .map(|(id, area)| LogRowRegion { id, area })
            .collect();
        let log_scroll_area = log_rows.first().map(|region| region.area);

        Self {
            log_rows,
            log_viewport: Some((0, 1)),
            log_scroll_area,
            input_mode,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn from_network_rows(
        rows: impl IntoIterator<Item = (Arc<str>, Rect)>,
        input_mode: InputMode,
    ) -> Self {
        let network_rows: Vec<_> = rows
            .into_iter()
            .map(|(name, area)| NetworkRowRegion { name, area })
            .collect();
        let network_scroll_area = network_rows.first().map(|region| region.area);

        Self {
            network_rows,
            network_viewport: Some((0, 1)),
            network_scroll_area,
            input_mode,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn from_process_signal_buttons(cancel: Rect, confirm: Rect) -> Self {
        Self {
            process_signal_cancel: Some(cancel),
            process_signal_confirm: Some(confirm),
            input_mode: InputMode::ProcessSignalConfirm,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_input_mode(mut self, input_mode: InputMode) -> Self {
        self.input_mode = input_mode;
        self
    }
}

pub fn render(frame: &mut Frame, app: &App) -> UiRegions {
    let area = frame.area();
    frame.render_widget(Clear, area);
    if area.width == 0 || area.height == 0 {
        return UiRegions::default();
    }
    if !layout::terminal_size_supported(area) {
        render_terminal_size_warning(frame, area);
        return UiRegions::default();
    }

    let mut outer = Block::default().borders(Borders::ALL).title(" tuxctl ");
    if let Some(marker) = stale_marker(app, area.width) {
        outer = outer.title_top(
            Line::from(marker)
                .right_aligned()
                .style(Style::default().fg(Color::Yellow)),
        );
    }
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let screen = layout::screen(inner);
    let tab_areas = layout::tab_areas(screen.tabs);
    frame.render_widget(Clear, screen.tabs);
    render_tabs(
        frame,
        app.active_tab(),
        app.hovered(),
        &tab_areas,
        screen.tabs,
    );
    let content_render = render_content(frame, app, screen.content);

    let mut regions = UiRegions::from_tabs(tab_areas);
    regions.input_mode = app.input_mode();
    match content_render {
        ContentRender::Processes(process_render) => {
            regions.process_rows = process_render
                .rows
                .into_iter()
                .map(|(identity, area)| ProcessRowRegion { identity, area })
                .collect();
            regions.process_headers = process_render
                .headers
                .into_iter()
                .map(|(field, area)| ProcessHeaderRegion { field, area })
                .collect();
            regions.process_viewport = Some((process_render.start, process_render.height));
            regions.process_scroll_area = Some(process_render.scroll_area);
        }
        ContentRender::Services(service_render) => {
            regions.service_rows = service_render
                .rows
                .into_iter()
                .map(|(unit, area)| ServiceRowRegion { unit, area })
                .collect();
            regions.service_viewport = Some((service_render.start, service_render.height));
            regions.service_scroll_area = Some(service_render.scroll_area);
        }
        ContentRender::Logs(log_render) => {
            regions.log_rows = log_render
                .rows
                .into_iter()
                .map(|(id, area)| LogRowRegion { id, area })
                .collect();
            regions.log_viewport = Some((log_render.start, log_render.height));
            regions.log_scroll_area = Some(log_render.scroll_area);
        }
        ContentRender::Network(network_render) => {
            regions.network_rows = network_render
                .rows
                .into_iter()
                .map(|(name, area)| NetworkRowRegion { name, area })
                .collect();
            regions.network_viewport = Some((network_render.start, network_render.height));
            regions.network_scroll_area = Some(network_render.scroll_area);
        }
        ContentRender::None => {}
    }

    if app.process_detail_visible() {
        processes::render_detail(frame, app.selected_process(), area);
        regions.suppress_background_interaction();
    }
    if app.service_detail_visible() {
        services::render_detail(frame, app.selected_service(), area);
        regions.suppress_background_interaction();
    }
    if app.log_detail_visible() {
        logs::render_detail(frame, app.selected_log(), area);
        regions.suppress_background_interaction();
    }
    if app.network_detail_visible() {
        network::render_detail(frame, app.selected_network(), area);
        regions.suppress_background_interaction();
    }
    if let Some(confirmation) = app.process_signal_confirmation() {
        let (cancel_rect, confirm_rect) =
            processes::render_signal_confirmation(frame, confirmation, app.hovered(), area);
        regions.suppress_background_interaction();
        regions.process_signal_cancel = Some(cancel_rect);
        regions.process_signal_confirm = Some(confirm_rect);
    }
    if app.help_visible() {
        render_help(frame, area);
        regions.suppress_background_interaction();
        regions.process_signal_cancel = None;
        regions.process_signal_confirm = None;
    }

    regions
}

/// Names the collectors behind this screen whose data stopped updating, falling
/// back to a bare marker when the names would crowd the title.
fn stale_marker(app: &App, width: u16) -> Option<String> {
    let names: Vec<&str> = app.stale_collectors().map(Collector::label).collect();
    if names.is_empty() {
        return None;
    }
    let detailed = format!(" stale: {} ", names.join(", "));
    let room = usize::from(width).saturating_sub(" tuxctl ".len() + 4);
    Some(if detailed.chars().count() <= room {
        detailed
    } else {
        " stale ".to_owned()
    })
}

fn render_terminal_size_warning(frame: &mut Frame, area: Rect) {
    let width = area.width.min(36);
    let height = area.height.min(9);
    let warning_area = layout::centered_rect(area, width, height);
    if warning_area.width == 0 || warning_area.height == 0 {
        return;
    }

    let lines = vec![
        Line::from("tuxctl").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        Line::from("Terminal too small").style(Style::default().add_modifier(Modifier::BOLD)),
        Line::from(""),
        Line::from(format!(
            "Minimum: {}x{}",
            layout::MIN_TERMINAL_WIDTH,
            layout::MIN_TERMINAL_HEIGHT
        )),
        Line::from(format!("Current: {}x{}", area.width, area.height)),
        Line::from(""),
        Line::from("Resize the terminal to continue."),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        warning_area,
    );
}

fn render_tabs(
    frame: &mut Frame,
    active_tab: Tab,
    hovered: Option<&MouseTarget>,
    tabs: &[(Tab, Rect)],
    tabs_area: Rect,
) {
    for &(tab, area) in tabs {
        let style = if tab == active_tab {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if hovered == Some(&MouseTarget::Tab(tab)) {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };

        frame.render_widget(
            Paragraph::new(layout::tab_text(tab, area.width)).style(style),
            area,
        );
    }

    if tabs_area.width >= 75 {
        let hint_text = "1-5 Tabs   ? Help ";
        let hint_width = hint_text.len() as u16;
        let hint_x = tabs_area.width.saturating_sub(hint_width);
        if hint_x >= 48 {
            let hint_rect = Rect::new(
                tabs_area.x.saturating_add(hint_x),
                tabs_area.y,
                hint_width,
                1,
            );
            frame.render_widget(
                Paragraph::new(hint_text)
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Right),
                hint_rect,
            );
        }
    }
}

fn render_content(frame: &mut Frame, app: &App, area: Rect) -> ContentRender {
    let active_tab = app.active_tab();
    if active_tab == Tab::Overview {
        overview::render(frame, app, area);
        return ContentRender::None;
    }

    if active_tab == Tab::Processes {
        return ContentRender::Processes(processes::render(frame, app, area));
    }

    if active_tab == Tab::Services {
        return ContentRender::Services(services::render(frame, app, area));
    }

    if active_tab == Tab::Logs {
        return ContentRender::Logs(logs::render(frame, app, area));
    }

    if active_tab == Tab::Network {
        return ContentRender::Network(network::render(frame, app, area));
    }

    let content = Paragraph::new(vec![
        Line::from(format!("{} screen", active_tab.label())),
        Line::from(""),
        Line::from("Press ? for help"),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(content, layout::centered_rows(area, 3));
    ContentRender::None
}

pub(super) fn format_usage(usage: ByteUsage) -> String {
    format!(
        "{} / {}",
        format_bytes(usage.used),
        format_bytes(usage.total)
    )
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub(super) fn format_uptime(uptime: std::time::Duration) -> String {
    let total_minutes = uptime.as_secs() / 60;
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes / 60) % 24;
    let minutes = total_minutes % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn render_help(frame: &mut Frame, area: Rect) {
    let popup = layout::centered_rect(area, 64, 21);
    if popup.width == 0 || popup.height == 0 {
        return;
    }

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("General:").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from("  1-5                 Select tab"),
            Line::from("  Tab / Shift+Tab     Next / previous tab (or ← / →)"),
            Line::from("  ?                   Toggle help"),
            Line::from("  Esc                 Close popup / cancel search"),
            Line::from("  q / Ctrl+C          Quit application"),
            Line::from(""),
            Line::from("Navigation:").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from("  ↑/k ↓/j PgUp/PgDn   Move selection / scroll (mouse wheel)"),
            Line::from("  Home / End          Jump to top / bottom"),
            Line::from("  /                   Search / filter current view"),
            Line::from("  Enter               Open item details"),
            Line::from(""),
            Line::from("Screen Controls:").style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from("  Processes           c CPU, m MEM, p PID, n Name sort"),
            Line::from("  Signals             t terminate (SIGTERM), K kill (SIGKILL)"),
            Line::from("  Services            r refresh system services"),
            Line::from("  Logs                f follow, Space toggle pause"),
            Line::from(""),
            Line::from("Esc closes").style(Style::default().fg(Color::DarkGray)),
        ])
        .block(Block::default().borders(Borders::ALL).title(" Help ")),
        popup,
    );
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    let column = u32::from(column);
    let row = u32::from(row);
    let left = u32::from(area.x);
    let top = u32::from(area.y);
    let right = left + u32::from(area.width);
    let bottom = top + u32::from(area.height);

    column >= left && column < right && row >= top && row < bottom
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::action::Action;
    use crate::linux::{
        ByteUsage, LogicalCpuId, LogicalCpuMetrics, ProcessIdentity, SystemMetrics,
    };

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn process_confirmation_app() -> App {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes: vec![crate::linux::ProcessInfo {
                pid: 1234,
                name: "testproc".into(),
                cpu_percent: Some(5.0),
                memory_bytes: 4096,
                command: Some("/bin/testproc".into()),
                state: "R (running)".into(),
                parent_pid: 1,
                state_code: 'R',
                start_time: 100,
                kernel_thread: false,
            }],
            error: None,
        }));
        app.update(Action::RequestProcessSignal(
            crate::linux::ProcessSignal::Term,
        ));
        app
    }

    fn rendered_regions(terminal: &mut Terminal<TestBackend>, app: &App) -> UiRegions {
        let mut regions = UiRegions::default();
        terminal
            .draw(|frame| {
                regions = render(frame, app);
            })
            .unwrap();
        regions
    }

    #[test]
    fn hit_testing_includes_top_left_and_excludes_bottom_right() {
        let regions = UiRegions::from_tabs([(Tab::Overview, Rect::new(10, 5, 8, 2))]);

        assert_eq!(
            regions.target_at(10, 5),
            Some(MouseTarget::Tab(Tab::Overview))
        );
        assert_eq!(
            regions.target_at(17, 6),
            Some(MouseTarget::Tab(Tab::Overview))
        );
        assert_eq!(regions.target_at(18, 6), None);
        assert_eq!(regions.target_at(17, 7), None);
    }

    #[test]
    fn hit_testing_does_not_overflow_at_terminal_limits() {
        let last_cell = u16::MAX - 1;
        let regions = UiRegions::from_tabs([(Tab::Network, Rect::new(last_cell, last_cell, 1, 1))]);

        assert_eq!(
            regions.target_at(last_cell, last_cell),
            Some(MouseTarget::Tab(Tab::Network))
        );
    }

    #[test]
    fn process_header_hit_testing_excludes_borders_and_spacing() {
        let regions = UiRegions::from_process_headers([
            (ProcessSortField::Pid, Rect::new(2, 4, 8, 1)),
            (ProcessSortField::Name, Rect::new(12, 4, 16, 1)),
        ]);

        assert_eq!(regions.target_at(1, 4), None);
        assert_eq!(
            regions.target_at(2, 4),
            Some(MouseTarget::ProcessSortHeader(ProcessSortField::Pid))
        );
        assert_eq!(regions.target_at(10, 4), None);
        assert_eq!(regions.target_at(11, 4), None);
        assert_eq!(regions.target_at(12, 3), None);
        assert_eq!(
            regions.target_at(12, 4),
            Some(MouseTarget::ProcessSortHeader(ProcessSortField::Name))
        );
        assert_eq!(regions.target_at(12, 5), None);
    }

    #[test]
    fn process_row_hit_testing_excludes_adjacent_lines() {
        let identity = ProcessIdentity {
            pid: 123,
            start_time: 456,
        };
        let regions =
            UiRegions::from_process_rows([(identity, Rect::new(2, 6, 40, 1))], InputMode::Normal);

        assert_eq!(regions.target_at(1, 6), None);
        assert_eq!(regions.target_at(2, 5), None);
        assert_eq!(
            regions.target_at(2, 6),
            Some(MouseTarget::ProcessRow(identity))
        );
        assert_eq!(regions.target_at(42, 6), None);
        assert_eq!(regions.target_at(2, 7), None);
    }

    #[test]
    fn service_row_hit_testing_uses_unit_identity_and_excludes_borders() {
        let regions = UiRegions::from_service_rows(
            [(Arc::from("dbus.service"), Rect::new(2, 6, 40, 1))],
            InputMode::Services,
        );

        assert_eq!(regions.target_at(1, 6), None);
        assert_eq!(
            regions.target_at(2, 6),
            Some(MouseTarget::ServiceRow(Arc::from("dbus.service")))
        );
        assert_eq!(regions.target_at(42, 6), None);
        assert_eq!(regions.target_at(2, 7), None);
    }

    #[test]
    fn log_row_hit_testing_uses_entry_identity_and_excludes_borders() {
        let regions = UiRegions::from_log_rows([(77, Rect::new(2, 6, 40, 1))], InputMode::Logs);

        assert_eq!(regions.target_at(1, 6), None);
        assert_eq!(regions.target_at(2, 6), Some(MouseTarget::LogRow(77)));
        assert_eq!(regions.target_at(42, 6), None);
        assert_eq!(regions.target_at(2, 7), None);
    }

    #[test]
    fn formats_uptime_at_day_hour_and_minute_boundaries() {
        let uptime = std::time::Duration::from_secs(3 * 86_400 + 14 * 3_600 + 22 * 60);

        assert_eq!(format_uptime(uptime), "3d 14h 22m");
        assert_eq!(
            format_uptime(std::time::Duration::from_secs(65 * 60)),
            "1h 5m"
        );
        assert_eq!(format_uptime(std::time::Duration::from_secs(59)), "0m");
    }

    #[test]
    fn formats_bytes_with_binary_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(10 * 1024 * 1024 * 1024), "10.0 GiB");
    }

    #[test]
    fn network_row_hit_testing_uses_interface_name_and_excludes_borders() {
        let regions = UiRegions::from_network_rows(
            [(Arc::from("enp6s0"), Rect::new(2, 6, 40, 1))],
            InputMode::Network,
        );

        assert_eq!(regions.target_at(1, 6), None);
        assert_eq!(
            regions.target_at(2, 6),
            Some(MouseTarget::NetworkRow(Arc::from("enp6s0")))
        );
        assert_eq!(regions.target_at(42, 6), None);
        assert_eq!(regions.target_at(2, 7), None);
    }

    #[test]
    fn modal_suppression_clears_background_targets_but_preserves_viewports() {
        let identity = ProcessIdentity {
            pid: 42,
            start_time: 9001,
        };
        let target_area = Rect::new(2, 6, 20, 1);
        let mut regions = UiRegions {
            tabs: vec![TabRegion {
                tab: Tab::Overview,
                area: target_area,
            }],
            process_rows: vec![ProcessRowRegion {
                identity,
                area: target_area,
            }],
            process_headers: vec![ProcessHeaderRegion {
                field: ProcessSortField::Cpu,
                area: target_area,
            }],
            process_scroll_area: Some(target_area),
            process_viewport: Some((3, 7)),
            service_rows: vec![ServiceRowRegion {
                unit: Arc::from("dbus.service"),
                area: target_area,
            }],
            service_scroll_area: Some(target_area),
            service_viewport: Some((4, 8)),
            log_rows: vec![LogRowRegion {
                id: 77,
                area: target_area,
            }],
            log_scroll_area: Some(target_area),
            log_viewport: Some((5, 9)),
            network_rows: vec![NetworkRowRegion {
                name: Arc::from("enp6s0"),
                area: target_area,
            }],
            network_scroll_area: Some(target_area),
            network_viewport: Some((6, 10)),
            process_signal_cancel: None,
            process_signal_confirm: None,
            input_mode: InputMode::ProcessSignalConfirm,
        };

        regions.suppress_background_interaction();

        assert!(regions.tabs.is_empty());
        assert!(regions.process_rows.is_empty());
        assert!(regions.process_headers.is_empty());
        assert!(regions.process_scroll_area.is_none());
        assert!(regions.service_rows.is_empty());
        assert!(regions.service_scroll_area.is_none());
        assert!(regions.log_rows.is_empty());
        assert!(regions.log_scroll_area.is_none());
        assert!(regions.network_rows.is_empty());
        assert!(regions.network_scroll_area.is_none());
        assert_eq!(regions.process_viewport(), Some((3, 7)));
        assert_eq!(regions.service_viewport(), Some((4, 8)));
        assert_eq!(regions.log_viewport(), Some((5, 9)));
        assert_eq!(regions.network_viewport(), Some((6, 10)));
        assert_eq!(regions.target_at(2, 6), None);

        let cancel = Rect::new(10, 12, 12, 1);
        let confirm = Rect::new(26, 12, 15, 1);
        regions.process_signal_cancel = Some(cancel);
        regions.process_signal_confirm = Some(confirm);
        assert_eq!(
            regions.target_at(cancel.x, cancel.y),
            Some(MouseTarget::ProcessSignalCancel)
        );
        assert_eq!(
            regions.target_at(confirm.x, confirm.y),
            Some(MouseTarget::ProcessSignalConfirm)
        );
    }

    fn populated_app(tab: Tab) -> App {
        let mut app = App::default();
        app.update(Action::SystemMetricsUpdated(SystemMetrics {
            memory: Some(ByteUsage {
                used: 4 << 30,
                total: 16 << 30,
            }),
            root_filesystem: Some(ByteUsage {
                used: 50 << 30,
                total: 100 << 30,
            }),
            ..SystemMetrics::default()
        }));
        app.update(Action::NetworkUpdated(crate::linux::NetworkSnapshot {
            interfaces: vec![crate::linux::NetworkInterfaceInfo {
                name: "lo".into(),
                operstate: crate::linux::OperState::Unknown,
                mac_address: None,
                mtu: Some(65536),
                ipv4_addresses: Vec::new(),
                ipv6_addresses: Vec::new(),
                rx_bytes: 0,
                tx_bytes: 0,
                rx_packets: 0,
                tx_packets: 0,
                rx_errors: 0,
                tx_errors: 0,
                rx_dropped: 0,
                tx_dropped: 0,
                rx_rate_bytes_per_sec: None,
                tx_rate_bytes_per_sec: None,
            }],
            error: None,
        }));
        app.update(Action::LogsUpdated(crate::linux::JournalBatch {
            entries: vec![crate::linux::JournalEntry {
                id: 1,
                timestamp_micros: Some(1_000_000),
                local_time: None,
                source: "sshd.service".into(),
                priority: Some(4),
                message: "warning message".into(),
            }],
            dropped: 0,
            error: None,
        }));
        app.update(Action::SelectTab(tab));
        app
    }

    #[test]
    fn stale_marker_is_shown_in_the_frame_without_hiding_search_state() {
        let mut app = App::default().with_collector_periods(
            crate::app::CollectorPeriods::for_sampling_interval(std::time::Duration::from_secs(1)),
        );
        app.update(Action::SelectTab(Tab::Processes));
        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('x'));
        let start = std::time::Instant::now();
        app.update(Action::Tick(start));
        app.update(Action::Tick(start + std::time::Duration::from_secs(10)));

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        rendered_regions(&mut terminal, &app);
        let text = buffer_text(&terminal);
        assert!(text.contains(" stale: metrics, processes "));
        assert!(text.contains("Search: x_"));

        app.update(Action::SelectTab(Tab::Overview));
        let mut narrow = Terminal::new(TestBackend::new(40, 15)).unwrap();
        rendered_regions(&mut narrow, &app);
        let text = buffer_text(&narrow);
        assert!(
            text.contains(" stale "),
            "three names fall back to a bare marker"
        );
        assert!(text.contains(" tuxctl "));
    }

    fn overview_sweep_app(cpu_count: u32) -> App {
        let mut app = App::default();
        app.update(Action::SystemMetricsUpdated(SystemMetrics {
            cpu_percent: Some(21.0),
            logical_cpus: (0..cpu_count)
                .map(|index| LogicalCpuMetrics {
                    id: LogicalCpuId::for_test(index),
                    utilization_percent: Some(f64::from(index % 100)),
                })
                .collect(),
            memory: Some(ByteUsage {
                used: 4 << 30,
                total: 16 << 30,
            }),
            root_filesystem: Some(ByteUsage {
                used: 50 << 30,
                total: 100 << 30,
            }),
            ..SystemMetrics::default()
        }));
        let module = crate::linux::MemoryModule {
            locator: None,
            capacity_bytes: 8 << 30,
            memory_type: Some("DDR5".into()),
            speed_mts: Some(5600),
            manufacturer: None,
            part_number: None,
        };
        app.update(Action::HardwareDiscovered(
            crate::linux::HardwareInventory {
                cpus: vec![crate::linux::CpuPackage {
                    physical_id: Some(0),
                    model: "Test Processor".into(),
                }],
                memory_modules: vec![module.clone(), module],
                gpus: vec![crate::linux::GpuDevice {
                    model: "Test Graphics".into(),
                    kind: None,
                    vram_bytes: None,
                }],
                storage_devices: vec![crate::linux::StorageDevice {
                    system_name: "nvme0n1".into(),
                    kind: crate::linux::StorageKind::Nvme,
                    model: Some("Test Disk".into()),
                    capacity_bytes: Some(1_000_000_000_000),
                }],
                network_devices: vec![crate::linux::NetworkDevice {
                    interface_name: "eth0".into(),
                    model: None,
                }],
            },
        ));
        app.update(Action::NetworkUpdated(crate::linux::NetworkSnapshot {
            interfaces: vec![crate::linux::NetworkInterfaceInfo {
                name: "eth0".into(),
                operstate: crate::linux::OperState::Up,
                mac_address: None,
                mtu: None,
                ipv4_addresses: Vec::new(),
                ipv6_addresses: Vec::new(),
                rx_bytes: 0,
                tx_bytes: 0,
                rx_packets: 0,
                tx_packets: 0,
                rx_errors: 0,
                tx_errors: 0,
                rx_dropped: 0,
                tx_dropped: 0,
                rx_rate_bytes_per_sec: None,
                tx_rate_bytes_per_sec: None,
            }],
            error: None,
        }));
        app
    }

    /// Content rows inside the Hardware panel, located by its title.
    fn hardware_panel_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let rows: Vec<Vec<&str>> = buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let Some((top, left)) = rows.iter().enumerate().find_map(|(y, row)| {
            let line: String = row.concat();
            let title = line.find(" Hardware ")?;
            let title_column = line[..title].chars().count();
            let left = (0..title_column).rev().find(|&x| row[x] == "┌")?;
            Some((y, left))
        }) else {
            return Vec::new();
        };
        let right = (left + 1..width).find(|&x| rows[top][x] == "┐").unwrap();
        rows[top + 1..]
            .iter()
            .take_while(|row| row[left] != "└")
            .map(|row| row[left + 1..right].concat())
            .collect()
    }

    fn shown_cpu_labels(rows: &[String]) -> Vec<u32> {
        let mut labels = Vec::new();
        for row in rows {
            let mut rest = row.as_str();
            while let Some(position) = rest.find("CPU") {
                rest = &rest[position + 3..];
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(index) = digits.parse() {
                    labels.push(index);
                }
            }
        }
        labels
    }

    fn more_cpus(rows: &[String]) -> usize {
        rows.iter()
            .find_map(|row| {
                row.trim()
                    .strip_prefix("… ")?
                    .strip_suffix(" more logical CPUs")?
                    .parse()
                    .ok()
            })
            .unwrap_or(0)
    }

    #[test]
    fn overview_resize_sweep_keeps_cpu_counts_and_sections_consistent() {
        for cpu_count in [12, 64] {
            let app = overview_sweep_app(cpu_count);
            for width in [40, 50, 60, 89, 90, 120, 160] {
                let mut previous_shown = 0;
                for height in layout::MIN_TERMINAL_HEIGHT..=60 {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    rendered_regions(&mut terminal, &app);
                    let rows = hardware_panel_rows(&terminal);
                    let context =
                        format!("{cpu_count} CPUs at {width}x{height}:\n{}", rows.join("\n"));
                    if rows.is_empty() {
                        continue;
                    }

                    let shown = shown_cpu_labels(&rows);
                    let mut distinct = shown.clone();
                    distinct.sort_unstable();
                    distinct.dedup();
                    assert_eq!(distinct.len(), shown.len(), "{context}");
                    assert_eq!(
                        shown.len() + more_cpus(&rows),
                        cpu_count as usize,
                        "shown + overflow must equal the CPU count; {context}"
                    );
                    assert!(
                        shown.len() >= previous_shown,
                        "visible CPUs decreased as height grew; {context}"
                    );
                    previous_shown = shown.len();

                    for (index, row) in rows.iter().enumerate() {
                        if ["RAM", "GPU", "STORAGE", "NETWORK", "CPU"].contains(&row.trim()) {
                            let next = rows.get(index + 1).map_or("", |next| next.trim());
                            assert!(
                                !next.is_empty(),
                                "section heading without content; {context}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn dense_cpu_cells_keep_a_separator_after_the_label() {
        let app = overview_sweep_app(64);
        for height in layout::MIN_TERMINAL_HEIGHT..=60 {
            let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
            rendered_regions(&mut terminal, &app);
            for row in hardware_panel_rows(&terminal) {
                let mut rest = row.as_str();
                while let Some(position) = rest.find("CPU") {
                    rest = &rest[position + 3..];
                    let after_digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                    if after_digits.len() != rest.len() {
                        assert!(
                            after_digits.starts_with(' ') || after_digits.is_empty(),
                            "CPU label runs into its value at 40x{height}: {row}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn tab_labels_never_touch_and_hit_regions_match_labels() {
        let app = App::default();
        for width in 40..=130 {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            let regions = rendered_regions(&mut terminal, &app);
            let tab_row: Vec<String> = terminal
                .backend()
                .buffer()
                .content()
                .chunks(usize::from(width))
                .nth(1)
                .unwrap()
                .iter()
                .map(|cell| cell.symbol().to_owned())
                .collect();

            assert_eq!(regions.tabs.len(), Tab::ALL.len(), "at width {width}");
            for pair in regions.tabs.windows(2) {
                let (left, right) = (pair[0].area, pair[1].area);
                let left_label = tab_row[usize::from(left.x)..usize::from(left.right())].concat();
                let right_label =
                    tab_row[usize::from(right.x)..usize::from(right.right())].concat();
                assert!(
                    left_label.ends_with(' ')
                        || right_label.starts_with(' ')
                        || right.x > left.right(),
                    "tab labels touch at width {width}: {left_label:?} {right_label:?}"
                );
            }
            for region in &regions.tabs {
                let label =
                    tab_row[usize::from(region.area.x)..usize::from(region.area.right())].concat();
                assert!(
                    !label.trim().is_empty(),
                    "empty tab hit region at width {width}"
                );
                assert_eq!(
                    regions.target_at(region.area.x, region.area.y),
                    Some(MouseTarget::Tab(region.tab))
                );
            }
        }
    }

    /// The rendered row containing `persistent` (the search or filter text).
    fn status_row(app: &App, width: u16, persistent: &str) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        rendered_regions(&mut terminal, app);
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .find(|row| row.contains(persistent))
            .unwrap_or_else(|| panic!("{persistent:?} is not visible at width {width}"))
    }

    fn process_info(pid: u32, name: &str) -> crate::linux::ProcessInfo {
        crate::linux::ProcessInfo {
            pid,
            name: name.into(),
            cpu_percent: None,
            memory_bytes: 0,
            command: None,
            state: "S".into(),
            parent_pid: 1,
            state_code: 'S',
            start_time: u64::from(pid),
            kernel_thread: false,
        }
    }

    fn process_snapshot(processes: Vec<crate::linux::ProcessInfo>) -> Action {
        Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes,
            error: None,
        })
    }

    /// Processes tab with a committed filter "ss" (typed, detail opened and closed).
    fn filtered_processes_app() -> App {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Processes));
        app.update(process_snapshot(vec![
            process_info(1, "sshd"),
            process_info(2, "init"),
        ]));
        app.update(Action::BeginProcessSearch);
        app.update(Action::AppendProcessSearch('s'));
        app.update(Action::AppendProcessSearch('s'));
        app.update(Action::OpenProcessDetails);
        app.update(Action::Escape);
        assert_eq!(app.input_mode(), InputMode::Normal);
        app
    }

    #[test]
    fn process_filter_stays_visible_with_an_action_message() {
        let mut app = filtered_processes_app();
        app.update(Action::RequestProcessSignal(
            crate::linux::ProcessSignal::Term,
        ));
        app.update(process_snapshot(vec![process_info(2, "init")]));
        assert!(app.process_action_message().is_some());

        status_row(&app, 40, "Filter: \"ss\"");
        let row = status_row(&app, 120, "Filter: \"ss\"");
        assert!(row.contains("exited before signal"), "{row}");
    }

    #[test]
    fn process_filter_stays_visible_with_a_refresh_error() {
        let mut app = filtered_processes_app();
        app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
            processes: Vec::new(),
            error: Some("proc unavailable".into()),
        }));

        status_row(&app, 40, "Filter: \"ss\"");
        let row = status_row(&app, 120, "Filter: \"ss\"");
        assert!(row.contains("Refresh error: proc unavailable"), "{row}");
    }

    #[test]
    fn service_search_and_filter_stay_visible_while_refreshing_or_failing() {
        let mut app = App::default();
        app.update(Action::SelectTab(Tab::Services));
        assert!(app.service_refreshing());
        app.update(Action::BeginServiceSearch);
        app.update(Action::AppendServiceSearch('x'));
        status_row(&app, 40, "Search: x_");
        let row = status_row(&app, 120, "Search: x_");
        assert!(row.contains("Refreshing system services"), "{row}");

        let generation = app.take_service_refresh_request().unwrap();
        app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
            services: vec![crate::linux::ServiceInfo {
                unit: "xyz.service".into(),
                load_state: "loaded".into(),
                active_state: "active".into(),
                sub_state: "running".into(),
                description: "X".into(),
            }],
            error: None,
            completed_refresh_generation: generation,
        }));
        app.update(Action::OpenServiceDetails);
        app.update(Action::Escape);
        app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
            error: Some("bus unavailable".into()),
            ..Default::default()
        }));
        status_row(&app, 40, "Filter: \"x\"");
        let row = status_row(&app, 120, "Filter: \"x\"");
        assert!(row.contains("Refresh error: bus unavailable"), "{row}");
    }

    #[test]
    fn logs_header_shows_the_full_priority_label() {
        let app = populated_app(Tab::Logs);
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        rendered_regions(&mut terminal, &app);

        let text = buffer_text(&terminal);
        assert!(text.contains("PRIORITY"), "priority header was truncated");
        assert!(text.contains("warning"));
    }

    #[test]
    fn overview_filesystem_row_keeps_label_and_bar_separate() {
        let app = populated_app(Tab::Overview);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        rendered_regions(&mut terminal, &app);

        let lines: Vec<String> = terminal
            .backend()
            .buffer()
            .content()
            .chunks(120)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let heading = lines
            .iter()
            .position(|line| line.contains("Filesystem"))
            .expect("filesystem heading");
        let row = &lines[heading + 1];
        let usage = format_usage(ByteUsage {
            used: 50 << 30,
            total: 100 << 30,
        });
        assert!(row.contains("/  50%  █"), "row: {row}");
        assert!(row.contains(&format!("░  {usage}")), "row: {row}");
    }

    #[test]
    fn loopback_unknown_state_renders_neutrally() {
        let app = populated_app(Tab::Network);
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        rendered_regions(&mut terminal, &app);

        let text = buffer_text(&terminal);
        assert!(text.contains("◌ unknown"));
        assert!(!text.contains("UNKNOWN"));
    }

    #[test]
    fn every_tab_renders_populated_data_at_the_minimum_size() {
        for tab in [
            Tab::Overview,
            Tab::Processes,
            Tab::Services,
            Tab::Logs,
            Tab::Network,
        ] {
            let app = populated_app(tab);
            for (width, height) in [
                (layout::MIN_TERMINAL_WIDTH, layout::MIN_TERMINAL_HEIGHT),
                (
                    layout::MIN_TERMINAL_WIDTH + 1,
                    layout::MIN_TERMINAL_HEIGHT + 1,
                ),
            ] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let regions = rendered_regions(&mut terminal, &app);
                assert!(!regions.tabs.is_empty(), "{tab:?} at {width}x{height}");
            }
        }
    }

    #[test]
    fn implemented_screens_use_the_warning_path_in_tiny_terminals() {
        for tab in [
            Tab::Overview,
            Tab::Processes,
            Tab::Services,
            Tab::Logs,
            Tab::Network,
        ] {
            let mut app = App::default();
            app.update(Action::SelectTab(tab));

            for (width, height) in [(1, 1), (2, 2), (10, 3)] {
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend).unwrap();
                assert!(rendered_regions_are_empty(&mut terminal, &app));
            }
        }
    }

    fn rendered_regions_are_empty(terminal: &mut Terminal<TestBackend>, app: &App) -> bool {
        let mut empty = false;
        terminal
            .draw(|frame| {
                empty = render(frame, app).tabs.is_empty();
            })
            .unwrap();
        empty
    }

    #[test]
    fn minimum_terminal_size_render_boundary_is_global() {
        let app = App::default();
        for (width, height) in [(39, 15), (40, 14), (39, 14)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            assert!(rendered_regions_are_empty(&mut terminal, &app));
            let text = buffer_text(&terminal);
            assert!(text.contains("Terminal too small"));
            assert!(text.contains(&format!("Current: {width}x{height}")));
        }

        let backend = TestBackend::new(40, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        assert!(!rendered_regions_are_empty(&mut terminal, &app));
        assert!(!buffer_text(&terminal).contains("Terminal too small"));
    }

    #[test]
    fn minimum_terminal_warning_is_safe_at_pathological_sizes() {
        let app = App::default();
        for (width, height) in [(1, 1), (1, 40), (40, 1), (2, 2)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            assert!(rendered_regions_are_empty(&mut terminal, &app));
        }
    }

    #[test]
    fn resize_transitions_clear_warning_and_normal_content() {
        let app = App::default();
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        assert!(!rendered_regions_are_empty(&mut terminal, &app));
        assert!(buffer_text(&terminal).contains("Processes"));

        terminal.backend_mut().resize(39, 40);
        terminal.autoresize().unwrap();
        assert!(rendered_regions_are_empty(&mut terminal, &app));
        let warning = buffer_text(&terminal);
        assert!(warning.contains("Terminal too small"));
        assert!(!warning.contains("Processes"));

        terminal.backend_mut().resize(80, 40);
        terminal.autoresize().unwrap();
        assert!(!rendered_regions_are_empty(&mut terminal, &app));
        let normal = buffer_text(&terminal);
        assert!(normal.contains("Processes"));
        assert!(!normal.contains("Terminal too small"));
    }

    #[test]
    fn process_confirmation_regions_are_rebuilt_after_resize() {
        let app = process_confirmation_app();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let old_regions = rendered_regions(&mut terminal, &app);
        let old_confirm = old_regions.process_signal_confirm.unwrap();
        let old_viewport = old_regions.process_viewport();

        terminal.backend_mut().resize(50, 16);
        terminal.autoresize().unwrap();
        let new_regions = rendered_regions(&mut terminal, &app);
        let new_cancel = new_regions.process_signal_cancel.unwrap();
        let new_confirm = new_regions.process_signal_confirm.unwrap();

        assert_ne!(new_confirm, old_confirm);
        assert_ne!(new_regions.process_viewport(), old_viewport);
        assert_eq!(
            new_regions.target_at(new_cancel.x, new_cancel.y),
            Some(MouseTarget::ProcessSignalCancel)
        );
        assert_eq!(
            new_regions.target_at(new_confirm.x, new_confirm.y),
            Some(MouseTarget::ProcessSignalConfirm)
        );
        assert_eq!(
            new_regions.target_at(old_confirm.x, old_confirm.y),
            None,
            "the prior confirmation coordinates must not remain active"
        );
        assert!(new_regions.tabs.is_empty());
        assert!(new_regions.process_rows.is_empty());
        assert!(new_regions.process_headers.is_empty());
        assert!(new_regions.process_scroll_area.is_none());
    }

    #[test]
    fn modal_resize_crosses_each_minimum_terminal_boundary() {
        let mut app = process_confirmation_app();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        for (width, height) in [
            (layout::MIN_TERMINAL_WIDTH - 1, layout::MIN_TERMINAL_HEIGHT),
            (layout::MIN_TERMINAL_WIDTH, layout::MIN_TERMINAL_HEIGHT - 1),
            (
                layout::MIN_TERMINAL_WIDTH - 1,
                layout::MIN_TERMINAL_HEIGHT - 1,
            ),
        ] {
            let valid_regions = rendered_regions(&mut terminal, &app);
            assert!(valid_regions.process_signal_confirm.is_some());
            assert!(buffer_text(&terminal).contains("Terminate Process"));

            terminal.backend_mut().resize(width, height);
            terminal.autoresize().unwrap();
            assert!(app.update(Action::Resize));
            assert!(app.process_signal_confirmation().is_some());
            let small_regions = rendered_regions(&mut terminal, &app);
            assert!(small_regions.process_signal_confirm.is_none());
            assert!(small_regions.tabs.is_empty());
            assert!(buffer_text(&terminal).contains("Terminal too small"));

            terminal.backend_mut().resize(80, 24);
            terminal.autoresize().unwrap();
            assert!(app.update(Action::Resize));
            assert!(app.process_signal_confirmation().is_some());
            let restored_regions = rendered_regions(&mut terminal, &app);
            assert!(restored_regions.process_signal_confirm.is_some());
            assert!(restored_regions.tabs.is_empty());
            let restored = buffer_text(&terminal);
            assert!(restored.contains("Terminate Process"));
            assert!(!restored.contains("Terminal too small"));
        }
    }

    #[test]
    fn overview_dashboard_renders_cached_data_across_responsive_sizes() {
        let mut app = App::default();
        let logical_cpus = (0..64)
            .map(|index| LogicalCpuMetrics {
                id: LogicalCpuId::for_test(index),
                utilization_percent: Some(f64::from(index % 101)),
            })
            .collect::<Vec<_>>();

        for sample in 0..65 {
            let mut metrics = SystemMetrics {
                cpu_percent: Some(f64::from(sample)),
                logical_cpus: logical_cpus.clone(),
                memory: Some(ByteUsage {
                    used: 8 * 1024 * 1024 * 1024,
                    total: 32 * 1024 * 1024 * 1024,
                }),
                uptime: Some(std::time::Duration::from_secs(90_000)),
                root_filesystem: Some(ByteUsage {
                    used: 120 * 1024 * 1024 * 1024,
                    total: 500 * 1024 * 1024 * 1024,
                }),
                ..SystemMetrics::default()
            };
            metrics.system_identity.hostname = Some("build-host".into());
            metrics.system_identity.kernel_release = Some("6.12.0-tuxctl".into());
            app.update(Action::SystemMetricsUpdated(metrics));
        }

        for (width, height) in [
            (180, 50),
            (120, 40),
            (90, 40),
            (80, 40),
            (46, 50),
            (46, 60),
            (40, 40),
            (40, 15),
        ] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, &app);
                })
                .unwrap();

            let buffer = buffer_text(&terminal);
            assert!(!buffer.contains("Dashboard"));

            let rows = terminal
                .backend()
                .buffer()
                .content()
                .chunks(usize::from(width))
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();

            // Row 0 is outer border, Row 1 is tab bar, Row 2 begins System content directly below tabs.
            assert!(rows[1].contains("Overview") || rows[1].contains(" Ovr "));
            assert!(rows[2].contains("System"));
            assert!(!rows[2].contains("Overview"));
            assert!(!rows[2].trim().is_empty());
            assert!(!rows[3].trim().is_empty());

            let content_width = width.saturating_sub(2);
            if content_width >= 90 {
                assert!(rows[2].contains("Hardware"));
            } else {
                assert!(rows.iter().skip(3).any(|row| row.contains("Hardware")));
            }
        }
    }

    #[test]
    fn data_screens_start_with_meaningful_content_below_tabs_at_responsive_sizes() {
        for (tab, marker) in [
            (Tab::Processes, "0 processes"),
            (Tab::Services, "0 services"),
            (Tab::Logs, "0 entries"),
            (Tab::Network, "0 interfaces"),
        ] {
            let mut app = App::default();
            app.update(Action::SelectTab(tab));
            if let Some(generation) = app.take_service_refresh_request() {
                // Settle the refresh that entering Services requests.
                app.update(Action::ServicesUpdated(crate::linux::ServiceSnapshot {
                    completed_refresh_generation: generation,
                    ..Default::default()
                }));
            }

            for (width, height) in [(40, 15), (46, 20), (80, 24), (120, 40), (180, 50)] {
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend).unwrap();
                let regions = rendered_regions(&mut terminal, &app);
                let rows = terminal
                    .backend()
                    .buffer()
                    .content()
                    .chunks(usize::from(width))
                    .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                    .collect::<Vec<_>>();

                assert!(rows[1].contains(tab.label()) || rows[1].contains(tab.short_label()));
                assert!(
                    rows[2].contains(marker),
                    "{tab:?} at {width}x{height} did not start with {marker:?}"
                );
                assert!(!rows[2].contains("Terminal too small"));

                let viewport = match tab {
                    Tab::Processes => regions.process_viewport(),
                    Tab::Services => regions.service_viewport(),
                    Tab::Logs => regions.log_viewport(),
                    Tab::Network => regions.network_viewport(),
                    Tab::Overview => unreachable!(),
                };
                assert!(viewport.is_some_and(|(_, height)| height > 0));
            }
        }
    }

    #[test]
    fn removing_redundant_screen_titles_reclaims_a_table_row() {
        let mut processes = App::default();
        processes.update(Action::SelectTab(Tab::Processes));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let regions = rendered_regions(&mut terminal, &processes);

        assert_eq!(regions.process_viewport(), Some((0, 18)));
        assert_eq!(regions.process_scroll_area.unwrap().y, 5);

        let mut services = App::default();
        services.update(Action::SelectTab(Tab::Services));
        let regions = rendered_regions(&mut terminal, &services);

        assert_eq!(regions.service_viewport(), Some((0, 19)));
        assert_eq!(regions.service_scroll_area.unwrap().y, 4);
    }

    #[test]
    fn process_signal_button_hit_testing() {
        let regions = UiRegions::from_process_signal_buttons(
            Rect::new(10, 5, 12, 1),
            Rect::new(26, 5, 15, 1),
        );

        assert_eq!(regions.target_at(9, 5), None);
        assert_eq!(
            regions.target_at(10, 5),
            Some(MouseTarget::ProcessSignalCancel)
        );
        assert_eq!(
            regions.target_at(21, 5),
            Some(MouseTarget::ProcessSignalCancel)
        );
        assert_eq!(regions.target_at(22, 5), None);

        assert_eq!(
            regions.target_at(26, 5),
            Some(MouseTarget::ProcessSignalConfirm)
        );
        assert_eq!(
            regions.target_at(40, 5),
            Some(MouseTarget::ProcessSignalConfirm)
        );
        assert_eq!(regions.target_at(41, 5), None);
        assert_eq!(regions.target_at(10, 6), None);
    }

    #[test]
    fn process_signal_modal_renders_in_all_terminal_sizes() {
        for signal in [
            crate::linux::ProcessSignal::Term,
            crate::linux::ProcessSignal::Kill,
        ] {
            let mut app = App::default();
            app.update(Action::SelectTab(Tab::Processes));
            app.update(Action::ProcessesUpdated(crate::linux::ProcessSnapshot {
                processes: vec![crate::linux::ProcessInfo {
                    pid: 1234,
                    name: "testproc".into(),
                    cpu_percent: Some(5.0),
                    memory_bytes: 4096,
                    command: Some("/bin/testproc".into()),
                    state: "R (running)".into(),
                    parent_pid: 1,
                    state_code: 'R',
                    start_time: 100,
                    kernel_thread: false,
                }],
                error: None,
            }));
            app.update(Action::RequestProcessSignal(signal));

            for (width, height) in [(1, 1), (5, 5), (20, 8), (60, 15), (120, 40)] {
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend).unwrap();
                terminal
                    .draw(|frame| {
                        render(frame, &app);
                    })
                    .unwrap();
            }
        }
    }

    #[test]
    fn tabs_hint_includes_horizontal_offset() {
        let app = App::default();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let line_chars = (81..99)
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        assert_eq!(line_chars, "1-5 Tabs   ? Help ");
    }
}
