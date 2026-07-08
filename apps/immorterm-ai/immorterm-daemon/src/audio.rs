//! Audio engine for ImmorTerm — procedurally synthesized sounds for AI feedback.
//!
//! Uses `rodio` for cross-platform audio playback. All sounds are generated
//! at runtime (no external files). The engine runs a dedicated audio thread
//! that owns the non-Send `OutputStream`, making the public `AudioEngine`
//! handle fully `Send + Sync` for use in `OnceLock` statics.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;

use rodio::{OutputStream, Sink};

/// Named sounds available for playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Short pleasant chime (success, task complete)
    Chime,
    /// Alert tone (danger, error)
    Alert,
    /// Soft click (UI interaction, button press)
    Click,
    /// Low rumble (high danger, critical warning)
    Rumble,
    /// Celebratory fanfare (confetti, fireworks)
    Fanfare,
    /// Gentle notification ping
    Ping,
    /// Typing/typewriter tick
    Tick,
}

impl Sound {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "chime" => Some(Sound::Chime),
            "alert" => Some(Sound::Alert),
            "click" => Some(Sound::Click),
            "rumble" => Some(Sound::Rumble),
            "fanfare" => Some(Sound::Fanfare),
            "ping" => Some(Sound::Ping),
            "tick" => Some(Sound::Tick),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Sound::Chime => "chime",
            Sound::Alert => "alert",
            Sound::Click => "click",
            Sound::Rumble => "rumble",
            Sound::Fanfare => "fanfare",
            Sound::Ping => "ping",
            Sound::Tick => "tick",
        }
    }
}

/// Commands sent to the audio thread.
enum AudioCmd {
    /// Play a named sound at the given volume (0.0 - 1.0).
    PlaySound(Sound, f32),
    /// Play a WAV byte buffer at the given volume.
    PlayWav(Vec<u8>, f32),
}

/// Thread-safe audio engine handle. The actual `OutputStream` lives on a
/// dedicated background thread; this handle sends commands via a channel.
pub struct AudioEngine {
    tx: mpsc::Sender<AudioCmd>,
    /// Volume 0-100 (atomic for lock-free reads from any thread).
    volume: AtomicU8,
    /// Mute toggle.
    muted: AtomicBool,
}

// AudioEngine is Send + Sync because it only holds mpsc::Sender (Send) and atomics.

impl AudioEngine {
    /// Create a new audio engine. Returns `None` if audio output is unavailable
    /// (e.g., headless server, no audio device).
    pub fn new() -> Option<Self> {
        let (tx, rx) = mpsc::channel::<AudioCmd>();

        // Spawn a dedicated thread that owns the OutputStream (non-Send).
        // Use a oneshot-style channel to report whether init succeeded.
        let (init_tx, init_rx) = mpsc::channel::<bool>();

        std::thread::Builder::new()
            .name("immorterm-audio".into())
            .spawn(move || {
                let (stream, handle) = match OutputStream::try_default() {
                    Ok(pair) => pair,
                    Err(_) => {
                        let _ = init_tx.send(false);
                        return;
                    }
                };
                let _ = init_tx.send(true);

                // Keep stream alive; process commands until channel closes.
                let _ = &stream; // prevent drop
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        AudioCmd::PlaySound(sound, vol) => {
                            let wav_data = synthesize(sound);
                            play_wav_on_handle(&handle, &wav_data, vol);
                        }
                        AudioCmd::PlayWav(wav_data, vol) => {
                            play_wav_on_handle(&handle, &wav_data, vol);
                        }
                    }
                }
            })
            .ok()?;

        // Wait for audio thread to report success
        match init_rx.recv() {
            Ok(true) => Some(AudioEngine {
                tx,
                volume: AtomicU8::new(70), // default 70%
                muted: AtomicBool::new(false),
            }),
            _ => None,
        }
    }

    /// Play a named sound. Non-blocking — sends command to audio thread.
    pub fn play(&self, sound: Sound) {
        if self.muted.load(Ordering::Relaxed) {
            return;
        }
        let vol = self.volume.load(Ordering::Relaxed) as f32 / 100.0;
        if vol <= 0.0 {
            return;
        }
        let _ = self.tx.send(AudioCmd::PlaySound(sound, vol));
    }

    /// Play a custom WAV/OGG/MP3 file from disk. Non-blocking.
    pub fn play_file(&self, path: &str) -> Result<(), String> {
        if self.muted.load(Ordering::Relaxed) {
            return Ok(());
        }
        let vol = self.volume.load(Ordering::Relaxed) as f32 / 100.0;
        let data = std::fs::read(path).map_err(|e| format!("Failed to read {}: {}", path, e))?;
        let _ = self.tx.send(AudioCmd::PlayWav(data, vol));
        Ok(())
    }

    /// Set volume (0-100).
    pub fn set_volume(&self, vol: u8) {
        self.volume.store(vol.min(100), Ordering::Relaxed);
    }

    /// Get current volume (0-100).
    pub fn volume(&self) -> u8 {
        self.volume.load(Ordering::Relaxed)
    }

    /// Toggle mute state. Returns new muted state.
    pub fn toggle_mute(&self) -> bool {
        let was = self.muted.fetch_xor(true, Ordering::Relaxed);
        !was
    }

    /// Set mute state explicitly.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Check if muted.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
}

