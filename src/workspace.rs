//! The window's root view: menu bar, editor, footer, command palette and the
//! shortcuts/about sheets. Also owns exchange-rate refresh and self-update.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, Image, ImageFormat, IntoElement, MouseButton,
    Pixels, Point, Render, SharedString, Subscription, UTF16Selection, Window, WindowControlArea,
    canvas, div, img, prelude::*, px, relative, rgb, rgba,
};

use crate::commands::{self, *};
use crate::editor::{Editor, FONT_FAMILY, theme};
use crate::{rates, updater};

const MENU_BAR_H: f32 = 36.;
/// Window-button glyphs from Segoe MDL2 Assets (present on Windows 10 and 11).
const ICON_FONT: &str = "Segoe MDL2 Assets";
const MENU_W: f32 = 50.;
/// x of the first menu title (padding + icon + gap).
const MENUS_X: f32 = 34.;
const ICON_PNG: &[u8] = include_bytes!("../assets/tallypad-128.png");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay {
    Palette,
    Shortcuts,
    About,
}

enum UpdateState {
    Idle,
    Checking,
    Available(updater::Release),
    Installing,
}

pub struct Workspace {
    editor: Entity<Editor>,
    commands: Rc<Vec<Command>>,
    palette_focus: FocusHandle,
    menu_open: Option<usize>,
    overlay: Option<Overlay>,
    query: String,
    selected: usize,
    rates_label: Option<String>,
    rates_busy: bool,
    update: UpdateState,
    icon: Arc<Image>,
    _observe: Subscription,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        updater::cleanup_previous();
        let cached = rates::load_cache();
        let editor = cx.new(Editor::new);
        let observe = cx.observe(&editor, |_, _, cx| cx.notify());

