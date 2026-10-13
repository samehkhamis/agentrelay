//! The ping played when an agent posts: a short two-note chime synthesized
//! with rodio. On Linux it plays through cpal's `pulseaudio` host, a pure-Rust
//! PulseAudio client (what reaches WSLg, whose ALSA has no pulse plugin);
//! elsewhere through the default output (Core Audio on macOS).
//! AGENTCHAT_SOUND=off silences it.

use rodio::source::SineWave;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static AUDIO: OnceLock<Option<Sender<()>>> = OnceLock::new();
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// Points the PulseAudio client at the per-user socket when the environment
/// doesn't (e.g. launched without XDG_RUNTIME_DIR). Call before threads start.
#[cfg(target_os = "linux")]
pub fn init_env() {
    if std::env::var_os("PULSE_SERVER").is_some() || std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let Ok(uid) = std::fs::metadata("/proc/self").map(|m| m.uid()) else { return };
    let dir = format!("/run/user/{uid}");
    if std::path::Path::new(&dir).join("pulse/native").exists() {
        // SAFETY: called from main before any other thread exists.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir) };
    }
}

#[cfg(not(target_os = "linux"))]
pub fn init_env() {}

/// PulseAudio only: the ALSA fallback would print errors over the TUI and,
/// under WSLg, has no device anyway.
#[cfg(target_os = "linux")]
fn open_sink() -> Option<MixerDeviceSink> {
    use rodio::cpal::traits::HostTrait;
    let host = rodio::cpal::host_from_id(rodio::cpal::HostId::PulseAudio).ok()?;
    let device = host.default_output_device()?;
    DeviceSinkBuilder::from_device(device).ok()?.open_stream().ok()
}

#[cfg(not(target_os = "linux"))]
fn open_sink() -> Option<MixerDeviceSink> {
    DeviceSinkBuilder::open_default_sink().ok()
}

pub fn enabled_by_default() -> bool {
    !matches!(std::env::var("AGENTCHAT_SOUND").as_deref(), Ok("off" | "0" | "false"))
}

fn tone(freq: f32, ms: u64, delay_ms: u64) -> impl Source {
    let d = Duration::from_millis(ms);
    SineWave::new(freq)
        .amplify(0.18)
        .fade_out(d)
        .take_duration(d)
        .delay(Duration::from_millis(delay_ms))
}

/// Opens the output once on a dedicated thread and keeps it open, so each
/// ping is just a mix into the live stream. None if there is no audio device.
fn audio() -> Option<&'static Sender<()>> {
    AUDIO
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<()>();
            let (ready_tx, ready_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let Some(sink) = open_sink() else {
                    let _ = ready_tx.send(false);
                    return;
                };
                let _ = ready_tx.send(true);
                for () in rx {
                    let player = Player::connect_new(sink.mixer());
                    player.append(tone(880.0, 160, 0).mix(tone(1318.5, 240, 90)));
                    player.detach();
                }
            });
            ready_rx.recv_timeout(Duration::from_secs(3)).unwrap_or(false).then_some(tx)
        })
        .as_ref()
}

/// Plays the ping, at most once every 1.5s so a burst of replies is one sound.
/// Falls back to the terminal bell when no audio output can be opened.
pub fn ping() {
    {
        let mut last = LAST.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_millis(1500)) {
            return;
        }
        *last = Some(Instant::now());
    }
    if audio().is_some_and(|tx| tx.send(()).is_ok()) {
        return;
    }
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}
