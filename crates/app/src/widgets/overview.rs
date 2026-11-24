//! The whole stream as one strip under the chart: how far it is indexed,
//! the chart's visible window and the current frame. Click or drag to jump.
//! Positions are in the caller's unit: seconds, or chart bars when the
//! stream has no timestamps.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::{Color, Event, Point, Rectangle, Renderer, Size, Theme};

/// The chart's playhead color.
const ACCENT: Color = Color::from_rgb(0.30, 0.82, 0.88);

/// Jump to a fraction (0..1) of the stream. `press` is the click; drags
/// that follow it send `false`.
#[derive(Debug, Clone, Copy)]
pub struct Seek {
    pub fraction: f32,
    pub press: bool,
}

pub struct Overview {
    /// What the strip spans: the whole stream.
    pub total: f32,
    /// The indexed part: from, to.
    pub indexed: (f32, f32),
    /// The chart's visible window: from, length.
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
        let seek = |x: f32, press| {
            let fraction = (x / bounds.width).clamp(0.0, 1.0);
            canvas::Action::publish(Seek { fraction, press }.into()).and_capture()
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let pos = cursor.position_in(bounds)?;
                state.dragging = true;
                Some(seek(pos.x, true))
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                Some(seek(position.x - bounds.x, false))
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
            x_of(self.indexed.0),
            x_of(self.indexed.1).max(x_of(self.indexed.0) + 1.0),
            palette.background.strong.color,
        );
        frame.stroke(
            &Path::rectangle(Point::new(0.5, 0.5), Size::new(w - 1.0, h - 1.0)),
            canvas::Stroke::default()
                .with_width(1.0)
                .with_color(palette.background.strong.color),
        );

        // Visible window, at least 3 px so it stays visible on long streams.
        let (from, length) = self.window;
        let x0 = x_of(from);
        let x1 = x_of(from + length).max(x0 + 3.0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::canvas::Program;

    #[derive(Debug)]
    struct M(Seek);

    impl From<Seek> for M {
        fn from(s: Seek) -> Self {
            M(s)
        }
    }

    #[test]
    fn click_and_drag_seek() {
        let overview = Overview {
            total: 100.0,
            indexed: (0.0, 50.0),
            window: (0.0, 10.0),
            current: 0.0,
        };
        let bounds = Rectangle::new(Point::new(10.0, 0.0), Size::new(200.0, 10.0));
        let mut state = State::default();
        let mut send = |event: mouse::Event, x: f32| {
            let cursor = mouse::Cursor::Available(Point::new(x, 5.0));
            Program::<M>::update(&overview, &mut state, &Event::Mouse(event), bounds, cursor)
                .and_then(|a| a.into_inner().0)
                .map(|M(s)| (s.fraction, s.press))
        };
        let press = mouse::Event::ButtonPressed(mouse::Button::Left);
        let release = mouse::Event::ButtonReleased(mouse::Button::Left);
        let moved = |x| mouse::Event::CursorMoved {
            position: Point::new(x, 5.0),
        };

        assert_eq!(send(press, 5.0), None, "press outside the strip");
        assert_eq!(send(moved(60.0), 60.0), None, "move without a drag");
        assert_eq!(send(press, 60.0), Some((0.25, true)));
        assert_eq!(send(moved(160.0), 160.0), Some((0.75, false)));
        assert_eq!(send(moved(400.0), 400.0), Some((1.0, false)), "clamped");
        assert_eq!(send(release, 400.0), None);
        assert_eq!(send(moved(60.0), 60.0), None, "drag ended");
    }
}
