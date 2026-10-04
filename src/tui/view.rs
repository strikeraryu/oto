use super::{
    form::{Field, Mode},
    App, Modal,
};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

fn accent() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}
fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn focused() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}
fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray))
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 64 || area.height < 26 {
        frame.render_widget(Paragraph::new("Oto needs a terminal at least 64 columns × 26 rows.\nEnlarge this window. Ctrl-C opens quit controls; press y to stop and quit.")
            .wrap(Wrap { trim: false }).block(panel(" OTO ")), area);
        // Keep modal navigation visible even after resizing a running session.
        if app.modal.is_some() {
            draw_modal(frame, app);
        }
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(if app.active() { 13 } else { 15 }),
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(area);
    let local = app
        .addresses
        .first()
        .map(String::as_str)
        .unwrap_or("No LAN address — check Wi-Fi");
    let extra = if app.addresses.len() > 1 {
        format!(" +{} more (F4 for all)", app.addresses.len() - 1)
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" OTO  ", accent()),
            Span::raw(format!("Local IP: {local}{extra}")),
        ]))
        .block(panel(" Audio on your network ")),
        rows[0],
    );
    if app.active() {
        dashboard(frame, app, rows[1]);
    } else {
        setup(frame, app, rows[1]);
    }
    let notice_style = if app.notice_error {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Yellow)
    };
    frame.render_widget(
        Paragraph::new(clean(&app.notice))
            .style(notice_style)
            .wrap(Wrap { trim: false }),
        rows[2],
    );
    logs(frame, app, rows[3]);
    let footer = if app.active() {
        "Tab/←→ Actions  Enter Select  -/+ 10ms  [/] 1ms  0 Reset\nd Output  s Stop  q Quit  PgUp/PgDn Logs  End Follow  ? Help"
    } else {
        "Tab/↑↓ Move  ←→ Host/Join  Enter Select  Space Toggle\nF1 Host  F2 Join  Ctrl-U Clear field  Esc Back  ? Help  Ctrl-C Quit"
    };
    frame.render_widget(Paragraph::new(footer).style(muted()), rows[4]);
    draw_modal(frame, app);
}

fn setup(frame: &mut Frame, app: &App, area: Rect) {
    let form = &app.form;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "  Host  ",
                if form.mode == Mode::Host {
                    if form.focus == 0 {
                        focused()
                    } else {
                        accent()
                    }
                } else {
                    muted()
                },
            ),
            Span::raw("   "),
            Span::styled(
                "  Join  ",
                if form.mode == Mode::Join {
                    if form.focus == 0 {
                        focused()
                    } else {
                        accent()
                    }
                } else {
                    muted()
                },
            ),
        ]),
        Line::raw(""),
    ];
    for (index, field) in form.fields().iter().enumerate() {
        let selected = form.focus == index + 1;
        let prefix = if selected { " > " } else { "   " };
        let label = match field {
            Field::CodeMode => "Connection",
            Field::HostPort | Field::JoinPort => "Port",
            Field::Buffer => "Host buffer (ms)",
            Field::Ip => "Host IP",
            Field::Code => "Code",
            Field::Start => "",
        };
        let line = if let Some(input) = form.input(*field) {
            let placeholder = match field {
                Field::Ip if form.join_no_code => "required",
                Field::Ip => "optional — leave blank for discovery",
                Field::Code => "five-character host code",
                _ => "",
            };
            let input_width = area.width.saturating_sub(29).max(4) as usize;
            let start = if selected {
                input.cursor.saturating_sub(input_width.saturating_sub(1))
            } else {
                0
            };
            let end = (start + input_width.saturating_sub(1)).min(input.value.len());
            let value = if input.value.is_empty() && !selected {
                placeholder.into()
            } else if selected {
                format!(
                    "{}│{}",
                    &input.value[start..input.cursor],
                    &input.value[input.cursor..end.max(input.cursor)]
                )
            } else {
                input.value[start..end].to_owned()
            };
            format!("{prefix}{label:<18} [ {value} ]")
        } else if *field == Field::CodeMode {
            let no_code = if form.mode == Mode::Host {
                form.host_no_code
            } else {
                form.join_no_code
            };
            format!(
                "{prefix}{label:<18} {}   (Space or Enter to switch)",
                if no_code { "No code" } else { "With code" }
            )
        } else {
            format!(
                "{prefix}[ {} ]",
                if form.mode == Mode::Host {
                    "Start hosting"
                } else {
                    "Join session"
                }
            )
        };
        lines.push(Line::styled(
            line,
            if selected {
                focused()
            } else {
                Style::default()
            },
        ));
    }
    lines.push(Line::raw(""));
    let hint = match form.mode {
        Mode::Host if form.host_no_code => "Anyone who can reach this host on the LAN can join.",
        Mode::Host => "A code will be generated when hosting starts.",
        Mode::Join if form.join_no_code => "Enter the host's IP and port; no code will be sent.",
        Mode::Join if form.ip.value.is_empty() => {
            "Enter a code to find the host automatically on your LAN."
        }
        Mode::Join => "Connect directly using the host's IP, port, and code.",
    };
    lines.push(Line::styled(hint, muted()));
    lines.push(Line::styled(
        if app.checking {
            "Checking for an existing session…"
        } else {
            "Use Tab to reach Start, then press Enter."
        },
        muted(),
    ));
    frame.render_widget(Paragraph::new(lines).block(panel(" Connect ")), area);
}

