#![cfg(feature = "desktop")]

use audiostrike::device::fill_output_f32;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn output_callback_reads_stereo_frames_then_writes_silence() {
    let source = [0.1_f32, -0.1, 0.2, -0.2];
    let position = AtomicUsize::new(0);
    let mut output = [9.0_f32; 8];
    fill_output_f32(&mut output, &source, 2, &position);
    assert_eq!(output, [0.1, -0.1, 0.2, -0.2, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(position.load(Ordering::Relaxed), 4);
}
