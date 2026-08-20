# Sans

Sans coordinates the machine and records the page images produced while scanning a book.

## Language

**Machine profile**:
The global, versioned set of hardware identities, camera geometry, safety-bounded commissioned values, timeouts, and storage root that configures one Sans machine independently of any scan session.
_Avoid_: Settings, runtime state, session data

**Camera role**:
The stable Left or Right position assigned to one physical camera in the scanner.
_Avoid_: Camera number, `/dev/video` number

**Captured frame**:
One image acquired by a camera role during a capture pair.
_Avoid_: Raw image, camera file

**Capture pair**:
The Left and Right captured frames belonging to one stationary acquisition step. The term does not imply simultaneous exposure.
_Avoid_: Synchronized shot, stereo pair

**Full-page preview**:
An aspect-preserving view of the configured page crop from the latest complete capture pair.
_Avoid_: Live view, camera stream

**100% preview**:
A native-resolution detail view in which one image pixel maps to one display pixel.
_Avoid_: Zoomed image, live zoom

**Scan session**:
One operator-started run that records the capture cycles for a single book. It begins when Start is accepted from scan readiness and ends with Finish Book or Abort Book.
_Avoid_: Job, scan job

**Capture cycle**:
One physical operation that captures the visible Left and Right pages before one page-turn outcome. A completed capture cycle produces exactly two page records.
_Avoid_: Page slot, flip cycle

**Capture attempt**:
One attempt to pick up and turn the page within a capture cycle. Retries remain part of the same capture cycle.
_Avoid_: Cycle, page slot

**Touchdown**:
The controlled operation that lowers the suction box until it detects page contact and holds the selected press level.
_Avoid_: Move down, descend

**Flutter fan**:
The airflow actuator that helps separate adjacent pages during pickup.
_Avoid_: Fan, page fan

**Turn blower**:
The airflow actuator that holds back a turned page while the suction box descends.
_Avoid_: Blower, positive-pressure pump

**Pressure baseline**:
The pressure reference for one capture attempt, measured immediately before Touchdown after vacuum has been confirmed off for at least one second.
_Avoid_: Ambient pressure, initial pressure

**Pressure drop**:
The Pressure baseline minus a later pressure reading from the same capture attempt.
_Avoid_: Absolute pressure, ambient differential

**Vacuum-working threshold**:
The minimum Pressure drop showing that the vacuum system is operating even when no page is sealed to the suction box.
_Avoid_: Pickup threshold, vacuum pressure

**Pickup threshold**:
The minimum Pressure drop showing that a page is sealed to the suction box, above the Vacuum-working threshold.
_Avoid_: Vacuum-working threshold, absolute pressure threshold

**Lift percentage**:
A vertical displacement from the current Touchdown position expressed as a percentage of the scan session's page width.
_Avoid_: Absolute Z percentage, fixed lift height

**Page record**:
The durable identity of one captured page image. A capture cycle produces separate Left and Right page records.
_Avoid_: Image entry, file slot

**Sequence number**:
The immutable, zero-based capture-order identity of a page record, distinct from any printed or recognized page number. Left page records use even values and Right page records use odd values.
_Avoid_: Page number

**Finish Book**:
The successful end of a scan session after committing the final capture cycle without requiring another page turn.
_Avoid_: Stop job, abort

**Abort Book**:
The unsuccessful end of a scan session that discards its uncommitted capture cycle without claiming the book is complete.
_Avoid_: Stop, Finish Book

**Operator intervention**:
A non-Stop pause after three confirmed pickup failures in which automatic scanning waits for the operator to try again, turn the page manually, or Finish Book.
_Avoid_: Stopped state, Stop job

**Position trust**:
Ligature's report of whether the suction box's current position can be used for absolute motion. Lost position trust requires Home before recovery motion.
_Avoid_: Homed flag, homed boolean

**Stop**:
A safety halt that interrupts machine operation without declaring the book complete.
_Avoid_: Finish Book

**Confirmed Stop**:
A Stop for which current-connection evidence shows that motion is stopped and vacuum, flutter fan, and turn blower were commanded off.
_Avoid_: Safe stop

**Unconfirmed Stop**:
A Stop lacking current-connection evidence for either stopped motion or commanded-off vacuum, flutter fan, and turn blower; it requires manual power intervention.
_Avoid_: Failed stop, safe stop