fn dashboard(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if let Some(status) = &app.status {
        let role = if status.role == "host" {
            "HOSTING"
        } else {
            "JOINED"
        };
        let code = status.code.as_deref().unwrap_or("No code required");
        lines.push(Line::styled(
            format!(
                "{role}  •  {}{}",
                status.state,
                if app.stopping { " • stopping" } else { "" }
            ),
            accent(),
        ));
        lines.push(Line::raw(format!(
            "Port: {}     Code: {code}",
            status.control_port
        )));
        let addresses = status
            .addresses
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::raw(format!(
            "{}: {}",
            if status.role == "host" {
                "Host address"
            } else {
                "Connected host"
            },
            if addresses.is_empty() {
                "Starting…"
            } else {
                &addresses
            }
        )));
        if status.role == "host" {
            lines.push(Line::raw(format!(
                "Clients ({}): {}",
                status.clients.len(),
                if status.clients.is_empty() {
                    "Waiting for devices…".into()
                } else {
                    clean(&status.clients.join(", "))
                }
            )));
        } else {
            lines.push(Line::raw(format!(
                "Clock correction: {:+.3} ms     RTT: {:.2} ms",
                status.clock_offset_ms, status.rtt_ms
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(format!(
            "Output: {}  ({})",
            clean(&status.output),
            if status.follow_default {
                "follows macOS"
            } else {
                "selected output"
            }
        )));
        lines.push(Line::from(vec![
            Span::raw("This speaker's delay: "),
            Span::styled(format!("{} ms", status.latency_ms), accent()),
            Span::raw(
                if app.pending_commands > 0 || status.applied_delay_ms != status.latency_ms as u32 {
                    "  • coordinating…"
                } else {
                    ""
                },
            ),
        ]));
        lines.push(Line::styled(
            format!(
                "Auto added: {} ms  Target: {} ms  Buffer: {} ms",
                status.compensation_ms, status.target_delay_ms, status.buffer_ms
            ),
            muted(),
        ));
        lines.push(Line::raw(format!(
            "Packets sent: {}  received: {}  missing: {}  late: {}",
            status.sent, status.received, status.missing, status.late
        )));
    } else {
        lines.push(Line::styled(
            format!(
                "{}{}…",
                app.requested_role,
                if app.stopping { " • stopping" } else { "" }
            ),
            accent(),
        ));
        lines.push(Line::raw(
            "Waiting for the audio engine. Check macOS permission prompts.",
        ));
        lines.push(Line::raw("Logs and errors will appear below."));
        for _ in 0..6 {
            lines.push(Line::raw(""));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            " [ Output (d) ] ",
            if app.action_focus == 0 {
                focused()
            } else {
                Style::default()
            },
        ),
        Span::raw("  "),
        Span::styled(
            " [ Stop (s) ] ",
            if app.action_focus == 1 {
                focused()
            } else {
                Style::default()
            },
        ),
        Span::raw("  "),
        Span::styled(
            " [ Quit (q) ] ",
            if app.action_focus == 2 {
                focused()
            } else {
                Style::default()
            },
        ),
    ]));
    frame.render_widget(Paragraph::new(lines).block(panel(" Session ")), area);
}

