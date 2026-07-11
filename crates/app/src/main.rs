mod app;
mod panels;
mod stages;
mod theme;
mod widgets;
mod worker;

fn main() -> iced::Result {
    env_logger::init();
    // Debug/testing hook: STREAMLENS_WINDOW=WxH overrides the initial size.
    let size = std::env::var("STREAMLENS_WINDOW")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1440.0, 900.0));
    iced::application(app::App::new, app::App::update, app::App::view)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .title(app::App::title)
        .window_size(size)
        .position(iced::window::Position::Centered)
        .run()
}
