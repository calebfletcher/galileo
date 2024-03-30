use crate::control::{
    EventPropagation, MouseButton, MouseButtonsState, MouseEvent, RawUserEvent, TouchId, UserEvent,
    UserEventHandler,
};
use crate::map::Map;
use galileo_types::cartesian::{CartesianPoint2d, Point2d};
use web_time::SystemTime;

use super::TouchEvent;

const DRAG_THRESHOLD: f64 = 3.0;
const ZOOM_THRESHOLD: f64 = 60.0;
const TILT_THRESHOLD: f64 = 60.0;
const ROTATE_THRESHOLD: f64 = 0.10;
const CLICK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);
const DBL_CLICK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

struct TouchInfo {
    id: TouchId,
    start_position: Point2d,
    _start_time: SystemTime,
    prev_position: Point2d,
}

#[derive(PartialEq)]
enum TouchMode {
    /// Single-touch map scrolling
    Pan,
    /// Zoom map view
    Zoom,
    /// Tilt map view along X axis
    Tilt,
    /// Rotate map view along Z axis
    Rotate,
}

/// Stores input state, converts [`RawUserEvent`] into [`UserEvent`] and manages a list of event handlers.
///
/// When an even is called, the `EventProcessor` will go through event handlers one by one until a handler returns
/// [`EventPropagation::Consume`] or [`EventPropagation::Stop`]. At this point the event is considered to be handled.
pub struct EventProcessor {
    handlers: Vec<Box<dyn UserEventHandler>>,
    pointer_position: Point2d,
    pointer_pressed_position: Point2d,
    touches: Vec<TouchInfo>,
    gesture_controller: GestureController,

    buttons_state: MouseButtonsState,

    last_pressed_time: SystemTime,
    last_click_time: SystemTime,

    drag_target: Option<usize>,
}

impl Default for EventProcessor {
    fn default() -> Self {
        Self {
            handlers: vec![],
            pointer_position: Default::default(),
            pointer_pressed_position: Default::default(),
            touches: Vec::new(),
            gesture_controller: Default::default(),
            buttons_state: Default::default(),
            last_pressed_time: SystemTime::UNIX_EPOCH,
            last_click_time: SystemTime::UNIX_EPOCH,
            drag_target: None,
        }
    }
}

impl EventProcessor {
    /// Adds a new handler to the end of the handler list.
    pub fn add_handler(&mut self, handler: impl UserEventHandler + 'static) {
        self.handlers.push(Box::new(handler));
    }