        let mut this = Workspace {
            editor,
            commands: Rc::new(commands::all()),
            palette_focus: cx.focus_handle(),
            menu_open: None,
            overlay: None,
            query: String::new(),
            selected: 0,
            rates_label: None,
            rates_busy: false,
            update: UpdateState::Idle,
            icon: Arc::new(Image::from_bytes(ImageFormat::Png, ICON_PNG.to_vec())),
            _observe: observe,
        };
        match cached {
            Some((label, stale)) => {
                this.rates_label = Some(label);
                if stale {
                    this.fetch_rates(false, cx);
                }
            }
            None => this.fetch_rates(false, cx),
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            this.update(cx, |ws, cx| ws.check_updates(false, cx)).ok();
        })
        .detach();
        this
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    /// Opens a panel at startup (`--show palette|shortcuts|about|menu:N`).
    /// Used to take documentation screenshots.
    pub fn show_on_launch(
        &mut self,
        what: &str,
        query: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match what {
            "palette" => {
                self.toggle_palette(&ToggleCommandPalette, window, cx);
                if let Some(q) = query {
                    self.query = q.to_string();
                }
            }
            "shortcuts" => self.toggle_overlay(Overlay::Shortcuts, window, cx),
            "about" => self.toggle_overlay(Overlay::About, window, cx),
            other => {
                if let Some(n) = other.strip_prefix("menu:").and_then(|n| n.parse().ok()) {
                    self.menu_open = Some(n);
                }
            }
        }
        cx.notify();
    }

    fn status(&self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        self.editor.update(cx, |e, cx| e.set_status(text, cx));
    }

    // ------------------------------------------------------------ rates ---

    fn fetch_rates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.rates_busy {
            return;
        }
        self.rates_busy = true;
        let task = cx.background_executor().spawn(async { rates::fetch() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |ws, cx| {
                ws.rates_busy = false;
                let outcome = match result {
                    Ok(body) => match rates::apply(&body) {
                        Some(label) => {
                            rates::save_cache(&body);
                            ws.rates_label = Some(label);
                            ws.editor.update(cx, |e, cx| e.refresh_results(cx));
                            Ok(())
                        }
                        None => Err("the rates service sent an unexpected response".to_string()),
                    },
                    Err(e) => Err(e),
                };
                match outcome {
                    Ok(()) if manual => ws.status("Exchange rates updated", cx),
                    Err(e) if manual => ws.status(format!("Couldn't update rates: {e}"), cx),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---------------------------------------------------------- updates ---

    fn check_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Installing) {
            return;
        }
        self.update = UpdateState::Checking;
        if manual {
            self.status("Checking for updates…", cx);
        }
        let task = cx.background_executor().spawn(async { updater::latest() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |ws, cx| {
                ws.update = UpdateState::Idle;
                match result {
                    Ok(release) if updater::is_newer(&release.version, updater::VERSION) => {
                        if manual {
                            ws.status(format!("Tallypad {} is available", release.version), cx);
                        }
                        ws.update = UpdateState::Available(release);
                    }
                    Ok(_) if manual => {
                        ws.status(format!("You're up to date (v{})", updater::VERSION), cx)
                    }
                    Err(e) if manual => ws.status(format!("Update check failed: {e}"), cx),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = &self.update else {
            return;
        };
        let Some(url) = release.exe_url.clone() else {
            cx.open_url(&release.page_url);
            return;
        };
        let version = release.version.clone();
        self.update = UpdateState::Installing;
        self.status(format!("Downloading Tallypad {version}…"), cx);
        let task = cx
            .background_executor()
            .spawn(async move { updater::install(&url) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |ws, cx| match result {
                Ok(exe) => {
                    ws.editor.update(cx, |e, _| e.save_now());
                    if std::process::Command::new(exe).spawn().is_ok() {
                        cx.quit();
                    } else {
                        ws.status("Updated — restart Tallypad to finish", cx);
                    }
                }
                Err(e) => {
                    ws.update = UpdateState::Idle;
                    ws.status(format!("Update failed: {e}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // ---------------------------------------------------------- actions ---

    fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.editor.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    fn run_command(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = None;
        self.overlay = None;
        self.focus_editor(window, cx);
        window.dispatch_action((self.commands[index].action)(), cx);
        cx.notify();
    }

    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay == Some(Overlay::Palette) {
            self.close_overlay(window, cx);
            return;
        }
        self.menu_open = None;
        self.overlay = Some(Overlay::Palette);
        self.query.clear();
        self.selected = 0;
        window.focus(&self.palette_focus);
        cx.notify();
    }

    fn toggle_overlay(&mut self, which: Overlay, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = None;
        if self.overlay == Some(which) {
            self.close_overlay(window, cx);
        } else {
            self.overlay = Some(which);
            self.focus_editor(window, cx);
            cx.notify();
        }
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.focus_editor(window, cx);
        cx.notify();
    }

    fn show_shortcuts(&mut self, _: &ShowShortcuts, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_overlay(Overlay::Shortcuts, window, cx);
    }

    fn show_about(&mut self, _: &ShowAbout, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_overlay(Overlay::About, window, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu_open.is_some() {
            self.menu_open = None;
            cx.notify();
        } else if self.overlay.is_some() {
            self.close_overlay(window, cx);
        }
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, _| e.save_now());
        cx.quit();
    }

    fn refresh_rates(&mut self, _: &RefreshRates, _: &mut Window, cx: &mut Context<Self>) {
        self.fetch_rates(true, cx);
    }

    fn check_for_updates(&mut self, _: &CheckForUpdates, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateState::Available(_)) {
            self.install_update(cx);
        } else {
            self.check_updates(true, cx);
        }
    }

    fn open_repo(&mut self, _: &OpenRepo, _: &mut Window, cx: &mut Context<Self>) {
        cx.open_url(&format!("https://github.com/{}", updater::REPO));
    }

    // ---------------------------------------------------------- palette ---

    fn matches(&self) -> Vec<usize> {
        let mut scored: Vec<(i32, usize)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let full = format!("{}: {}", c.menu.label(), c.name);
                let s = commands::fuzzy_score(&self.query, c.name)
                    .map(|s| s + 50)
                    .or_else(|| commands::fuzzy_score(&self.query, &full))?;
                Some((s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    fn palette_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.matches().len().max(1);
        self.selected = (self.selected + n - 1) % n;
        cx.notify();
    }

    fn palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.matches().len().max(1);
        self.selected = (self.selected + 1) % n;
        cx.notify();
    }

    fn palette_confirm(&mut self, _: &PaletteConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&index) = self.matches().get(self.selected) {
            self.run_command(index, window, cx);
        }
    }

    fn palette_backspace(&mut self, _: &PaletteBackspace, _: &mut Window, cx: &mut Context<Self>) {
        self.query.pop();
        self.selected = 0;
        cx.notify();
    }

    // ----------------------------------------------------------- render ---

    /// Custom title bar: icon, menus, draggable title, palette hint and
    /// window buttons. Drag/Min/Max/Close areas are hit-tested by Windows
    /// itself, so snapping, double-click-to-maximize and Snap Layouts work.
    fn render_menu_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title: SharedString = self.editor.read(cx).file_name().into();
        let maximized = window.is_maximized();
        let window_button =
            |id: &'static str, glyph: &'static str, area: WindowControlArea, close: bool| {
                div()
                    .id(id)
                    .w(px(46.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(ICON_FONT)
                    .text_size(px(10.))
                    .text_color(theme::label())
                    .window_control_area(area)
                    .hover(move |s| {
                        if close {
                            s.bg(rgb(0xc42b1c)).text_color(rgb(0xffffff))
                        } else {
                            s.bg(rgb(0x2e3138)).text_color(theme::text())
                        }
                    })
                    .child(glyph)
            };
        div()
            .h(px(MENU_BAR_H))
            .flex_none()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .pl(px(10.))
            .bg(theme::results_bg())
            .border_b_1()
            .border_color(theme::divider())
            .text_size(px(13.))
            .child(img(self.icon.clone()).size(px(18.)).mr(px(6.)))
            .children(Menu::ALL.iter().enumerate().map(|(i, menu)| {
                let open = self.menu_open == Some(i);
                div()
                    .id(("menu", i))
                    .w(px(MENU_W))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .text_color(if open { theme::text() } else { theme::label() })
                    .when(open, |d| d.bg(rgb(0x2e3138)))
                    .hover(|s| s.bg(rgb(0x2a2d34)).text_color(theme::text()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| {
                            ws.menu_open = if ws.menu_open == Some(i) {
                                None
                            } else {
                                Some(i)
                            };
                            ws.overlay = None;
                            cx.notify();
                        }),
                    )
                    .on_hover(cx.listener(move |ws, hovered: &bool, _, cx| {
                        if *hovered && ws.menu_open.is_some() && ws.menu_open != Some(i) {
                            ws.menu_open = Some(i);
                            cx.notify();
                        }
                    }))
                    .child(menu.label())
            }))
            .child(
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.5))
                    .text_color(theme::muted())
                    .window_control_area(WindowControlArea::Drag)
                    .child(title),
            )
            .child(
                div()
                    .id("palette-hint")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .h(px(22.))
                    .rounded(px(5.))
                    .cursor(CursorStyle::PointingHand)
                    .text_color(theme::muted())
                    .text_size(px(12.))
                    .hover(|s| s.bg(rgb(0x2a2d34)))
                    .child("Commands")
                    .child(kbd("Ctrl+K"))
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.toggle_palette(&ToggleCommandPalette, window, cx)
                    })),
            )
            .child(div().w(px(8.)))
            .child(window_button(
                "win-min",
                "\u{E921}",
                WindowControlArea::Min,
                false,
            ))
            .child(window_button(
                "win-max",
                if maximized { "\u{E923}" } else { "\u{E922}" },
                WindowControlArea::Max,
                false,
            ))
            .child(window_button(
                "win-close",
                "\u{E8BB}",
                WindowControlArea::Close,
                true,
            ))
    }

    fn render_menu_dropdown(&self, menu_index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = Menu::ALL[menu_index];
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, cmd) in self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, c)| c.menu == menu)
        {
            if cmd.separator && !rows.is_empty() {
                rows.push(
                    div()
                        .h(px(1.))
                        .mx(px(10.))
                        .my(px(4.))
                        .bg(theme::divider())
                        .into_any_element(),
                );
            }
            let label = cmd.keys.first().map(|k| commands::key_label(k));
            rows.push(
                div()
                    .id(("menu-item", index))
                    .mx(px(5.))
                    .px(px(10.))
                    .h(px(28.))
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(24.))
                    .hover(|s| s.bg(rgb(0x2f4a3f)))
                    .child(cmd.name)
                    .children(
                        label.map(|l| div().text_color(theme::muted()).text_size(px(12.)).child(l)),
                    )
                    .on_click(
                        cx.listener(move |ws, _, window, cx| ws.run_command(index, window, cx)),
                    )
                    .into_any_element(),
            );
        }
        div()
            .absolute()
            .top(px(MENU_BAR_H - 2.))
            .left(px(MENUS_X + MENU_W * menu_index as f32))
            .min_w(px(250.))
            .py(px(5.))
            .bg(rgb(0x25272d))
            .border_1()
            .border_color(rgb(0x363941))
            .rounded(px(8.))
            .shadow_lg()
            .text_size(px(13.))
            .occlude()
            .children(rows)
    }

    fn backdrop(&self, top: f32, dim: bool, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .bottom_0()
            .when(dim, |d| d.bg(rgba(0x0000007a)))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _, window, cx| {
                    ws.menu_open = None;
                    if ws.overlay.is_some() {
                        ws.close_overlay(window, cx);
                    }
                    cx.notify();
                }),
            )
    }

    fn render_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let matches = self.matches();
        let selected = self.selected.min(matches.len().saturating_sub(1));
        let entity = cx.entity();
        let focus = self.palette_focus.clone();
        let query: SharedString = if self.query.is_empty() {
            "Search commands…".into()
        } else {
            self.query.clone().into()
        };
        let input = div()
            .relative()
            .h(px(46.))
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(theme::divider())
            .text_size(px(15.))
            .child(div().text_color(theme::accent()).child("›"))
            .child(
                div()
                    .text_color(if self.query.is_empty() {
                        theme::faint()
                    } else {
                        theme::text()
                    })
                    .child(query),
            )
            .child(div().w(px(2.)).h(px(18.)).bg(theme::accent()))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );

        let list = div()
            .py(px(6.))
            .children(matches.iter().take(14).enumerate().map(|(row, &index)| {
                let cmd = &self.commands[index];
                let key = cmd.keys.first().map(|k| commands::key_label(k));
                div()
                    .id(("palette-item", index))
                    .mx(px(6.))
                    .px(px(12.))
                    .h(px(34.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .when(row == selected, |d| d.bg(rgb(0x2a4237)))
                    .hover(|s| s.bg(rgb(0x2a2d34)))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_color(theme::muted())
                                    .child(format!("{}:", cmd.menu.label())),
                            )
                            .child(cmd.name),
                    )
                    .children(key.map(|k| kbd(&k)))
                    .on_click(
                        cx.listener(move |ws, _, window, cx| ws.run_command(index, window, cx)),
                    )
            }))
            .when(matches.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(18.))
                        .py(px(10.))
                        .text_color(theme::muted())
                        .child("No matching commands"),
                )
            });

        div()
            .absolute()
            .top(px(72.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("palette")
                    .key_context("Palette")
                    .track_focus(&self.palette_focus)
                    .on_action(cx.listener(Self::palette_up))
                    .on_action(cx.listener(Self::palette_down))
                    .on_action(cx.listener(Self::palette_confirm))
                    .on_action(cx.listener(Self::palette_backspace))
                    .w(px(560.))
                    .bg(rgb(0x222429))
                    .border_1()
                    .border_color(rgb(0x363941))
                    .rounded(px(10.))
                    .shadow_lg()
                    .text_size(px(13.5))
                    .occlude()
                    .child(input)
                    .child(list),
            )
    }

    fn panel(&self, width: f32) -> gpui::Div {
        div()
            .w(px(width))
            .bg(rgb(0x222429))
            .border_1()
            .border_color(rgb(0x363941))
            .rounded(px(12.))
            .shadow_lg()
            .p(px(22.))
            .occlude()
    }

    fn render_shortcuts(&self) -> impl IntoElement {
        let section = |title: &str, rows: Vec<(String, String)>| {
            div()
                .w(px(236.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_color(theme::accent())
                        .text_size(px(12.))
                        .mb(px(4.))
                        .child(title.to_uppercase()),
                )
                .children(rows.into_iter().map(|(name, keys)| {
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .h(px(25.))
                        .child(div().text_color(theme::text()).child(name))
                        .child(kbd(&keys))
                }))
        };
        let mut sections: Vec<AnyElement> = Menu::ALL
            .iter()
            .map(|menu| {
                let rows = self
                    .commands
                    .iter()
                    .filter(|c| c.menu == *menu && !c.keys.is_empty())
                    .map(|c| (c.name.to_string(), commands::key_label(c.keys[0])))
                    .collect();
                section(menu.label(), rows).into_any_element()
            })
            .collect();
        sections.push(
            section(
                "Editing",
                commands::EDITING_KEYS
                    .iter()
                    .map(|(n, k)| (n.to_string(), commands::key_label(k)))
                    .collect(),
            )
            .into_any_element(),
        );

        overlay_center(
            self.panel(820.)
                .id("shortcuts")
                .max_h(relative(0.92))
                .overflow_y_scroll()
                .text_size(px(12.5))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .mb(px(16.))
                        .child(div().text_size(px(17.)).child("Keyboard Shortcuts"))
                        .child(div().text_color(theme::muted()).child("Esc to close")),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_x(px(30.))
                        .gap_y(px(18.))
                        .children(sections),
                ),
        )
    }

    fn render_about(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let link = |id: &'static str, label: &'static str, url: String| {
            div()
                .id(id)
                .text_color(theme::accent())
                .cursor(CursorStyle::PointingHand)
                .hover(|s| s.text_color(theme::accent_hover()))
                .child(label)
                .on_click(move |_, _, cx: &mut App| cx.open_url(&url))
        };
        overlay_center(
            self.panel(420.)
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .text_size(px(13.))
                .child(img(self.icon.clone()).size(px(84.)))
                .child(div().text_size(px(20.)).child("Tallypad"))
                .child(
                    div()
                        .text_color(theme::muted())
                        .child(format!("Version {}", updater::VERSION)),
                )
                .child(
                    div()
                        .text_color(theme::label())
                        .text_center()
                        .child("A notepad calculator for Windows, built with Rust and GPUI."),
                )
                .child(
                    div()
                        .text_color(theme::muted())
                        .text_center()
                        .child("Inspired by Soulver (soulver.app). Not affiliated with or endorsed by its makers."),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(18.))
                        .mt(px(6.))
                        .child(link(
                            "about-repo",
                            "GitHub",
                            format!("https://github.com/{}", updater::REPO),
                        ))
                        .child(link("about-soulver", "Soulver", "https://soulver.app".into()))
                        .child(
                            div()
                                .id("about-updates")
                                .text_color(theme::accent())
                                .cursor(CursorStyle::PointingHand)
                                .child("Check for updates")
                                .on_click(cx.listener(|ws, _, _, cx| ws.check_updates(true, cx))),
                        ),
                ),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.editor.read(cx);
        let left: SharedString = editor.status().unwrap_or_else(|| editor.summary().into());
        let total = editor.footer_total();
        let rates: SharedString = match (&self.rates_label, self.rates_busy) {
            (_, true) => "rates: updating…".into(),
            (Some(label), false) => format!("rates: {label}").into(),
            (None, false) => "rates: offline estimates".into(),
        };
        let update = match &self.update {
            UpdateState::Available(r) => Some(format!("Update to v{}", r.version)),
            UpdateState::Installing => Some("Updating…".to_string()),
            _ => None,
        };

        div()
            .h(px(32.))
            .flex_none()
            .w_full()
            .px(px(18.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.))
            .border_t_1()
            .border_color(theme::divider())
            .bg(theme::results_bg())
            .text_size(px(12.5))
            .child(div().text_color(theme::muted()).child(left))
            .child(
                div()
                    .id("rates")
                    .text_color(theme::faint())
                    .cursor(CursorStyle::PointingHand)
                    .hover(|s| s.text_color(theme::muted()))
                    .child(rates)
                    .on_click(cx.listener(|ws, _, _, cx| ws.fetch_rates(true, cx))),
            )
            .children(update.map(|label| {
                div()
                    .id("update")
                    .px(px(8.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .rounded(px(10.))
                    .bg(rgb(0x2a4237))
                    .text_color(theme::accent())
                    .cursor(CursorStyle::PointingHand)
                    .child(label)
                    .on_click(cx.listener(|ws, _, _, cx| ws.install_update(cx)))
            }))
            .child(div().flex_1())
            .children(total.map(|(label, value)| {
                let copy = value.clone();
                div()
                    .id("total")
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .cursor(CursorStyle::PointingHand)
                    .child(div().text_color(theme::muted()).child(label))
                    .child(
                        div()
                            .text_color(theme::accent())
                            .text_size(px(14.))
                            .child(value),
                    )
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        ws.status("Copied total", cx);
                    }))
            }))
    }
}