fn logs(frame: &mut Frame, app: &App, area: Rect) {
    let visible = area.height.saturating_sub(2) as usize;
    let end = app.logs.len().saturating_sub(app.log_scroll);
    let start = end.saturating_sub(visible);
    let lines = app
        .logs
        .iter()
        .skip(start)
        .take(end - start)
        .map(|entry| {
            Line::from(vec![
                Span::styled(format!("{}  ", entry.time), muted()),
                Span::raw(entry.text.clone()),
            ])
        })
        .collect::<Vec<_>>();
    let title = if app.log_scroll == 0 {
        " Logs • following "
    } else {
        " Logs • paused (End to follow) "
    };
    frame.render_widget(Paragraph::new(lines).block(panel(title)), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn draw_modal(frame: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else {
        return;
    };
    match modal {
        Modal::Addresses { selected } => {
            let area = centered(
                frame.area(),
                76,
                (app.addresses.len() as u16 + 4).clamp(7, 18),
            );
            frame.render_widget(Clear, area);
            let items = if app.addresses.is_empty() {
                vec![ListItem::new(
                    "No LAN addresses. Connect to Wi-Fi or Ethernet.",
                )]
            } else {
                app.addresses
                    .iter()
                    .map(|address| ListItem::new(address.clone()))
                    .collect()
            };
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel(" Local IPs • ↑↓ scroll • Enter / Esc close "))
                    .highlight_style(focused()),
                area,
                &mut state,
            );
        }
        Modal::Help => {
            let area = centered(frame.area(), 76, 22);
            frame.render_widget(Clear, area);
            let mut lines = vec![
                Line::styled("NAVIGATION", accent()),
                Line::raw("Tab / Shift-Tab / ↑↓  Move between fields and actions"),
                Line::raw("←→ on Host/Join       Choose a connection mode"),
                Line::raw("Enter / Space         Select / toggle   •   Esc: back"),
                Line::raw("F1: Host  F2: Join     Ctrl-U: clear the current field"),
                Line::raw("F4                    Show all local network addresses"),
                Line::raw("PgUp / PgDn           Scroll logs   •   End: follow live logs"),
                Line::raw(""),
                Line::styled("DURING A SESSION — REPORT YOUR SPEAKER'S DELAY", accent()),
                Line::raw("- / + (or =)          Adjust by -10 / +10 ms"),
                Line::raw("[ / ]                 Adjust by -1 / +1 ms"),
                Line::raw("0                     Reset this speaker's report to zero"),
                Line::raw("d: choose output      s: stop / leave   q: quit controls"),
                Line::raw(""),
                Line::raw("If this speaker sounds late, increase its reported delay."),
                Line::raw("Oto automatically delays faster outputs across the session."),
                Line::raw("Range: 0–500 ms. Saved per speaker and restored on switch."),
            ];
            lines.push(Line::raw(""));
            lines.push(Line::styled("Enter / Esc to close", muted()));
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(panel(" Keyboard help ")),
                area,
            );
        }
        Modal::Quit { stop } => {
            let area = centered(frame.area(), 62, if app.managed.is_none() { 9 } else { 8 });
            frame.render_widget(Clear, area);
            let mut lines = vec![
                Line::raw("Stop the local audio session and quit Oto?"),
                Line::raw("Other Macs will stop receiving if this Mac is hosting."),
                Line::raw(""),
            ];
            lines.push(Line::from(vec![
                Span::styled(
                    " [ Stop and quit ] ",
                    if *stop { focused() } else { Style::default() },
                ),
                Span::raw("  "),
                Span::styled(
                    " [ Cancel ] ",
                    if !*stop { focused() } else { Style::default() },
                ),
            ]));
            lines.push(Line::styled(
                "←→ / Tab selects • Enter confirms • Esc cancels",
                muted(),
            ));
            if app.managed.is_none() {
                lines.push(Line::styled(
                    "d: close this TUI and leave the existing session running",
                    muted(),
                ));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(panel(" Quit ")),
                area,
            );
        }
        Modal::Outputs {
            devices,
            selected,
            loaded,
        } => {
            let height = (devices.len() as u16 + 7).clamp(9, 18);
            let area = centered(frame.area(), 72, height);
            frame.render_widget(Clear, area);
            let block = panel(" Local output • ↑↓ select, Enter apply, Esc cancel ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            if !*loaded {
                frame.render_widget(
                    Paragraph::new("Loading connected audio outputs…").alignment(Alignment::Center),
                    inner,
                );
                return;
            }
            let mut items = vec![ListItem::new(
                "System default — follow macOS output changes",
            )];
            for device in devices {
                items.push(ListItem::new(format!(
                    "{}{}",
                    clean(&device.name),
                    if device.is_default {
                        " (macOS default)"
                    } else {
                        ""
                    }
                )));
            }
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(focused())
                    .highlight_symbol(" > "),
                inner,
                &mut state,
            );
        }
    }
}