/// Play WAV data on an output stream handle (runs on the audio thread).
fn play_wav_on_handle(handle: &rodio::OutputStreamHandle, wav_data: &[u8], vol: f32) {
    if let Ok(source) = rodio::Decoder::new(Cursor::new(wav_data.to_vec()))
        && let Ok(sink) = Sink::try_new(handle) {
            sink.set_volume(vol);
            sink.append(source);
            sink.detach(); // fire-and-forget
        }
}

// ─── Sound synthesis ─────────────────────────────────────────────────

/// Sample rate for all synthesized sounds.
const SAMPLE_RATE: u32 = 44100;

/// Synthesize a named sound as a WAV byte buffer.
fn synthesize(sound: Sound) -> Vec<u8> {
    let samples: Vec<f32> = match sound {
        Sound::Chime => synth_chime(),
        Sound::Alert => synth_alert(),
        Sound::Click => synth_click(),
        Sound::Rumble => synth_rumble(),
        Sound::Fanfare => synth_fanfare(),
        Sound::Ping => synth_ping(),
        Sound::Tick => synth_tick(),
    };
    encode_wav(&samples, SAMPLE_RATE)
}

/// Two-tone ascending chime (C5 -> E5), 200ms.
fn synth_chime() -> Vec<f32> {
    let dur = 0.2;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    let freq1 = 523.25; // C5
    let freq2 = 659.25; // E5
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = fade_envelope(t, dur, 0.01, 0.05);
        let phase = if t < dur * 0.5 { freq1 } else { freq2 };
        *sample = (2.0 * std::f32::consts::PI * phase * t).sin() * env * 0.4;
    }
    out
}

/// Harsh two-tone alert (descending), 300ms.
fn synth_alert() -> Vec<f32> {
    let dur = 0.3;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    let freq1 = 880.0; // A5
    let freq2 = 440.0; // A4
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = fade_envelope(t, dur, 0.005, 0.05);
        let freq = freq1 + (freq2 - freq1) * (t / dur);
        let sine = (2.0 * std::f32::consts::PI * freq * t).sin();
        let clipped = (sine * 1.5).clamp(-1.0, 1.0);
        *sample = clipped * env * 0.35;
    }
    out
}

/// Short click/pop, ~15ms.
fn synth_click() -> Vec<f32> {
    let dur = 0.015;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = 1.0 - (t / dur);
        let noise = ((i as f32 * 12_345.679).sin() * 43_758.547).fract() * 2.0 - 1.0;
        *sample = noise * env * env * 0.3;
    }
    out
}

/// Low rumble, 500ms — sub-bass with noise modulation.
fn synth_rumble() -> Vec<f32> {
    let dur = 0.5;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = fade_envelope(t, dur, 0.05, 0.2);
        let bass = (2.0 * std::f32::consts::PI * 55.0 * t).sin(); // A1
        let sub = (2.0 * std::f32::consts::PI * 36.7 * t).sin(); // D1
        let noise = ((i as f32 * 7919.0).sin() * 43_758.547).fract() * 2.0 - 1.0;
        *sample = (bass * 0.5 + sub * 0.3 + noise * 0.1) * env * 0.5;
    }
    out
}

/// Short celebratory fanfare — ascending arpeggio (C-E-G-C), 400ms.
fn synth_fanfare() -> Vec<f32> {
    let dur = 0.4;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    let notes = [523.25, 659.25, 783.99, 1046.50]; // C5, E5, G5, C6
    let note_dur = dur / notes.len() as f32;
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let note_idx = (t / note_dur).min(notes.len() as f32 - 1.0) as usize;
        let note_t = t - note_idx as f32 * note_dur;
        let env = fade_envelope(note_t, note_dur, 0.005, 0.03);
        let freq = notes[note_idx];
        // Add a subtle harmonic for richness
        let fundamental = (2.0 * std::f32::consts::PI * freq * t).sin();
        let harmonic = (2.0 * std::f32::consts::PI * freq * 2.0 * t).sin() * 0.3;
        *sample = (fundamental + harmonic) * env * 0.3;
    }
    out
}

/// Soft ping — single sine tone with fast decay, 150ms.
fn synth_ping() -> Vec<f32> {
    let dur = 0.15;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    let freq = 1200.0; // High ping
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = (-t * 20.0).exp(); // exponential decay
        *sample = (2.0 * std::f32::consts::PI * freq * t).sin() * env * 0.25;
    }
    out
}

/// Typewriter tick — very short burst, ~8ms.
fn synth_tick() -> Vec<f32> {
    let dur = 0.008;
    let n = (SAMPLE_RATE as f32 * dur) as usize;
    let mut out = vec![0.0f32; n];
    for (i, sample) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = (1.0 - t / dur).powi(3);
        let tone = (2.0 * std::f32::consts::PI * 3000.0 * t).sin();
        *sample = tone * env * 0.2;
    }
    out
}

