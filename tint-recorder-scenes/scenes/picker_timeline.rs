//! Picker timeline prototype: record causal terminal output first, then
//! compile viewer-facing timing from a typed presentation policy.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use tint_recorder::observer::Predicate;
use tint_recorder::proof::DwellMs;
use tint_recorder::recorder::{Key, Recorder, RecorderConfig};
use tint_recorder::recording::RecordingBuilder;
use tint_recorder_scenes::scenes::{
    ALT_SCREEN_ENTER, ALT_SCREEN_EXIT, BASH_SETTLE_WALL, PICKER_COMMIT_TIMEOUT,
    PICKER_STARTUP_TIMEOUT, lookup_picker_idx, ms,
};
use tint_recorder::timeline::{PresentationBeat, TimelinePolicy};

const PICKER_TARGET: &str = "dark-azure";
const CAPTURE_SETTLE: Duration = ms(20);
const ECHO_TIMEOUT: Duration = ms(500);

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/home/cory/code/tint/tint", env = "TINT_PATH")]
    tint_path: PathBuf,
    #[arg(long, default_value = "assets/picker_timeline.cast")]
    cast: PathBuf,
    #[arg(long, default_value = "assets/picker_timeline.trace.json")]
    trace: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let target_idx = lookup_picker_idx(&args.tint_path, PICKER_TARGET)?;

    let mut recorder = Recorder::start(RecorderConfig::default())?;
    let mut recording = RecordingBuilder::new();
    let policy = TimelinePolicy::default();

    push_capture(
        &mut recorder,
        &mut recording,
        &[],
        BASH_SETTLE_WALL,
        None,
        &policy,
    )?;
    type_text(
        &mut recorder,
        &mut recording,
        "# pick interactively",
        &policy,
    )?;
    press_key(&mut recorder, &mut recording, Key::Enter, None, &policy)?;
    type_text(&mut recorder, &mut recording, "tint", &policy)?;

    let alt_in = recorder.arm_watch(ALT_SCREEN_ENTER);
    let started = Instant::now();
    recorder.write_bytes(Key::Enter.bytes())?;
    alt_in.wait(PICKER_STARTUP_TIMEOUT).ok_or_else(|| {
        anyhow::anyhow!(
            "picker startup timed out after {}ms - alt-screen never claimed",
            PICKER_STARTUP_TIMEOUT.as_millis(),
        )
    })?;
    push_marker(&mut recording, "picker_alt_screen_enter", started.elapsed());
    push_capture(
        &mut recorder,
        &mut recording,
        Key::Enter.bytes(),
        CAPTURE_SETTLE,
        Some(PresentationBeat::PickerAppeared),
        &policy,
    )?;

    for _ in 0..(target_idx + 3) {
        press_key(
            &mut recorder,
            &mut recording,
            Key::Down,
            Some(PresentationBeat::PickerNav),
            &policy,
        )?;
    }
    recording.record_beat(DwellMs::from_duration(
        policy.dwell_for(PresentationBeat::PickerOvershoot),
    ))?;

    for _ in 0..3 {
        press_key(
            &mut recorder,
            &mut recording,
            Key::Up,
            Some(PresentationBeat::PickerNav),
            &policy,
        )?;
    }
    recording.record_beat(DwellMs::from_duration(
        policy.dwell_for(PresentationBeat::PickerSelected),
    ))?;

    let alt_out = recorder.arm_watch(ALT_SCREEN_EXIT);
    let started = Instant::now();
    recorder.write_bytes(Key::Enter.bytes())?;
    alt_out.wait(PICKER_COMMIT_TIMEOUT).ok_or_else(|| {
        anyhow::anyhow!(
            "picker commit timed out after {}ms - alt-screen never exited",
            PICKER_COMMIT_TIMEOUT.as_millis(),
        )
    })?;
    push_marker(&mut recording, "picker_alt_screen_exit", started.elapsed());
    push_capture(
        &mut recorder,
        &mut recording,
        Key::Enter.bytes(),
        CAPTURE_SETTLE,
        Some(PresentationBeat::PickerDigest),
        &policy,
    )?;

    let verified = recording.finish_synthetic(recorder.cols(), recorder.rows())?;
    let _legacy_cast = recorder.stop()?;
    verified.write_json(&args.trace)?;
    verified.cast().write_with_summary(&args.cast)?;
    println!(
        "wrote {} ({} trace events)",
        args.trace.display(),
        verified.trace().len()
    );
    Ok(())
}

fn type_text(
    recorder: &mut Recorder,
    recording: &mut RecordingBuilder,
    text: &str,
    policy: &TimelinePolicy,
) -> anyhow::Result<()> {
    for ch in text.chars() {
        let mut buf = [0_u8; 4];
        let bytes = ch.encode_utf8(&mut buf).as_bytes();
        write_echoed(
            recorder,
            recording,
            bytes,
            Some(PresentationBeat::TypeChar),
            policy,
        )
        .with_context(|| format!("type {ch:?}"))?;
    }
    Ok(())
}

fn write_echoed(
    recorder: &mut Recorder,
    recording: &mut RecordingBuilder,
    bytes: &[u8],
    beat: Option<PresentationBeat>,
    policy: &TimelinePolicy,
) -> anyhow::Result<()> {
    let watch = recorder.arm_watch(bytes);
    let started = Instant::now();
    recorder.write_bytes(bytes)?;
    watch.wait(ECHO_TIMEOUT).ok_or_else(|| {
        anyhow::anyhow!(
            "typed-byte echo timed out after {}ms for {:?}",
            ECHO_TIMEOUT.as_millis(),
            String::from_utf8_lossy(bytes),
        )
    })?;
    push_marker(recording, "typed_echo", started.elapsed());
    let output = recorder.capture_after(Duration::ZERO)?;
    recording.record_step_matching(
        bytes.to_vec(),
        output,
        dwell_for(beat, policy),
        Some(Predicate::ContainsText {
            text: String::from_utf8_lossy(bytes).into_owned(),
        }),
    )?;
    Ok(())
}

fn press_key(
    recorder: &mut Recorder,
    recording: &mut RecordingBuilder,
    key: Key,
    beat: Option<PresentationBeat>,
    policy: &TimelinePolicy,
) -> anyhow::Result<()> {
    recorder.write_bytes(key.bytes())?;
    let output = recorder.capture_after(CAPTURE_SETTLE)?;
    recording.record_step(key.bytes().to_vec(), output, dwell_for(beat, policy))?;
    Ok(())
}

fn push_capture(
    recorder: &mut Recorder,
    recording: &mut RecordingBuilder,
    input: &[u8],
    settle: Duration,
    beat: Option<PresentationBeat>,
    policy: &TimelinePolicy,
) -> anyhow::Result<()> {
    let output = recorder.capture_after(settle)?;
    recording.record_step(input.to_vec(), output, dwell_for(beat, policy))?;
    Ok(())
}

fn dwell_for(beat: Option<PresentationBeat>, policy: &TimelinePolicy) -> DwellMs {
    beat.map_or_else(
        || DwellMs::new(0),
        |beat| DwellMs::from_duration(policy.dwell_for(beat)),
    )
}

fn push_marker(recording: &mut RecordingBuilder, label: &str, elapsed: Duration) {
    recording.push_marker(label, elapsed);
    if std::env::var_os("TINT_RECORDER_PROFILE").is_some() {
        eprintln!(
            "[profile] marker {label} fired in {}us",
            elapsed.as_micros()
        );
    }
}