fn overlay_center(panel: impl IntoElement) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(panel)
}

/// Renders "Ctrl+Shift+S" as keycaps.
fn kbd(label: &str) -> impl IntoElement + use<> {
    let alternatives: Vec<&str> = label.split(" / ").collect();
    let first = alternatives[0];
    let parts: Vec<String> = if first.contains('+') && first.len() > 1 {
        first
            .split('+')
            .map(|s| {
                if s.is_empty() {
                    "+".into()
                } else {
                    s.to_string()
                }
            })
            .collect()
    } else {
        vec![first.to_string()]
    };
    div()
        .flex()
        .gap(px(3.))
        .children(parts.into_iter().map(|p| {
            div()
                .px(px(6.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(rgb(0x3a3d45))
                .bg(rgb(0x2a2c32))
                .text_color(theme::label())
                .text_size(px(11.))
                .child(p)
        }))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workspace")
            .key_context("Workspace")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(FONT_FAMILY)
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::show_shortcuts))
            .on_action(cx.listener(Self::show_about))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::refresh_rates))
            .on_action(cx.listener(Self::check_for_updates))
            .on_action(cx.listener(Self::open_repo))
            .child(self.render_menu_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(self.editor.clone()),
            )
            .child(self.render_footer(cx))
            .when_some(self.menu_open, |d, i| {
                d.child(self.backdrop(MENU_BAR_H, false, cx))
                    .child(self.render_menu_dropdown(i, cx))
            })
            .when_some(self.overlay, |d, overlay| {
                let d = d.child(self.backdrop(0., true, cx));
                match overlay {
                    Overlay::Palette => d.child(self.render_palette(cx)),
                    Overlay::Shortcuts => d.child(self.render_shortcuts()),
                    Overlay::About => d.child(self.render_about(cx)),
                }
            })
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

/// Text input for the command palette's search box.
impl EntityInputHandler for Workspace {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.query.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay != Some(Overlay::Palette) {
            return;
        }
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.selected = 0;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
