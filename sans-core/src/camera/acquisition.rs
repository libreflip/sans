//! Fresh-frame selection for the bounded V4L2 transfer buffers.

pub(super) const CAPTURE_BUFFERS: u32 = 2;

pub(super) fn capture_fresh_frame<Frame, Error>(
    mut capture: impl FnMut() -> Result<Frame, Error>,
) -> Result<Frame, Error> {
    // At most these two buffers can contain frames from before this request.
    // Dropping each frame requeues its transfer buffer before the next dequeue.
    for _ in 0..CAPTURE_BUFFERS {
        drop(capture()?);
    }
    capture()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::capture_fresh_frame;

    #[test]
    fn buffered_frames_are_dropped_before_the_requested_frame_is_selected() {
        struct Frame {
            value: &'static str,
            held: Rc<Cell<bool>>,
        }

        impl Drop for Frame {
            fn drop(&mut self) {
                self.held.set(false);
            }
        }

        let held = Rc::new(Cell::new(false));
        let mut frames = VecDeque::from(["stale first", "stale second", "fresh"]);
        let selected = capture_fresh_frame(|| {
            assert!(
                !held.replace(true),
                "previous transfer buffer is still held"
            );
            Ok::<_, ()>(Frame {
                value: frames.pop_front().unwrap(),
                held: Rc::clone(&held),
            })
        })
        .unwrap();
        assert_eq!(selected.value, "fresh");
        assert!(frames.is_empty());
        drop(selected);
        assert!(!held.get());
    }

    #[test]
    fn acquisition_failure_is_returned_without_selecting_an_old_frame() {
        let mut frames = VecDeque::from([Ok("stale"), Err("disconnected"), Ok("fresh")]);
        assert_eq!(
            capture_fresh_frame(|| frames.pop_front().unwrap()),
            Err("disconnected")
        );
        assert_eq!(frames.pop_front(), Some(Ok("fresh")));
    }
}
