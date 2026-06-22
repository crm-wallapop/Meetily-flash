/// Reports whether a recording is currently active. The composition root wires this
/// to the live recording flag; unit tests inject a fake so the abnormal-activation
/// guards (cold-start, double-tap, continue-while-recording) can be exercised without
/// Tauri or audio hardware.
pub trait RecordingStatePort {
    fn is_recording(&self) -> bool;
}
