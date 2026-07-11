//! Shared colors and small UI building blocks so every panel renders keys,
//! values, sections, badges and toolbar groups the same way.

use iced::widget::{button, container, row, text};
use iced::{Color, Element, Fill, Theme};
use streamlens_types::FrameType;

/// Frame types follow the common analyzer convention (I red, P blue,
/// B green).
pub fn frame_type_color(t: FrameType) -> Color {
    match t {
        FrameType::I => Color::from_rgb8(0xE5, 0x48, 0x4D),
        FrameType::P => Color::from_rgb8(0x4D, 0x82, 0xE5),
        FrameType::B => Color::from_rgb8(0x53, 0xC0, 0x6A),
    }
}

pub fn frame_type_rgba(t: FrameType) -> [f32; 4] {
    let c = frame_type_color(t);
    [c.r, c.g, c.b, 1.0]
}

pub const DIM: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.45);

/// Section header: uppercase, small, dimmed ("STREAM", "FRAME", …).
pub fn section<'a, M: 'a>(label: &str) -> Element<'a, M> {
    text(label.to_uppercase()).size(11).color(DIM).into()
}

/// Key/value row: dimmed key left, monospace value right-aligned.
pub fn kv<'a, M: 'a>(key: &'a str, value: String) -> Element<'a, M> {
    row![
        text(key).size(12).color(DIM),
        iced::widget::space::horizontal(),
        text(value).size(12).font(iced::Font::MONOSPACE),
    ]
    .width(Fill)
    .into()
}

/// Small colored badge (frame type, prediction mode, …).
pub fn badge<'a, M: 'a>(label: String, color: Color) -> Element<'a, M> {
    container(text(label).size(11).color(Color::WHITE))
        .padding([1, 6])
        .style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(Color { a: 0.35, ..color })),
            border: iced::Border {
                color: Color { a: 0.8, ..color },
                width: 1.0,
                radius: 3.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Toolbar group: children visually clustered in a bordered box.
pub fn group<'a, M: 'a>(children: impl IntoIterator<Item = Element<'a, M>>) -> Element<'a, M> {
    let mut r = row![].spacing(2).align_y(iced::Alignment::Center);
    for c in children {
        r = r.push(c);
    }
    container(r)
        .padding(2)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(iced::Background::Color(palette.background.weak.color)),
                border: iced::Border {
                    color: palette.background.strong.color,
                    width: 1.0,
                    radius: 5.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// Toggle chip: filled with the accent color when active, flat otherwise.
/// Used for overlay/chart-line toggles and segmented choices.
pub fn chip<'a, M: Clone + 'a>(label: &'a str, active: bool, msg: M) -> Element<'a, M> {
    let mut b = button(text(label).size(12)).padding([3, 8]);
    if !active {
        b = b.style(|theme: &Theme, status| {
            let palette = theme.extended_palette();
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: hovered
                    .then_some(iced::Background::Color(palette.background.strong.color)),
                text_color: DIM,
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        });
    }
    b.on_press(msg).into()
}
