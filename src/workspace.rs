//! The window's root view: menu bar, editor, footer, command palette and the
//! shortcuts/about sheets. Also owns exchange-rate refresh and self-update.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, Image, ImageFormat, IntoElement, MouseButton,
    PathBuilder, Pixels, Point, Render, SharedString, Subscription, UTF16Selection, Window,
    WindowControlArea, canvas, div, fill, img, point, prelude::*, px, relative, rgb, rgba, size,
};

use crate::commands::{self, *};
use crate::editor::{Editor, FONT_FAMILY, Graph, theme};
use crate::{ai, rates, updater};

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
    ApiKey,
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
    graph_open: bool,
    model_menu: bool,
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
            graph_open: false,
            model_menu: false,
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
            "graph" => self.graph_open = true,
            "models" => self.model_menu = true,
            "apikey" => self.set_api_key(&SetApiKey, window, cx),
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
        if self.model_menu {
            self.model_menu = false;
            cx.notify();
        } else if self.menu_open.is_some() {
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

    // --------------------------------------------------------------- AI ---

    fn use_model(&mut self, model: &str, cx: &mut Context<Self>) {
        self.model_menu = false;
        self.editor.update(cx, |e, cx| e.set_ai_model(model, cx));
        cx.notify();
    }

    fn use_opus(&mut self, _: &UseOpus, _: &mut Window, cx: &mut Context<Self>) {
        self.use_model(ai::MODELS[0].0, cx);
    }

    fn use_sonnet(&mut self, _: &UseSonnet, _: &mut Window, cx: &mut Context<Self>) {
        self.use_model(ai::MODELS[1].0, cx);
    }

    fn use_haiku(&mut self, _: &UseHaiku, _: &mut Window, cx: &mut Context<Self>) {
        self.use_model(ai::MODELS[2].0, cx);
    }

    fn set_api_key(&mut self, _: &SetApiKey, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = None;
        self.model_menu = false;
        self.overlay = Some(Overlay::ApiKey);
        self.query.clear();
        window.focus(&self.palette_focus);
        cx.notify();
    }

    fn save_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = std::mem::take(&mut self.query);
        if key.trim().is_empty() {
            return;
        }
        self.close_overlay(window, cx);
        match ai::set_api_key(&key) {
            Ok(()) => {
                self.status("API key saved", cx);
                self.editor.update(cx, |e, cx| e.retry_ai(cx));
            }
            Err(e) => self.status(format!("Couldn't save the key: {e}"), cx),
        }
    }

    fn remove_api_key(&mut self, _: &RemoveApiKey, _: &mut Window, cx: &mut Context<Self>) {
        ai::remove_api_key();
        self.status("API key removed", cx);
    }

    fn toggle_graph(&mut self, _: &ToggleGraph, _: &mut Window, cx: &mut Context<Self>) {
        self.graph_open = !self.graph_open;
        cx.notify();
    }

    fn palette_paste(&mut self, _: &PalettePaste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.query.extend(text.chars().filter(|c| !c.is_control()));
            self.selected = 0;
            cx.notify();
        }
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
        if self.overlay == Some(Overlay::ApiKey) {
            self.save_api_key(window, cx);
            return;
        }
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
            let current = self.editor.read(cx).ai_model().to_string();
            let checked = ai::MODELS
                .iter()
                .any(|m| m.0 == current && cmd.name == format!("Use {}", m.2));
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
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w(px(10.))
                                    .text_color(theme::accent())
                                    .child(if checked { "✓" } else { "" }),
                            )
                            .child(cmd.name),
                    )
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
                    ws.model_menu = false;
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
        let model = ai::short_name(self.editor.read(cx).ai_model());
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
            .child(
                div()
                    .id("graph-toggle")
                    .px(px(8.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .rounded(px(10.))
                    .cursor(CursorStyle::PointingHand)
                    .text_color(if self.graph_open {
                        theme::accent()
                    } else {
                        theme::muted()
                    })
                    .hover(|s| s.bg(rgb(0x2a2d34)))
                    .child("Graph")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.graph_open = !ws.graph_open;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("model-pill")
                    .px(px(9.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(10.))
                    .bg(rgba(0xb8a6ff22))
                    .text_color(theme::ai())
                    .cursor(CursorStyle::PointingHand)
                    .hover(|s| s.bg(rgba(0xb8a6ff38)))
                    .child("✦")
                    .child(model)
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.model_menu = !ws.model_menu;
                        ws.menu_open = None;
                        cx.notify();
                    })),
            )
    }

    /// Model picker that opens above the footer's model pill.
    fn render_model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.editor.read(cx).ai_model().to_string();
        let has_key = ai::has_api_key();
        let row = |id: &'static str| {
            div()
                .id(id)
                .mx(px(5.))
                .px(px(10.))
                .h(px(30.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .gap(px(8.))
                .hover(|s| s.bg(rgb(0x2f4a3f)))
        };
        let blurb = |m: &str| match m {
            "claude-opus-5-5" => "smartest",
            "claude-sonnet-5-5" => "balanced",
            _ => "fastest, cheapest",
        };
        div()
            .absolute()
            .bottom(px(36.))
            .right(px(10.))
            .w(px(260.))
            .py(px(5.))
            .bg(rgb(0x25272d))
            .border_1()
            .border_color(rgb(0x363941))
            .rounded(px(8.))
            .shadow_lg()
            .text_size(px(13.))
            .occlude()
            .child(
                div()
                    .px(px(15.))
                    .py(px(5.))
                    .text_size(px(11.5))
                    .text_color(theme::muted())
                    .child("Answer ? lines with"),
            )
            .children(ai::MODELS.iter().map(|&(id, _, name)| {
                row(id)
                    .child(
                        div()
                            .w(px(10.))
                            .text_color(theme::accent())
                            .child(if id == current { "✓" } else { "" }),
                    )
                    .child(div().flex_1().child(name))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::faint())
                            .child(blurb(id)),
                    )
                    .on_click(cx.listener(move |ws, _, _, cx| ws.use_model(id, cx)))
            }))
            .child(div().h(px(1.)).mx(px(10.)).my(px(4.)).bg(theme::divider()))
            .child(
                row("model-key")
                    .child(div().w(px(10.)))
                    .child(if has_key {
                        "Change API key…"
                    } else {
                        "Set API key…"
                    })
                    .on_click(
                        cx.listener(|ws, _, window, cx| ws.set_api_key(&SetApiKey, window, cx)),
                    ),
            )
    }

    fn render_api_key(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let focus = self.palette_focus.clone();
        let n = self.query.chars().count();
        let shown: SharedString = if n == 0 {
            "sk-ant-…".into()
        } else {
            let tail: String = self.query.chars().skip(n.saturating_sub(4)).collect();
            format!("{}{}", "•".repeat(n.saturating_sub(4).min(28)), tail).into()
        };
        let env_note = std::env::var("ANTHROPIC_API_KEY").is_ok().then(|| {
            div()
                .text_color(theme::muted())
                .text_size(px(12.))
                .child("ANTHROPIC_API_KEY is set and takes priority over a saved key.")
        });
        let input = div()
            .relative()
            .h(px(40.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(2.))
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0x3a3d45))
            .bg(rgb(0x1b1c20))
            .font_family("Consolas")
            .text_size(px(14.))
            .text_color(if n == 0 {
                theme::faint()
            } else {
                theme::text()
            })
            .child(shown)
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
        div()
            .absolute()
            .top(px(90.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                self.panel(470.)
                    .id("api-key")
                    .key_context("Palette")
                    .track_focus(&self.palette_focus)
                    .on_action(cx.listener(Self::palette_confirm))
                    .on_action(cx.listener(Self::palette_backspace))
                    .on_action(cx.listener(Self::palette_paste))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .text_size(px(13.))
                    .child(div().text_size(px(16.)).child("Anthropic API key"))
                    .child(div().text_color(theme::label()).child(
                        "Questions on ? lines are answered by Claude using your own API key \
                         (console.anthropic.com). It's stored in Windows Credential Manager \
                         and only sent to api.anthropic.com.",
                    ))
                    .child(input)
                    .children(env_note)
                    .child(
                        div()
                            .flex()
                            .gap(px(14.))
                            .text_color(theme::muted())
                            .text_size(px(12.))
                            .child("Paste with Ctrl+V")
                            .child("Enter to save")
                            .child("Esc to cancel"),
                    ),
            )
    }

    fn render_graph(&self, cx: &mut Context<Self>) -> impl IntoElement {
        const BAR_AREA: f32 = 118.;
        let graph = self.editor.read(cx).graph();
        let (title, body): (String, AnyElement) = match graph {
            Graph::Plot(plot) => {
                let valid: Vec<f64> = plot.ys.iter().flatten().copied().collect();
                let mut lo = valid.iter().copied().fold(f64::INFINITY, f64::min);
                let mut hi = valid.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                if (hi - lo).abs() < 1e-12 {
                    lo -= 1.;
                    hi += 1.;
                }
                let pad = (hi - lo) * 0.08;
                let (lo, hi) = (lo - pad, hi + pad);
                let unit = if plot.unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", plot.unit)
                };
                let title = format!(
                    "y = {}    x from {} to {}    y from {}{} to {}{}",
                    plot.expr,
                    short(plot.from),
                    short(plot.to),
                    short(lo + pad),
                    unit,
                    short(hi - pad),
                    unit
                );
                let (from, to) = (plot.from, plot.to);
                let ys = plot.ys;
                let body = canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let w = bounds.size.width;
                        let h = bounds.size.height;
                        let to_x = |x: f64| bounds.left() + w * ((x - from) / (to - from)) as f32;
                        let to_y = |y: f64| bounds.bottom() - h * ((y - lo) / (hi - lo)) as f32;
                        let axis = theme::divider();
                        if lo < 0. && hi > 0. {
                            let y = to_y(0.);
                            window.paint_quad(fill(
                                gpui::Bounds::new(point(bounds.left(), y), size(w, px(1.))),
                                axis,
                            ));
                        }
                        if from < 0. && to > 0. {
                            let x = to_x(0.);
                            window.paint_quad(fill(
                                gpui::Bounds::new(point(x, bounds.top()), size(px(1.), h)),
                                axis,
                            ));
                        }
                        let n = ys.len().saturating_sub(1).max(1);
                        let mut path = PathBuilder::stroke(px(2.));
                        let mut pen_down = false;
                        let mut points = 0;
                        for (k, y) in ys.iter().enumerate() {
                            let x = from + (to - from) * k as f64 / n as f64;
                            match y {
                                Some(y) if *y >= lo - (hi - lo) && *y <= hi + (hi - lo) => {
                                    let p = point(to_x(x), to_y(*y));
                                    if pen_down {
                                        path.line_to(p);
                                    } else {
                                        path.move_to(p);
                                        pen_down = true;
                                    }
                                    points += 1;
                                }
                                _ => pen_down = false,
                            }
                        }
                        if points > 1
                            && let Ok(path) = path.build()
                        {
                            window.paint_path(path, theme::accent());
                        }
                    },
                )
                .flex_1()
                .w_full()
                .into_any_element();
                (title, body)
            }
            Graph::Bars(bars) => {
                let max = bars
                    .iter()
                    .map(|b| b.1.abs())
                    .fold(0., f64::max)
                    .max(1e-12);
                let body = div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_end()
                    .gap(px(10.))
                    .children(bars.iter().take(24).map(|(label, v, display)| {
                        let height = (BAR_AREA * (v.abs() / max) as f32).max(2.);
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme::accent())
                                    .child(display.clone()),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(64.))
                                    .h(px(height))
                                    .rounded_t(px(4.))
                                    .bg(if *v < 0. {
                                        rgba(0xff8a80cc)
                                    } else {
                                        rgba(0x5fd4a0cc)
                                    }),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .overflow_hidden()
                                    .flex()
                                    .justify_center()
                                    .text_size(px(11.))
                                    .text_color(theme::muted())
                                    .child(label.clone()),
                            )
                    }))
                    .into_any_element();
                ("Values in this block".to_string(), body)
            }
            Graph::Empty => (
                "Graph".to_string(),
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme::muted())
                    .child("Put the cursor in a block of numbers to chart it, or write a line like  plot x^2 from -3 to 3")
                    .into_any_element(),
            ),
        };
        div()
            .id("graph-panel")
            .h(px(236.))
            .flex_none()
            .w_full()
            .px(px(18.))
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .border_t_1()
            .border_color(theme::divider())
            .bg(theme::results_bg())
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(12.))
                    .text_color(theme::muted())
                    .child(title)
                    .child(
                        div()
                            .id("graph-close")
                            .px(px(6.))
                            .rounded(px(4.))
                            .cursor(CursorStyle::PointingHand)
                            .hover(|s| s.bg(rgb(0x2a2d34)).text_color(theme::text()))
                            .child("Close  Ctrl+G")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                ws.graph_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(body)
    }
}

/// Compact number for graph labels.
fn short(n: f64) -> String {
    let s = format!("{:.3}", n);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
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
            .on_action(cx.listener(Self::toggle_graph))
            .on_action(cx.listener(Self::use_opus))
            .on_action(cx.listener(Self::use_sonnet))
            .on_action(cx.listener(Self::use_haiku))
            .on_action(cx.listener(Self::set_api_key))
            .on_action(cx.listener(Self::remove_api_key))
            .child(self.render_menu_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(self.editor.clone()),
            )
            .when(self.graph_open, |d| d.child(self.render_graph(cx)))
            .child(self.render_footer(cx))
            .when(self.model_menu, |d| {
                d.child(self.backdrop(0., false, cx))
                    .child(self.render_model_menu(cx))
            })
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
                    Overlay::ApiKey => d.child(self.render_api_key(cx)),
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
        if !matches!(self.overlay, Some(Overlay::Palette | Overlay::ApiKey)) {
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
