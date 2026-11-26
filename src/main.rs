#![cfg_attr(not(test), windows_subsystem = "windows")]

mod commands;
mod editor;
mod engine;
mod rates;
mod updater;
mod workspace;

use std::path::PathBuf;

use gpui::{
    App, AppContext, Application, Bounds, Focusable, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use workspace::Workspace;

/// Where sheets, settings and cached rates live: %APPDATA%\Tallypad
pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Tallypad")
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys(commands::editing_bindings());
        cx.bind_keys(commands::bindings());

        let bounds = Bounds::centered(None, size(px(940.), px(660.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Tallypad".into()),
                        // Hide the system title bar; Workspace draws its own.
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(480.), px(300.))),
                    app_id: Some("Tallypad".into()),
                    ..Default::default()
                },
                |_, cx| cx.new(Workspace::new),
            )
            .expect("failed to open window");

        let editor = window
            .update(cx, |ws, _, _| ws.editor().clone())
            .expect("window");
        cx.on_app_quit(move |cx| {
            editor.update(cx, |editor, _| editor.save_now());
            async {}
        })
        .detach();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let args: Vec<String> = std::env::args().collect();
        let arg = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1).cloned())
        };
        let show = arg("--show");
        let query = arg("--query");
        let line = arg("--line").and_then(|l| l.parse::<usize>().ok());

        window
            .update(cx, |ws, window, cx| {
                window.focus(&ws.focus_handle(cx));
                if let Some(line) = line {
                    ws.editor()
                        .update(cx, |e, cx| e.move_to_line(line.saturating_sub(1), cx));
                }
                if let Some(show) = show {
                    ws.show_on_launch(&show, query.as_deref(), window, cx);
                }
                cx.activate(true);
            })
            .unwrap();
    });
}
