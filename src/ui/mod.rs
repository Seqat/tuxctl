//! Rendering. `render` draws one frame from the cached `App` state and returns
//! the `UiRegions`, where tabs, rows and buttons ended up, which the event
//! handler uses to turn mouse coordinates into targets. Drawing never reads the
//! system, and the finished frame passes through `sanitize` before it reaches
//! the terminal.

mod cards;
mod hardware;
mod hardware_cpu;
mod hardware_network_summary;
mod layout;
mod logs;
mod menu;
mod network;
mod overview;
mod processes;
mod sanitize;
mod services;
mod sparkline;
mod status;
mod theme;

pub use theme::{init_color_depth, ColorDepth};

use std::sync::Arc;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    action::{InputMode, IntervalStep, MenuItem, MouseTarget, PinMove, ProcessSortField, Tab},
    app::{App, Collector},
    linux::NvidiaAccess,
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
    menu_items: Vec<(MenuItem, Rect)>,
    interval_buttons: Vec<(IntervalStep, Rect)>,
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
    /// ▲/▼ controls inside `area`, only on pinned rows that can move that way.
    pin_up: Option<Rect>,
    pin_down: Option<Rect>,
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
        self.interval_buttons.clear();
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
        if let Some((item, _)) = self
            .menu_items
            .iter()
            .find(|(_, area)| contains(*area, column, row))
        {
            return Some(MouseTarget::MenuItem(*item));
        }
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
                self.interval_buttons
                    .iter()
                    .find(|(_, area)| contains(*area, column, row))
                    .map(|(step, _)| MouseTarget::IntervalStep(*step))
            })
            .or_else(|| {
                self.process_headers
                    .iter()
                    .find(|region| contains(region.area, column, row))
                    .map(|region| MouseTarget::ProcessSortHeader(region.field))
            })
            .or_else(|| {
                // Pin controls sit inside their row, so they are checked first.
                self.process_rows.iter().find_map(|region| {
                    let hit = |control: Option<Rect>| {
                        control.is_some_and(|area| contains(area, column, row))
                    };
                    if hit(region.pin_up) {
                        Some(MouseTarget::PinMove(region.identity, PinMove::Up))
                    } else if hit(region.pin_down) {
                        Some(MouseTarget::PinMove(region.identity, PinMove::Down))
                    } else {
                        None
                    }
                })
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

    /// Like [`Self::target_at`], but a pin control reports its row: moving
    /// between a row and its controls is not a hover change and redraws nothing.
    pub fn hover_target_at(&self, column: u16, row: u16) -> Option<MouseTarget> {
        match self.target_at(column, row) {
            Some(MouseTarget::PinMove(identity, _)) => Some(MouseTarget::ProcessRow(identity)),
            other => other,
        }
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
            .map(|(identity, area)| ProcessRowRegion {
                identity,
                area,
                pin_up: None,
                pin_down: None,
            })
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
    pub(crate) fn with_interval_buttons(
        mut self,
        buttons: impl IntoIterator<Item = (IntervalStep, Rect)>,
    ) -> Self {
        self.interval_buttons = buttons.into_iter().collect();
        self
    }

    /// Adds ▲/▼ controls to the first process row.
    #[cfg(test)]
    pub(crate) fn with_pin_controls(mut self, up: Rect, down: Rect) -> Self {
        if let Some(row) = self.process_rows.first_mut() {
            row.pin_up = Some(up);
            row.pin_down = Some(down);
        }
        self
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

/// Renders one frame from cached state and returns its mouse hit regions.
pub fn render(frame: &mut Frame, app: &App) -> UiRegions {
    let regions = render_frame(frame, app);
    // Untrusted text (process names, journal messages, …) is rendered as-is
    // above; strip control characters before the frame reaches the terminal.
    sanitize::sanitize_buffer(frame.buffer_mut());
    regions
}

fn render_frame(frame: &mut Frame, app: &App) -> UiRegions {
    let area = frame.area();
    frame.render_widget(Clear, area);
    if area.width == 0 || area.height == 0 {
        return UiRegions::default();
    }
    if !layout::terminal_size_supported(area) {
        render_terminal_size_warning(frame, area);
        return UiRegions::default();
    }

    let outer = Block::default().borders(Borders::ALL).title(TITLE);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let (interval_buttons, corner_x) = render_top_right(frame, app, area);
    if app.active_tab() == Tab::Overview {
        render_overview_header(frame, app, area, corner_x);
    }

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
    regions.interval_buttons = interval_buttons;
    regions.input_mode = app.input_mode();
    match content_render {
        ContentRender::Processes(process_render) => {
            regions.process_rows = process_render.rows;
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

    if app.overlay_open() {
        dim_backdrop(frame.buffer_mut(), area);
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
    if let Some(selected) = app.menu_selection() {
        let items = menu::render_menu(frame, selected, app.hovered(), area);
        regions.suppress_background_interaction();
        regions.menu_items = items;
    }
    if app.about_visible() {
        menu::render_about(frame, area);
        regions.suppress_background_interaction();
    }
    if app.help_visible() {
        render_help(frame, app, area);
        regions.suppress_background_interaction();
        regions.process_signal_cancel = None;
        regions.process_signal_confirm = None;
    }

    regions
}

/// Greys out the screen behind a popup so the popup stands out. Popups clear
/// their own area, so they are drawn at full color on top of it.
fn dim_backdrop(buffer: &mut Buffer, area: Rect) {
    buffer.set_style(
        area,
        Style::new()
            .fg(theme::BACKDROP_FG)
            .bg(theme::BACKDROP_BG)
            .remove_modifier(Modifier::BOLD | Modifier::REVERSED),
    );
}

const TITLE: &str = " tuxctl ";
/// The `[-]` and `[+]` buttons after the interval, each followed by a space.
const INTERVAL_BUTTONS: [(IntervalStep, &str); 2] = [
    (IntervalStep::Shorter, "[-]"),
    (IntervalStep::Longer, "[+]"),
];

/// The system summary after the title on the Overview, in the part of the top
/// border left of `corner_x` (where the right corner starts), with a gap.
fn render_overview_header(frame: &mut Frame, app: &App, area: Rect, corner_x: u16) {
    let x = area.x.saturating_add(1 + TITLE.len() as u16);
    let room = corner_x.saturating_sub(1).saturating_sub(x);
    if let Some(line) = overview::header_line(app, usize::from(room)) {
        let width = (line.width() as u16).min(room);
        frame.render_widget(Paragraph::new(line), Rect::new(x, area.y, width, 1));
    }
}

/// Draws the right end of the top border: the stale marker (collectors behind
/// this screen that stopped updating), the sampling interval and its `[-]`/`[+]`
/// buttons. Space is given up in that order: stale names become a bare marker,
/// then the buttons go. Returns the drawn buttons for hit testing and the
/// column where the corner starts.
fn render_top_right(frame: &mut Frame, app: &App, area: Rect) -> (Vec<(IntervalStep, Rect)>, u16) {
    // Keep both corners and a gap after the title free.
    let room = usize::from(area.width).saturating_sub(TITLE.len() + 3);
    let names: Vec<&str> = app.stale_collectors().map(Collector::label).collect();
    let stale_options = if names.is_empty() {
        vec![None]
    } else {
        vec![
            Some(format!(" stale: {} ", names.join(", "))),
            Some(" stale ".to_owned()),
        ]
    };
    let interval = format!(" ⟳ {} ", app.sampling_interval_label());
    let buttons_width = INTERVAL_BUTTONS
        .iter()
        .map(|(_, label)| label.len() + 1)
        .sum::<usize>();
    let width_of = |stale: &Option<String>, buttons: bool| {
        stale.as_deref().map_or(0, |stale| stale.chars().count())
            + interval.chars().count()
            + if buttons { buttons_width } else { 0 }
    };
    let Some((stale, buttons)) = [true, false]
        .into_iter()
        .flat_map(|buttons| stale_options.iter().map(move |stale| (stale, buttons)))
        .find(|(stale, buttons)| width_of(stale, *buttons) <= room)
    else {
        return (Vec::new(), area.right().saturating_sub(1));
    };

    let width = width_of(stale, buttons) as u16;
    let mut x = area.right().saturating_sub(1 + width);
    let corner_x = x;
    let mut spans = Vec::with_capacity(4);
    if let Some(stale) = stale {
        spans.push(Span::styled(
            stale.clone(),
            Style::default().fg(theme::WARNING),
        ));
    }
    spans.push(Span::styled(
        interval.clone(),
        Style::default().fg(theme::MUTED),
    ));
    let corner = Rect::new(x, area.y, width, 1);
    // Blank the border under the whole corner, including the gaps between buttons.
    frame.render_widget(Clear, corner);
    frame.render_widget(Paragraph::new(Line::from(spans)), corner);
    if !buttons {
        return (Vec::new(), corner_x);
    }
    x = x.saturating_add(width - buttons_width as u16);
    let buttons = INTERVAL_BUTTONS
        .iter()
        .map(|&(step, label)| {
            let button = Rect::new(x, area.y, label.len() as u16, 1);
            let style = if app.hovered() == Some(&MouseTarget::IntervalStep(step)) {
                Style::default().fg(theme::ACCENT).bg(theme::HOVER_BG)
            } else {
                Style::default().fg(theme::ACCENT)
            };
            frame.render_widget(Paragraph::new(label).style(style), button);
            x = x.saturating_add(button.width + 1);
            (step, button)
        })
        .collect();
    (buttons, corner_x)
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
                .fg(theme::ACCENT)
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
                .fg(theme::SELECTED_FG)
                .bg(theme::SELECTED_BG)
                .add_modifier(Modifier::BOLD)
        } else if hovered == Some(&MouseTarget::Tab(tab)) {
            Style::default().bg(theme::HOVER_BG)
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
                    .style(Style::default().fg(theme::MUTED))
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

/// Width of the Help popup; every line must fit inside its borders.
const HELP_WIDTH: u16 = 64;

fn render_help(frame: &mut Frame, app: &App, area: Rect) {
    let lines = help_lines(app.nvidia());
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let popup = layout::centered_rect(area, HELP_WIDTH, height);
    if popup.width == 0 || popup.height == 0 {
        return;
    }

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Help ")),
        popup,
    );
}

fn help_lines(nvidia: NvidiaAccess) -> Vec<Line<'static>> {
    let heading = |text: &'static str| {
        Line::from(text).style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
    };
    vec![
        heading("General:"),
        Line::from("  1-5                 Select tab"),
        Line::from("  Tab / Shift+Tab     Next / previous tab (or ← / →)"),
        Line::from("  ?                   Toggle help"),
        Line::from("  + / -               Longer / shorter sampling interval (⟳)"),
        Line::from("  Esc                 Close popup / clear search, view / menu"),
        Line::from("  q                   Quit (confirm with Enter or q)"),
        Line::from("  Ctrl+C              Quit immediately"),
        Line::from(""),
        heading("Navigation:"),
        Line::from("  ↑/k ↓/j PgUp/PgDn   Move selection / scroll (mouse wheel)"),
        Line::from("  Home / End          Jump to top / bottom"),
        Line::from("  /                   Search / filter (↑/↓ move while typing)"),
        Line::from("  Enter               Open item details"),
        Line::from(""),
        heading("Screen Controls:"),
        Line::from("  Processes           c CPU, m MEM, p PID, n Name sort"),
        Line::from("  Signals             T terminate (SIGTERM), K kill (SIGKILL)"),
        Line::from("  Pins                P pin / unpin, Shift+↑/↓ move (or ▲/▼)"),
        Line::from("  Views               v kernel threads, failed units, priority"),
        Line::from("  Services            r refresh system services"),
        Line::from("  Logs                f follow, Space toggle pause"),
        Line::from(""),
        heading("Temperatures (Overview):"),
        Line::from("  Sources             hwmon, thermal zones (– = no value now)"),
        Line::from(nvidia_help(nvidia)),
        Line::from(""),
        Line::from("Esc closes").style(Style::default().fg(theme::MUTED)),
    ]
}

/// Whether NVIDIA temperatures are on, and how to change that.
fn nvidia_help(nvidia: NvidiaAccess) -> &'static str {
    match nvidia {
        NvidiaAccess::On => "  NVIDIA GPUs         NVML; off: --no-nvidia-temperature",
        NvidiaAccess::Off => "  NVIDIA GPUs         off (--no-nvidia-temperature)",
        NvidiaAccess::Unsupported => "  NVIDIA GPUs         off; NVML needs a glibc build",
    }
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
mod tests;
