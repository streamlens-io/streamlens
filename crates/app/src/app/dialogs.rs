//! Modal dialogs drawn over the main view: settings, and opening a URL.

use iced::widget::{
    button, center, column, container, mouse_area, opaque, row, slider, stack, text, text_input,
};
use iced::{Color, Element, Length, Theme};
use streamlens_core::{estimated_frame_bytes, CacheWindow};

use crate::theme;

use super::{App, Message};

/// Id of the URL field, focused when the dialog opens.
pub(super) const URL_INPUT: &str = "open-url";

impl App {
    pub(super) fn with_settings_dialog<'a>(
        &'a self,
        base: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let s = &self.settings;
        let window = s.window();
        let slider_row = |label: &'a str, value: u32, min: u32, msg: fn(u32) -> Message| {
            row![
                text(label).size(12).width(110),
                slider(min..=CacheWindow::MAX, value, msg)
                    .step(16u32)
                    .on_release(Message::ApplyWindow)
                    .width(Length::Fill),
                text(value.to_string())
                    .size(12)
                    .font(iced::Font::MONOSPACE)
                    .width(48),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
        };

        let frames = (window.ahead + window.behind + 1) as usize;
        let estimate = match &self.index {
            Some(index) => format!(
                "≈ {} MB for this stream, ≈ {} MB with stage planes",
                (frames * estimated_frame_bytes(index, false)) >> 20,
                (frames * estimated_frame_bytes(index, true)) >> 20,
            ),
            None => "open a stream to see the memory estimate".into(),
        };

        let content = column![
            text("Settings").size(16),
            theme::section("Decoded frame window"),
            text(format!(
                "Decoded frames kept in memory around the shown frame, in decode \
                 order. Ahead is at least {}: the decoder outputs frames up to that \
                 far past the shown one.",
                CacheWindow::MIN_AHEAD
            ))
            .size(11)
            .color(theme::DIM),
            slider_row(
                "Frames ahead",
                window.ahead,
                CacheWindow::MIN_AHEAD,
                Message::SetFramesAhead
            ),
            slider_row("Frames behind", window.behind, 0, Message::SetFramesBehind),
            text(estimate).size(11).color(theme::DIM),
            row![
                iced::widget::space::horizontal(),
                button(text("Close").size(12)).on_press(Message::ToggleSettings)
            ],
        ]
        .spacing(10);
        modal(base, content.into(), Message::ToggleSettings)
    }

    pub(super) fn with_url_dialog<'a>(
        &'a self,
        base: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let open = (!self.url_input.trim().is_empty()).then_some(Message::OpenUrl);
        let content = column![
            text("Open URL").size(16),
            text("An HTTP(S) video file or an HLS playlist (.m3u8).")
                .size(11)
                .color(theme::DIM),
            text_input("https://…", &self.url_input)
                .id(URL_INPUT)
                .on_input(Message::UrlInput)
                .on_submit_maybe(open.clone())
                .size(13)
                .padding(6),
            row![
                iced::widget::space::horizontal(),
                button(text("Cancel").size(12))
                    .style(button::text)
                    .on_press(Message::ToggleUrlDialog),
                button(text("Open").size(12)).on_press_maybe(open),
            ]
            .spacing(8),
        ]
        .spacing(10);
        modal(base, content.into(), Message::ToggleUrlDialog)
    }
}

/// `content` in a card over a dimmed `base`. Clicks outside the card send
/// `on_blur`; the card swallows its own clicks.
fn modal<'a>(
    base: Element<'a, Message>,
    content: Element<'a, Message>,
    on_blur: Message,
) -> Element<'a, Message> {
    let card = container(content)
        .width(480)
        .padding(16)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.base.color.into()),
                border: iced::Border {
                    color: palette.background.strong.color,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        });
    let backdrop = mouse_area(center(opaque(card)).style(|_: &Theme| {
        container::Style {
            background: Some(
                Color {
                    a: 0.6,
                    ..Color::BLACK
                }
                .into(),
            ),
            ..Default::default()
        }
    }))
    .on_press(on_blur);
    stack![base, opaque(backdrop)].into()
}