    /// Handles the event.
    pub fn handle(&mut self, event: RawUserEvent, map: &mut Map) {
        if let Some(user_events) = self.process(event) {
            for user_event in user_events {
                let mut drag_start_target = None;

                if let UserEvent::Click(
                    _,
                    MouseEvent {
                        screen_pointer_position,
                        ..
                    },
                ) = user_event
                {
                    let map_position = map.view().screen_to_map(screen_pointer_position);
                    log::info!("click position: {map_position:?}");
                }

                for (index, handler) in self.handlers.iter_mut().enumerate() {
                    if matches!(user_event, UserEvent::Drag(..) | UserEvent::DragEnded(..)) {
                        if let Some(target) = &self.drag_target {
                            if index != *target {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }

                    match handler.handle(&user_event, map) {
                        EventPropagation::Propagate => {}
                        EventPropagation::Stop => break,
                        EventPropagation::Consume => {
                            if let UserEvent::DragStarted(..) = user_event {
                                drag_start_target = Some(index);
                            }

                            break;
                        }
                    }
                }

                if drag_start_target.is_some() {
                    self.drag_target = drag_start_target;
                }

                if matches!(user_event, UserEvent::DragEnded(..)) {
                    self.drag_target = None;
                }
            }
        }
    }

    fn process(&mut self, event: RawUserEvent) -> Option<Vec<UserEvent>> {
        let now = SystemTime::now();
        match event {
            RawUserEvent::ButtonPressed(button) => {
                self.buttons_state.set_pressed(button);
                self.last_pressed_time = now;
                self.pointer_pressed_position = self.pointer_position;

                Some(vec![UserEvent::ButtonPressed(
                    button,
                    self.get_mouse_event(),
                )])
            }
            RawUserEvent::ButtonReleased(button) => {
                self.buttons_state.set_released(button);
                let mut events = vec![UserEvent::ButtonReleased(button, self.get_mouse_event())];

                if (now.duration_since(self.last_pressed_time)).unwrap_or_default() < CLICK_TIMEOUT
                {
                    log::info!("click position: {:?}", self.pointer_position);
                    events.push(UserEvent::Click(button, self.get_mouse_event()));

                    if (now.duration_since(self.last_click_time)).unwrap_or_default()
                        < DBL_CLICK_TIMEOUT
                    {
                        events.push(UserEvent::DoubleClick(button, self.get_mouse_event()));
                    }

                    self.last_click_time = now;

                    if self.drag_target.take().is_some() {
                        events.push(UserEvent::DragEnded(button, self.get_mouse_event()));
                    }
                }

                Some(events)
            }
            RawUserEvent::PointerMoved(position) => {
                let prev_position = self.pointer_position;
                self.pointer_position = position;

                let mut events = vec![UserEvent::PointerMoved(self.get_mouse_event())];
                if let Some(button) = self.buttons_state.single_pressed() {
                    let mut is_dragging = self.drag_target.is_some();
                    if self.drag_target.is_none()
                        && position.taxicab_distance(&self.pointer_pressed_position)
                            > DRAG_THRESHOLD
                    {
                        events.push(UserEvent::DragStarted(
                            button,
                            self.get_mouse_event_pos(self.pointer_pressed_position),
                        ));

                        is_dragging = true;
                    }

                    if is_dragging {
                        events.push(UserEvent::Drag(
                            button,
                            self.pointer_position - prev_position,
                            self.get_mouse_event(),
                        ));
                    }
                }

                Some(events)
            }
            RawUserEvent::Scroll(delta) => {
                Some(vec![UserEvent::Scroll(delta, self.get_mouse_event())])
            }
            RawUserEvent::TouchStart(touch) => {
                for i in 0..self.touches.len() {
                    if self.touches[i].id == touch.touch_id {
                        // This should never happen, but in case it does, we don't wont a touch to be stuck here forever
                        self.touches.remove(i);
                        break;
                    }
                }

                self.touches.push(TouchInfo {
                    id: touch.touch_id,
                    start_position: touch.position,
                    _start_time: now,
                    prev_position: touch.position,
                });

                if self.touches.len() == 2 {
                    self.gesture_controller
                        .start([&self.touches[0], &self.touches[1]]);
                }

                None
            }
            RawUserEvent::TouchMove(touch) => {
                let touch_info = self.touches.iter().find(|t| t.id == touch.touch_id)?;
                let position = touch.position;

                let mut events = vec![];

                if self.touches.len() == 1 {
                    let mut is_dragging = self.drag_target.is_some();
                    if self.drag_target.is_none()
                        && position.taxicab_distance(&touch_info.start_position) > DRAG_THRESHOLD
                    {
                        events.push(UserEvent::DragStarted(
                            MouseButton::Other,
                            self.get_mouse_event_pos(touch_info.start_position),
                        ));

                        is_dragging = true
                    }

                    if is_dragging {
                        events.push(UserEvent::Drag(
                            MouseButton::Other,
                            position - touch_info.prev_position,
                            self.get_mouse_event_pos(position),
                        ));
                    }
                } else if self.touches.len() == 2 {
                    let gesture_events = self
                        .gesture_controller
                        .update_gesture([&self.touches[0], &self.touches[1]], &touch);
                    events.extend_from_slice(&gesture_events);
                }

                for touch_info in &mut self.touches {
                    if touch_info.id == touch.touch_id {
                        touch_info.prev_position = position;
                    }
                }

                Some(events)
            }
            RawUserEvent::TouchEnd(touch) => {
                for i in 0..self.touches.len() {
                    if self.touches[i].id == touch.touch_id {
                        self.touches.remove(i);
                        break;
                    }
                }

                let mut events = vec![];

                if self.drag_target.is_some() && self.touches.is_empty() {
                    self.drag_target = None;
                    events.push(UserEvent::DragEnded(
                        MouseButton::Other,
                        self.get_mouse_event_pos(touch.position),
                    ));
                }

                Some(events)
            }
        }
    }

    fn get_mouse_event(&self) -> MouseEvent {
        self.get_mouse_event_pos(self.pointer_position)
    }

    fn get_mouse_event_pos(&self, screen_pointer_position: Point2d) -> MouseEvent {
        MouseEvent {
            screen_pointer_position,
            buttons: self.buttons_state,
        }
    }
}

/// A controller to manage two-touch gestures.
///
/// Supports zoom, pan, and tilt gestures.
struct GestureController {
    touch_mode: TouchMode,
    midpoint_start: Point2d,
    angle_start: f64,
    distance_start: f64,
}

impl Default for GestureController {
    fn default() -> Self {
        Self {
            touch_mode: TouchMode::Pan,
            midpoint_start: Default::default(),
            angle_start: Default::default(),
            distance_start: Default::default(),
        }
    }
}

impl GestureController {
    /// Start an active gesture.
    ///
    /// Called the moment there is two active touches
    fn start(&mut self, touches: [&TouchInfo; 2]) {
        let point_1 = touches[0].start_position;
        let point_2 = touches[1].start_position;
        let delta = point_1 - point_2;

        self.distance_start = delta.magnitude();
        self.midpoint_start = Point2d::from((point_1.coords + point_2.coords) / 2.);
        self.angle_start = delta.y.atan2(delta.x);

        self.touch_mode = TouchMode::Pan;
    }

    /// Update the controller with the state of the two touches, and the event.
    fn update_gesture(&mut self, touches: [&TouchInfo; 2], event: &TouchEvent) -> Vec<UserEvent> {
        if !touches.iter().any(|t| t.id == event.touch_id) {
            log::warn!("Unexpected touch id");
            return Vec::new();
        }
        let other_touch_position = if event.touch_id == touches[0].id {
            touches[1].prev_position
        } else {
            touches[0].prev_position
        };

        let old_positions = [touches[0].prev_position, touches[1].prev_position];
        let new_positions = if event.touch_id == touches[0].id {
            [event.position, touches[1].prev_position]
        } else {
            [touches[0].prev_position, event.position]
        };

        let delta = new_positions[0] - new_positions[1];
        let distance = delta.magnitude();
        let midpoint = Point2d::from((new_positions[0].coords + new_positions[1].coords) / 2.);
        let angle = delta.y.atan2(delta.x);

        // Check whether we should switch out of drag mode
        if self.touch_mode == TouchMode::Pan {
            if (distance - self.distance_start).abs() > ZOOM_THRESHOLD {
                self.touch_mode = TouchMode::Zoom;
            } else if (midpoint - self.midpoint_start).magnitude() > TILT_THRESHOLD {
                self.touch_mode = TouchMode::Tilt;
            } else if (angle - self.angle_start).abs() > ROTATE_THRESHOLD {
                self.touch_mode = TouchMode::Rotate;
            }
        }

        let mut events = Vec::new();

        match self.touch_mode {
            TouchMode::Pan => {
                let last_midpoint =
                    Point2d::from((old_positions[0].coords + old_positions[1].coords) / 2.);
                let midpoint_delta = midpoint - last_midpoint;
                events.push(UserEvent::Pan(midpoint_delta, midpoint));
            }
            TouchMode::Zoom => {
                let prev_distance = (old_positions[0] - old_positions[1]).magnitude();
                let zoom = prev_distance / distance;
                events.push(UserEvent::Zoom(zoom, other_touch_position));
            }
            TouchMode::Rotate => {
                let old_delta = old_positions[0] - old_positions[1];
                let old_angle = old_delta.y.atan2(old_delta.x);
                let angle_diff = -(angle - old_angle);
                events.push(UserEvent::Rotate(0., angle_diff));
            }
            TouchMode::Tilt => {
                let last_midpoint =
                    Point2d::from((old_positions[0].coords + old_positions[1].coords) / 2.);
                let midpoint_delta = midpoint - last_midpoint;
                events.push(UserEvent::Rotate(midpoint_delta.y, 0.));
            }
        }
        events
    }
}
