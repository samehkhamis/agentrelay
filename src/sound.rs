//! The ping played when an agent posts: a short two-note chime. On Linux it
//! goes straight to PulseAudio with a pure-Rust client (what reaches WSLg, whose
//! ALSA has no pulse plugin), so the binary links no system sound library and
//! runs where there is none. On macOS it plays through rodio (Core Audio).
//! AGENTRELAY_SOUND=off silences it.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static AUDIO: OnceLock<Option<Sender<()>>> = OnceLock::new();
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

const RATE: u32 = 44_100;

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

pub fn enabled_by_default() -> bool {
    !matches!(std::env::var("AGENTRELAY_SOUND").as_deref(), Ok("off" | "0" | "false"))
}

/// The chime, as mono samples at [`RATE`]: two sine notes, each fading out.
fn chime() -> Vec<f32> {
    // (frequency, length ms, start ms)
    const NOTES: [(f32, u32, u32); 2] = [(880.0, 160, 0), (1318.5, 240, 90)];
    let ms = |n: u32| (n * RATE / 1000) as usize;
    let len = NOTES.iter().map(|&(_, l, start)| ms(start + l)).max().unwrap_or(0);
    let mut out = vec![0.0; len];
    for (freq, length, start) in NOTES {
        let n = ms(length);
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let fade = 1.0 - i as f32 / n as f32;
            out[ms(start) + i] += 0.18 * fade * (std::f32::consts::TAU * freq * t).sin();
        }
    }
    out
}

/// Plays one chime per message on a dedicated thread, keeping the output
/// open between them. None if there is no audio output.
fn audio() -> Option<&'static Sender<()>> {
    AUDIO
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<()>();
            let (ready_tx, ready_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let Some(mut out) = output::Output::open() else {
                    let _ = ready_tx.send(false);
                    return;
                };
                let _ = ready_tx.send(true);
                let samples = chime();
                for () in rx {
                    out.play(&samples);
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

#[cfg(target_os = "linux")]
mod output {
    use super::RATE;
    use anyhow::{bail, Context, Result};
    use pulseaudio::protocol::{self, Command};
    use std::ffi::CString;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    /// A connection to the PulseAudio server, reopened if it drops.
    pub struct Output {
        conn: Option<(BufReader<UnixStream>, u16)>,
    }

    impl Output {
        pub fn open() -> Option<Output> {
            connect().ok().map(|c| Output { conn: Some(c) })
        }

        pub fn play(&mut self, samples: &[f32]) {
            if self.conn.is_none() {
                self.conn = connect().ok();
            }
            let Some((sock, version)) = self.conn.as_mut() else { return };
            if play(sock, *version, samples).is_err() {
                self.conn = None;
            }
        }
    }

    /// Connects and authenticates, as any PulseAudio client does.
    fn connect() -> Result<(BufReader<UnixStream>, u16)> {
        let path = pulseaudio::socket_path_from_env().context("no PulseAudio socket")?;
        let mut sock = BufReader::new(UnixStream::connect(path)?);
        let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
        let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
        protocol::write_command_message(sock.get_mut(), 0, &Command::Auth(auth), protocol::MAX_VERSION)?;
        let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)?;
        let version = protocol::MAX_VERSION.min(reply.version);
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::ApplicationName, CString::new("agentrelay")?);
        protocol::write_command_message(sock.get_mut(), 1, &Command::SetClientName(props), version)?;
        protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)?;
        Ok((sock, version))
    }

    /// Plays samples on a new stream: sends them as the server asks, waits
    /// for them to drain, then removes the stream.
    fn play(sock: &mut BufReader<UnixStream>, version: u16, samples: &[f32]) -> Result<()> {
        const CREATE: u32 = 10;
        const DRAIN: u32 = 11;
        const DELETE: u32 = 12;
        let bytes: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).to_le_bytes()).collect();
        let params = protocol::PlaybackStreamParams {
            sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::S16Le, channels: 1, sample_rate: RATE },
            channel_map: protocol::ChannelMap::mono(),
            cvolume: Some(protocol::ChannelVolume::norm(1)),
            sink_name: Some(protocol::DEFAULT_SINK.to_owned()),
            ..Default::default()
        };
        protocol::write_command_message(sock.get_mut(), CREATE, &Command::CreatePlaybackStream(params), version)?;
        let (_, stream) = protocol::read_reply_message::<protocol::CreatePlaybackStreamReply>(sock, version)?;
        let channel = stream.channel;

        let mut sent = 0;
        let mut send = |sock: &mut BufReader<UnixStream>, want: usize| -> Result<bool> {
            let end = (sent + want).min(bytes.len());
            if end > sent {
                protocol::write_memblock(sock.get_mut(), channel, &bytes[sent..end], 0)?;
                sent = end;
            }
            Ok(sent == bytes.len())
        };
        let mut draining = send(sock, stream.requested_bytes as usize)?;
        if draining {
            protocol::write_command_message(sock.get_mut(), DRAIN, &Command::DrainPlaybackStream(channel), version)?;
        }
        loop {
            match protocol::read_command_message(sock, version)? {
                (_, Command::Request(r)) if r.channel == channel && !draining => {
                    if send(sock, r.length as usize)? {
                        protocol::write_command_message(sock.get_mut(), DRAIN, &Command::DrainPlaybackStream(channel), version)?;
                        draining = true;
                    }
                }
                (DRAIN, Command::Reply) => break,
                (_, Command::Error(e)) => bail!("PulseAudio error: {e:?}"),
                _ => {}
            }
        }
        protocol::write_command_message(sock.get_mut(), DELETE, &Command::DeletePlaybackStream(channel), version)?;
        loop {
            match protocol::read_command_message(sock, version)? {
                (DELETE, Command::Reply) => return Ok(()),
                (_, Command::Error(e)) => bail!("PulseAudio error: {e:?}"),
                _ => {}
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod output {
    use super::RATE;
    use rodio::buffer::SamplesBuffer;
    use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
    use std::num::NonZero;

    /// The default output device, kept open.
    pub struct Output {
        sink: MixerDeviceSink,
    }

    impl Output {
        pub fn open() -> Option<Output> {
            DeviceSinkBuilder::open_default_sink().ok().map(|sink| Output { sink })
        }

        pub fn play(&mut self, samples: &[f32]) {
            let player = Player::connect_new(self.sink.mixer());
            player.append(SamplesBuffer::new(NonZero::<u16>::MIN, NonZero::new(RATE).expect("nonzero"), samples.to_vec()));
            player.detach();
        }
    }
}
