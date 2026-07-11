mod app;
mod panels;
mod stages;
mod theme;
mod widgets;
mod worker;

fn main() -> iced::Result {
    env_logger::init();
    iced::application(app::App::new, app::App::update, app::App::view)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .title(app::App::title)
        .window_size((1440.0, 900.0))
        .position(iced::window::Position::Centered)
        .run()
}
