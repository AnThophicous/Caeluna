//! Validated interactive move and resize grabs.
//!
//! XDG toplevel `move`/`resize` requests and Rouch's own chrome gestures both
//! funnel into the same grabs. A grab only starts from a pointer press whose
//! serial the compositor itself issued, which is exactly what the XDG shell
//! requires before honouring a client request.

use smithay::{
    input::{
        SeatHandler,
        pointer::{
            AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
            GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
            GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
            PointerInnerHandle, RelativeMotionEvent,
        },
    },
    utils::{Logical, Point, Serial},
};

use crate::windowing::{Point as RouchPoint, Rect, ResizeEdge, WindowId};

/// Everything the grab needs to mutate a window during a drag.
pub trait DragTarget {
    /// The frame of the window a grab would act on, if it still exists.
    fn drag_frame(&self, id: WindowId) -> Option<Rect>;
    fn drag_move_to(&mut self, id: WindowId, position: RouchPoint);
    fn drag_resize_by(&mut self, id: WindowId, edge: ResizeEdge, delta: RouchPoint);
    fn drag_reconfigured(&mut self);
}

/// A live pointer grab that moves or resizes one Rouch window.
pub enum WindowGrab {
    Move {
        id: WindowId,
        /// Pointer location minus window origin at grab time.
        offset: Point<f64, Logical>,
    },
    Resize {
        id: WindowId,
        edge: ResizeEdge,
        /// Pointer location at grab time; deltas are measured from here.
        start: Point<f64, Logical>,
    },
}

impl<D> PointerGrab<D> for WindowGrab
where
    D: SeatHandler + DragTarget + 'static,
{
    fn motion(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        _focus: Option<(D::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        match *self {
            Self::Move { id, offset } => data.drag_move_to(
                id,
                RouchPoint {
                    x: (event.location.x - offset.x).round() as i32,
                    y: (event.location.y - offset.y).round() as i32,
                },
            ),
            Self::Resize { id, edge, start } => data.drag_resize_by(
                id,
                edge,
                RouchPoint {
                    x: (event.location.x - start.x).round() as i32,
                    y: (event.location.y - start.y).round() as i32,
                },
            ),
        }
        data.drag_reconfigured();
        // Keep the grabbed window focused while the pointer may travel over
        // other surfaces mid-drag.
        handle.motion(data, handle.current_focus(), event);
    }

    fn relative_motion(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        focus: Option<(D::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(&mut self, data: &mut D, handle: &mut PointerInnerHandle<'_, D>, event: &ButtonEvent) {
        handle.button(data, event);
        if event.state == smithay::backend::input::ButtonState::Released {
            data.drag_reconfigured();
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(&mut self, data: &mut D, handle: &mut PointerInnerHandle<'_, D>, details: AxisFrame) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut D, handle: &mut PointerInnerHandle<'_, D>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut D,
        handle: &mut PointerInnerHandle<'_, D>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<D> {
        unreachable!("Rouch grabs are created exclusively by start_move/start_resize")
    }

    fn unset(&mut self, data: &mut D) {
        data.drag_reconfigured();
    }
}

/// Records pointer press serials so client `xdg_toplevel.move`/`resize`
/// requests can be validated against a real user gesture.
#[derive(Debug, Default)]
pub struct PressedSerials {
    last_press: Option<Serial>,
}

impl PressedSerials {
    pub fn record_press(&mut self, serial: Serial) {
        self.last_press = Some(serial);
    }

    /// True when `serial` matches a press serial this compositor issued.
    pub fn validates(&self, serial: Serial) -> bool {
        self.last_press.is_some_and(|press| press == serial)
    }
}

/// Begin an interactive move from a validated press.
pub fn start_move<D>(
    data: &mut D,
    pointer: &smithay::input::pointer::PointerHandle<D>,
    serial: Serial,
    id: WindowId,
    press: Point<f64, Logical>,
) where
    D: SeatHandler + DragTarget + 'static,
{
    let Some(frame) = data.drag_frame(id) else {
        return;
    };
    let offset = Point::from((press.x - frame.origin.x as f64, press.y - frame.origin.y as f64));

    pointer.set_grab(data, WindowGrab::Move { id, offset }, serial, Focus::Clear);
}

/// Begin an interactive resize from a validated press.
pub fn start_resize<D>(
    data: &mut D,
    pointer: &smithay::input::pointer::PointerHandle<D>,
    serial: Serial,
    id: WindowId,
    edge: ResizeEdge,
    press: Point<f64, Logical>,
) where
    D: SeatHandler + DragTarget + 'static,
{
    if data.drag_frame(id).is_none() {
        return;
    }
    pointer.set_grab(
        data,
        WindowGrab::Resize {
            id,
            edge,
            start: press,
        },
        serial,
        Focus::Clear,
    );
}
