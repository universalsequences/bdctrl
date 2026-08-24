mod bd;
mod dashboard;
mod herdr;
mod model;
mod theme;

use std::{env, path::PathBuf};

use dashboard::{Dashboard, init, window_options};
use gpui::{App, AppContext, Application};

fn main() {
    let project = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().expect("could not determine current directory"));
    let project = project.canonicalize().unwrap_or(project);

    Application::new().run(move |cx: &mut App| {
        init(cx);
        let options = window_options(cx);
        cx.open_window(options, |window, cx| {
            window.set_window_title("beadsctrl");
            let dashboard = cx.new(|cx| Dashboard::new(project.clone(), cx));
            dashboard.read(cx).focus(window);
            dashboard
        })
        .expect("could not open beadsctrl window");
        cx.activate(true);
    });
}