/// Fade-in / fade-out envelope.
fn fade_envelope(t: f32, dur: f32, fade_in: f32, fade_out: f32) -> f32 {
    let attack = if t < fade_in { t / fade_in } else { 1.0 };
    let remaining = dur - t;
    let release = if remaining < fade_out {
        remaining / fade_out
    } else {
        1.0
    };
    attack * release
}

/// Encode f32 samples as a 16-bit mono WAV in memory.
fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = samples.len() as u32;
    let data_size = num_samples * 2; // 16-bit = 2 bytes per sample
    let file_size = 36 + data_size;

    let mut buf = Vec::with_capacity(file_size as usize + 8);
    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16_val.to_le_bytes());
    }
    buf
}

/// Map an AI expression change to an optional auto-sound.
/// Called by the MCP handler after applying a SetExpression request.
pub fn expression_auto_sound(
    danger: Option<&str>,
    celebrate: Option<&str>,
    mood: Option<&str>,
) -> Option<Sound> {
    // Celebrations take priority
    if let Some("confetti" | "sparkle" | "fireworks") = celebrate {
        return Some(Sound::Fanfare);
    }
    // Danger levels
    if let Some(d) = danger {
        match d {
            "critical" => return Some(Sound::Rumble),
            "high" => return Some(Sound::Alert),
            "medium" => return Some(Sound::Ping),
            _ => {}
        }
    }
    // Mood-based sounds (only for strong signals)
    if let Some("success") = mood {
        return Some(Sound::Chime);
    }
    if let Some("error") = mood {
        return Some(Sound::Alert);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sound_from_str() {
        assert_eq!(Sound::parse("chime"), Some(Sound::Chime));
        assert_eq!(Sound::parse("ALERT"), Some(Sound::Alert));
        assert_eq!(Sound::parse("Click"), Some(Sound::Click));
        assert_eq!(Sound::parse("rumble"), Some(Sound::Rumble));
        assert_eq!(Sound::parse("fanfare"), Some(Sound::Fanfare));
        assert_eq!(Sound::parse("ping"), Some(Sound::Ping));
        assert_eq!(Sound::parse("tick"), Some(Sound::Tick));
        assert_eq!(Sound::parse("unknown"), None);
    }

    #[test]
    fn test_sound_roundtrip() {
        for sound in [Sound::Chime, Sound::Alert, Sound::Click, Sound::Rumble, Sound::Fanfare, Sound::Ping, Sound::Tick] {
            assert_eq!(Sound::parse(sound.as_str()), Some(sound));
        }
    }

    #[test]
    fn test_synthesize_produces_valid_wav() {
        for sound in [Sound::Chime, Sound::Alert, Sound::Click, Sound::Rumble, Sound::Fanfare, Sound::Ping, Sound::Tick] {
            let wav = synthesize(sound);
            // Check RIFF header
            assert_eq!(&wav[0..4], b"RIFF", "WAV for {:?} missing RIFF header", sound);
            assert_eq!(&wav[8..12], b"WAVE", "WAV for {:?} missing WAVE marker", sound);
            // Must be non-trivial size (header + some data)
            assert!(wav.len() > 44, "WAV for {:?} too short: {} bytes", sound, wav.len());
        }
    }

    #[test]
    fn test_encode_wav_structure() {
        let samples = vec![0.0f32; 100];
        let wav = encode_wav(&samples, 44100);
        // File size field = total - 8
        let file_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]);
        assert_eq!(file_size as usize, wav.len() - 8);
        // Data chunk size
        let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_size, 200); // 100 samples * 2 bytes
    }

    #[test]
    fn test_expression_auto_sound() {
        assert_eq!(expression_auto_sound(None, Some("confetti"), None), Some(Sound::Fanfare));
        assert_eq!(expression_auto_sound(Some("critical"), None, None), Some(Sound::Rumble));
        assert_eq!(expression_auto_sound(Some("high"), None, None), Some(Sound::Alert));
        assert_eq!(expression_auto_sound(Some("medium"), None, None), Some(Sound::Ping));
        assert_eq!(expression_auto_sound(None, None, Some("success")), Some(Sound::Chime));
        assert_eq!(expression_auto_sound(None, None, Some("error")), Some(Sound::Alert));
        assert_eq!(expression_auto_sound(None, None, Some("neutral")), None);
        assert_eq!(expression_auto_sound(None, None, None), None);
        // Celebrate takes priority over danger
        assert_eq!(expression_auto_sound(Some("high"), Some("fireworks"), None), Some(Sound::Fanfare));
    }

    #[test]
    fn test_fade_envelope() {
        // At start, should be near 0
        assert!(fade_envelope(0.0, 1.0, 0.1, 0.1) < 0.01);
        // In the middle, should be 1.0
        assert!((fade_envelope(0.5, 1.0, 0.1, 0.1) - 1.0).abs() < 0.01);
        // Near end, should be decreasing
        assert!(fade_envelope(0.95, 1.0, 0.1, 0.1) < 1.0);
    }
}
