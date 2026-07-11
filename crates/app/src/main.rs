mod app;
mod panels;
mod stages;
mod theme;
mod widgets;
mod worker;

/// Window/dock icon (the .ico embedded in the Windows exe comes from
/// build.rs; macOS release bundles would use an .icns).
const ICON: &[u8] = include_bytes!("../../../assets/icon-256.png");

fn main() -> iced::Result {
    env_logger::init();
    #[cfg(target_os = "macos")]
    set_dock_icon();
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
        .window(iced::window::Settings {
            size: size.into(),
            position: iced::window::Position::Centered,
            icon: iced::window::icon::from_file_data(ICON, None).ok(),
            ..Default::default()
        })
        .run()
}

/// macOS takes the dock icon from the .app bundle, and a bare binary gets
/// the generic executable icon — set it on the shared NSApplication
/// instead (winit reuses the same instance).
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: the image is a valid, non-None NSImage.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}
