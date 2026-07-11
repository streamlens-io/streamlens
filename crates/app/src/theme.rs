//! Shared colors: frame types follow the common analyzer convention (I red, P blue,
//! B green) so users coming from it feel at home.

use iced::Color;
use streamlens_types::FrameType;

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
