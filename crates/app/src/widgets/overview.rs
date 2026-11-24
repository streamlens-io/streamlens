//! The whole stream as one strip under the chart: how far it is indexed,
//! the chart's visible window and the current frame. Click or drag to jump.
//! Positions are in chart bars (frames in the chart's order).

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::{Color, Event, Point, Rectangle, Renderer, Size, Theme};

/// The chart's playhead color.
const ACCENT: Color = Color::from_rgb(0.30, 0.82, 0.88);

/// Jump to this fraction (0..1) of the stream.
#[derive(Debug, Clone, Copy)]
pub struct Seek(pub f32);

pub struct Overview {
    /// Bars the strip spans: the whole stream, estimated while indexing.
    pub total: f32,
    /// Bars indexed so far.
    pub indexed: f32,
    /// The chart's visible window: first bar and number of bars.
    pub window: (f32, f32),
    pub current: f32,
}

#[derive(Default)]
pub struct State {
    dragging: bool,
}

impl<Message: From<Seek>> canvas::Program<Message> for Overview {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let seek = |x: f32| {
            canvas::Action::publish(Seek((x / bounds.width).clamp(0.0, 1.0)).into()).and_capture()
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let pos = cursor.position_in(bounds)?;
                state.dragging = true;
                Some(seek(pos.x))
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                Some(seek(position.x - bounds.x))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = theme.extended_palette();
        let (w, h) = (bounds.width, bounds.height);
        let x_of = |bar: f32| (bar / self.total.max(1.0)).clamp(0.0, 1.0) * w;
        let mut frame = Frame::new(renderer, bounds.size());
        let rect = |frame: &mut Frame, x0: f32, x1: f32, color: Color| {
            frame.fill_rectangle(Point::new(x0, 0.0), Size::new(x1 - x0, h), color);
        };

        // The indexed part filled; the rest only outlined.
        rect(
            &mut frame,
            0.0,
            x_of(self.indexed),
            palette.background.strong.color,
        );
        frame.stroke(
            &Path::rectangle(Point::new(0.5, 0.5), Size::new(w - 1.0, h - 1.0)),
            canvas::Stroke::default()
                .with_width(1.0)
                .with_color(palette.background.strong.color),
        );

        // Visible window, at least 3 px so it stays visible on long streams.
        let (first, visible) = self.window;
        let x0 = x_of(first);
        let x1 = x_of(first + visible).max(x0 + 3.0);
        rect(&mut frame, x0, x1, Color { a: 0.35, ..ACCENT });

        let x = x_of(self.current);
        rect(&mut frame, x - 1.0, x + 1.0, ACCENT);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.dragging || cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}
