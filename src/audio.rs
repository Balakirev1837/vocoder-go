use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig};
use std::sync::{Arc, Mutex};

/// A block of audio samples captured from the input device.
/// Each inner `Vec<f32>` is one channel's worth of samples.
pub type AudioBlock = Vec<Vec<f32>>;

/// Information about an available audio device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// The device name as reported by the host.
    pub name: String,
}

/// ALSA device name substrings that typically represent non-working or
/// duplicate subdevices (e.g. surround40, front, iec958, dmix).  Any device
/// whose name contains one of these substrings is excluded from the list
/// returned to the caller.
const ALSA_SKIP_PATTERNS: &[&str] = &[
    "surround",
    "front",
    "iec958",
    "dmix",
    "dsnoop",
    "null",
    "file:",
    "rate",
    "speexrate",
    "adapter",
    "speex",
];

/// Returns `true` if the device name looks like a usable ALSA device.
/// Devices whose names match a known non-working pattern are rejected.
fn is_relevant_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !ALSA_SKIP_PATTERNS.iter().any(|pat| lower.contains(pat))
}

/// Removes duplicate devices by name, keeping the first occurrence.
fn dedup_devices(devices: Vec<DeviceInfo>) -> Vec<DeviceInfo> {
    let mut seen = std::collections::HashSet::new();
    devices
        .into_iter()
        .filter(|d| seen.insert(d.name.clone()))
        .collect()
}

/// Filter and deduplicate a raw device list so that only relevant, unique
/// devices are returned.
fn filter_devices(devices: Vec<DeviceInfo>) -> Vec<DeviceInfo> {
    let filtered: Vec<DeviceInfo> = devices
        .into_iter()
        .filter(|d| is_relevant_device_name(&d.name))
        .collect();
    dedup_devices(filtered)
}

/// Returns a list of available audio input devices.
pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .context("could not enumerate input devices")?;
    let mut list = Vec::new();
    for device in devices {
        if let Ok(name) = device.name() {
            list.push(DeviceInfo { name });
        }
    }
    Ok(filter_devices(list))
}

/// Returns a list of available audio output devices.
pub fn list_output_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let devices = host
        .output_devices()
        .context("could not enumerate output devices")?;
    let mut list = Vec::new();
    for device in devices {
        if let Ok(name) = device.name() {
            list.push(DeviceInfo { name });
        }
    }
    Ok(filter_devices(list))
}

/// Find an input device by name. Returns `Ok(device)` if found.
fn find_input_device(name: &str) -> Result<Device> {
    let host = cpal::default_host();
    let mut devices = host
        .input_devices()
        .context("could not enumerate input devices")?;
    devices
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .with_context(|| format!("input device '{}' not found", name))
}

/// Find an output device by name. Returns `Ok(device)` if found.
fn find_output_device(name: &str) -> Result<Device> {
    let host = cpal::default_host();
    let mut devices = host
        .output_devices()
        .context("could not enumerate output devices")?;
    devices
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .with_context(|| format!("output device '{}' not found", name))
}

/// Configuration for audio I/O tuned for low latency.
pub struct AudioIoConfig {
    /// Preferred sample rate in Hz. `None` means use the device default.
    pub sample_rate: Option<u32>,
    /// Preferred buffer size in frames. `None` uses `BufferSize::Default`.
    pub buffer_size: Option<u32>,
}

impl Default for AudioIoConfig {
    fn default() -> Self {
        Self {
            sample_rate: None,
            buffer_size: None,
        }
    }
}

/// Shared buffer state between input and output audio callbacks.
/// Provides latest-value semantics with block recycling to avoid
/// heap allocations in the real-time audio path.
struct SharedBuffer {
    /// Latest audio block written by input, waiting to be consumed by output.
    latest: Option<AudioBlock>,
    /// Previously consumed block returned by output for reuse by input.
    recycle: Option<AudioBlock>,
}

/// Holds the running audio input and output streams.
pub struct AudioIo {
    _input_stream: Stream,
    _output_stream: Stream,
}

impl AudioIo {
    /// Opens audio input and output devices and builds streams configured
    /// for minimal latency.
    ///
    /// If `input_device_name` is `Some(name)`, the input device with that
    /// name is used; otherwise the system default is used. The same applies
    /// to `output_device_name`.
    ///
    /// The closure `process` is called on every output buffer. It receives
    /// the most-recently captured input block (or `None` if none is
    /// available yet) and must fill the output buffer.
    pub fn new(
        io_config: &AudioIoConfig,
        input_device_name: Option<&str>,
        output_device_name: Option<&str>,
        process: impl FnMut(Option<&AudioBlock>, &mut [f32], u16) + Send + 'static,
    ) -> Result<Self> {
        let host = cpal::default_host();

        let input_device = match input_device_name {
            Some(name) => find_input_device(name)?,
            None => host
                .default_input_device()
                .context("no input device available")?,
        };
        let output_device = match output_device_name {
            Some(name) => find_output_device(name)?,
            None => host
                .default_output_device()
                .context("no output device available")?,
        };

        let (config, sample_format) = build_low_latency_config(&output_device, io_config)?;

        let shared = Arc::new(Mutex::new(SharedBuffer {
            latest: None,
            recycle: None,
        }));
        let input_stream =
            build_input_stream(&input_device, &config, sample_format, Arc::clone(&shared))?;
        let output_stream = build_output_stream(
            &output_device,
            &config,
            sample_format,
            Arc::clone(&shared),
            process,
        )?;

        input_stream
            .play()
            .context("failed to start input stream")?;
        output_stream
            .play()
            .context("failed to start output stream")?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
        })
    }
}

/// Choose the best supported stream config for low latency:
/// - Prefer f32 sample format (avoids conversion overhead).
/// - Use the requested sample rate or the device default.
/// - Use the requested buffer size or `Default`.
fn build_low_latency_config(
    device: &Device,
    io_config: &AudioIoConfig,
) -> Result<(StreamConfig, SampleFormat)> {
    let mut supported = device
        .supported_output_configs()
        .context("could not query supported output configs")?;

    // Prefer f32 to avoid format conversion at runtime.
    let config_range = supported
        .find(|c| c.sample_format() == SampleFormat::F32)
        .or_else(|| {
            // Fall back to any supported format if f32 isn't available.
            device.supported_output_configs().ok()?.next()
        })
        .context("no supported output config found")?;

    let sample_format = config_range.sample_format();

    let sample_rate = match io_config.sample_rate {
        Some(rate) => {
            let rate = SampleRate(rate);
            if rate < config_range.min_sample_rate() {
                config_range.min_sample_rate()
            } else if rate > config_range.max_sample_rate() {
                config_range.max_sample_rate()
            } else {
                rate
            }
        }
        None => config_range.max_sample_rate(),
    };

    let supported_config = config_range.with_sample_rate(sample_rate);
    let mut stream_config = supported_config.config();

    if let Some(size) = io_config.buffer_size {
        stream_config.buffer_size = BufferSize::Fixed(size);
    }

    Ok((stream_config, sample_format))
}

fn build_input_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    shared: Arc<Mutex<SharedBuffer>>,
) -> Result<Stream> {
    let channels = config.channels;
    let err_fn = |err: cpal::StreamError| {
        eprintln!("[audio input error] {}", err);
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Acquire a reusable block from the recycle slot
                let mut block = {
                    let mut guard = shared.lock().unwrap();
                    guard
                        .recycle
                        .take()
                        .unwrap_or_else(|| vec![Vec::new(); channels as usize])
                };
                // Write interleaved f32 data into the block (reuses allocations)
                deinterleave_into(data, channels, &mut block, |s| s);
                // Publish to latest slot; move any old latest to recycle
                let mut guard = shared.lock().unwrap();
                guard.recycle = guard.latest.take();
                guard.latest = Some(block);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let mut block = {
                    let mut guard = shared.lock().unwrap();
                    guard
                        .recycle
                        .take()
                        .unwrap_or_else(|| vec![Vec::new(); channels as usize])
                };
                // Convert i16 → f32 and deinterleave directly into the block
                deinterleave_into(data, channels, &mut block, i16_to_f32);
                let mut guard = shared.lock().unwrap();
                guard.recycle = guard.latest.take();
                guard.latest = Some(block);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let mut block = {
                    let mut guard = shared.lock().unwrap();
                    guard
                        .recycle
                        .take()
                        .unwrap_or_else(|| vec![Vec::new(); channels as usize])
                };
                // Convert u16 → f32 and deinterleave directly into the block
                deinterleave_into(data, channels, &mut block, u16_to_f32);
                let mut guard = shared.lock().unwrap();
                guard.recycle = guard.latest.take();
                guard.latest = Some(block);
            },
            err_fn,
            None,
        )?,
        _ => anyhow::bail!("unsupported sample format {:?}", sample_format),
    };

    Ok(stream)
}

fn build_output_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    shared: Arc<Mutex<SharedBuffer>>,
    mut process: impl FnMut(Option<&AudioBlock>, &mut [f32], u16) + Send + 'static,
) -> Result<Stream> {
    let channels = config.channels;
    let err_fn = |err: cpal::StreamError| {
        eprintln!("[audio output error] {}", err);
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Take the latest input block (latest-value semantics)
                let input_block = shared.lock().unwrap().latest.take();
                process(input_block.as_ref(), data, channels);
                // Recycle the block for reuse by the input callback
                if let Some(block) = input_block {
                    shared.lock().unwrap().recycle = Some(block);
                }
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => {
            // Pre-allocate conversion buffer to avoid per-callback allocation
            let mut f32_buf: Vec<f32> = Vec::new();
            device.build_output_stream(
                config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let input_block = shared.lock().unwrap().latest.take();
                    f32_buf.resize(data.len(), 0.0);
                    process(input_block.as_ref(), &mut f32_buf, channels);
                    for (out, s) in data.iter_mut().zip(f32_buf.iter()) {
                        *out = f32_to_i16(*s);
                    }
                    if let Some(block) = input_block {
                        shared.lock().unwrap().recycle = Some(block);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            // Pre-allocate conversion buffer to avoid per-callback allocation
            let mut f32_buf: Vec<f32> = Vec::new();
            device.build_output_stream(
                config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    let input_block = shared.lock().unwrap().latest.take();
                    f32_buf.resize(data.len(), 0.0);
                    process(input_block.as_ref(), &mut f32_buf, channels);
                    for (out, s) in data.iter_mut().zip(f32_buf.iter()) {
                        *out = f32_to_u16(*s);
                    }
                    if let Some(block) = input_block {
                        shared.lock().unwrap().recycle = Some(block);
                    }
                },
                err_fn,
                None,
            )?
        }
        _ => anyhow::bail!("unsupported sample format {:?}", sample_format),
    };

    Ok(stream)
}

/// Convert an i16 sample to f32 in the [-1.0, ~1.0) range.
fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

/// Convert a u16 sample to f32 in the [-1.0, ~1.0) range.
fn u16_to_f32(s: u16) -> f32 {
    (s as f32 - 32768.0) / 32768.0
}

/// Convert an f32 sample in [-1.0, 1.0] to i16.
fn f32_to_i16(s: f32) -> i16 {
    (s * 32768.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// Convert an f32 sample in [-1.0, 1.0] to u16.
fn f32_to_u16(s: f32) -> u16 {
    (s * 32768.0 + 32768.0).clamp(0.0, 65535.0) as u16
}

/// Writes interleaved samples into a pre-allocated `AudioBlock`, applying
/// a per-sample conversion function. Reuses existing vector capacities to
/// avoid heap allocations in the real-time audio path.
fn deinterleave_into<T: Copy>(
    interleaved: &[T],
    channels: u16,
    block: &mut AudioBlock,
    convert: impl Fn(T) -> f32,
) {
    let ch = channels as usize;
    let frames = interleaved.len() / ch;
    block.resize_with(ch, Vec::new);
    for ch_vec in block.iter_mut() {
        ch_vec.resize(frames, 0.0);
    }
    for (i, frame) in interleaved.chunks_exact(ch).enumerate() {
        for (c, sample) in frame.iter().enumerate() {
            block[c][i] = convert(*sample);
        }
    }
}

/// Converts interleaved f32 samples into a per-channel `AudioBlock`.
#[cfg(test)]
fn deinterleave(interleaved: &[f32], channels: u16) -> AudioBlock {
    let mut block = Vec::new();
    deinterleave_into(interleaved, channels, &mut block, |s| s);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AUD-DI: deinterleave tests ---

    #[test]
    fn deinterleave_stereo() {
        let interleaved = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let block = deinterleave(&interleaved, 2);
        assert_eq!(block.len(), 2);
        assert_eq!(block[0], vec![1.0, 3.0, 5.0]);
        assert_eq!(block[1], vec![2.0, 4.0, 6.0]);
    }

    #[test]
    fn deinterleave_mono() {
        let interleaved = vec![10.0, 20.0, 30.0];
        let block = deinterleave(&interleaved, 1);
        assert_eq!(block.len(), 1);
        assert_eq!(block[0], vec![10.0, 20.0, 30.0]);
    }

    /// AUD-DI-01: Empty input with 2 channels returns 2 empty Vecs.
    #[test]
    fn deinterleave_empty_input() {
        let block = deinterleave(&[], 2);
        assert_eq!(block.len(), 2);
        assert!(block[0].is_empty());
        assert!(block[1].is_empty());
    }

    /// AUD-DI-02: Single frame stereo.
    #[test]
    fn deinterleave_single_frame_stereo() {
        let block = deinterleave(&[1.0, 2.0], 2);
        assert_eq!(block, vec![vec![1.0], vec![2.0]]);
    }

    /// AUD-DI-03: Three channels deinterleaving.
    #[test]
    fn deinterleave_three_channels() {
        let block = deinterleave(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3);
        assert_eq!(block, vec![vec![1.0, 4.0], vec![2.0, 5.0], vec![3.0, 6.0]]);
    }

    /// AUD-DI-04: Channel count == 0 causes division by zero panic.
    #[test]
    #[should_panic]
    fn deinterleave_zero_channels_panics() {
        let _ = deinterleave(&[1.0, 2.0], 0);
    }

    /// AUD-DI-05: Round-trip — deinterleave then re-interleave produces original.
    #[test]
    fn deinterleave_round_trip() {
        let original: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let block = deinterleave(&original, 2);
        let mut reinterleaved = Vec::with_capacity(original.len());
        let frames = block.first().map(|c| c.len()).unwrap_or(0);
        for i in 0..frames {
            for ch in &block {
                reinterleaved.push(ch[i]);
            }
        }
        assert_eq!(reinterleaved, original);
    }

    // --- AUD-SF: Sample format conversion tests ---

    /// AUD-SF-01: I16 → f32: i16::MIN maps to -1.0.
    #[test]
    fn i16_to_f32_min() {
        let result = i16_to_f32(i16::MIN);
        assert!((result - (-1.0)).abs() < 1e-10);
    }

    /// AUD-SF-02: I16 → f32: i16::MAX maps to approximately 0.99997.
    #[test]
    fn i16_to_f32_max() {
        let result = i16_to_f32(i16::MAX);
        let expected = 32767.0_f32 / 32768.0;
        assert!((result - expected).abs() < 1e-10);
        assert!(result < 1.0);
    }

    /// AUD-SF-03: I16 → f32: 0 maps to 0.0.
    #[test]
    fn i16_to_f32_zero() {
        assert_eq!(i16_to_f32(0), 0.0);
    }

    /// AUD-SF-04: U16 → f32: 0 maps to -1.0.
    #[test]
    fn u16_to_f32_zero() {
        let result = u16_to_f32(0);
        assert!((result - (-1.0)).abs() < 1e-10);
    }

    /// AUD-SF-05: U16 → f32: 32768 maps to 0.0.
    #[test]
    fn u16_to_f32_midpoint() {
        let result = u16_to_f32(32768);
        assert!((result - 0.0).abs() < 1e-10);
    }

    /// AUD-SF-06: U16 → f32: 65535 maps to approximately 0.99997.
    #[test]
    fn u16_to_f32_max() {
        let result = u16_to_f32(65535);
        let expected = (65535.0_f32 - 32768.0) / 32768.0;
        assert!((result - expected).abs() < 1e-10);
        assert!(result < 1.0);
    }

    /// AUD-SF-07: f32 → U16: -1.0 maps to 0.
    #[test]
    fn f32_to_u16_negative_one() {
        assert_eq!(f32_to_u16(-1.0), 0);
    }

    /// AUD-SF-08: f32 → U16: 0.0 maps to 32768.
    #[test]
    fn f32_to_u16_zero() {
        assert_eq!(f32_to_u16(0.0), 32768);
    }

    /// AUD-SF-09: f32 → U16: 1.0 maps to 65535.
    #[test]
    fn f32_to_u16_one() {
        assert_eq!(f32_to_u16(1.0), 65535);
    }

    // --- AUD-CF: AudioIoConfig tests ---

    /// AUD-CF-01: Default config has sample_rate and buffer_size as None.
    #[test]
    fn audio_io_config_default() {
        let cfg = AudioIoConfig::default();
        assert!(cfg.sample_rate.is_none());
        assert!(cfg.buffer_size.is_none());
    }

    // --- AUD-FL: Audio device filtering tests ---

    /// AUD-FL-01: is_relevant_device_name rejects surround devices.
    #[test]
    fn filter_rejects_surround() {
        assert!(!is_relevant_device_name("surround40"));
        assert!(!is_relevant_device_name("surround51"));
        assert!(!is_relevant_device_name("Surround71"));
    }

    /// AUD-FL-02: is_relevant_device_name rejects front, iec958, dmix, dsnoop.
    #[test]
    fn filter_rejects_known_bad() {
        assert!(!is_relevant_device_name("front"));
        assert!(!is_relevant_device_name("iec958:CARD=Intel"));
        assert!(!is_relevant_device_name("dmix:CARD=Intel"));
        assert!(!is_relevant_device_name("dsnoop:CARD=Intel"));
    }

    /// AUD-FL-03: is_relevant_device_name keeps sysdefault, default, hw, usb, pulse, pipewire.
    #[test]
    fn filter_keeps_relevant() {
        assert!(is_relevant_device_name("sysdefault:CARD=Intel"));
        assert!(is_relevant_device_name("default"));
        assert!(is_relevant_device_name("hw:CARD=Intel,DEV=0"));
        assert!(is_relevant_device_name("usb:CARD=USB"));
        assert!(is_relevant_device_name("pulse"));
        assert!(is_relevant_device_name("pipewire"));
    }

    /// AUD-FL-04: dedup_devices removes duplicates, keeps first occurrence.
    #[test]
    fn dedup_removes_duplicates() {
        let devices = vec![
            DeviceInfo {
                name: "default".into(),
            },
            DeviceInfo {
                name: "hw:CARD=Intel".into(),
            },
            DeviceInfo {
                name: "default".into(),
            },
        ];
        let result = dedup_devices(devices);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "default");
        assert_eq!(result[1].name, "hw:CARD=Intel");
    }

    /// AUD-FL-05: filter_devices combines filtering and dedup.
    #[test]
    fn filter_devices_combined() {
        let devices = vec![
            DeviceInfo {
                name: "default".into(),
            },
            DeviceInfo {
                name: "surround40".into(),
            },
            DeviceInfo {
                name: "front".into(),
            },
            DeviceInfo {
                name: "sysdefault:CARD=Intel".into(),
            },
            DeviceInfo {
                name: "default".into(),
            },
        ];
        let result = filter_devices(devices);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "default");
        assert_eq!(result[1].name, "sysdefault:CARD=Intel");
    }

    /// AUD-FL-06: is_relevant_device_name rejects null, file:, rate, speexrate.
    #[test]
    fn filter_rejects_misc_bad() {
        assert!(!is_relevant_device_name("null"));
        assert!(!is_relevant_device_name("file:/tmp/out.wav"));
        assert!(!is_relevant_device_name("rate_converter"));
        assert!(!is_relevant_device_name("speexrate"));
    }

    /// AUD-FL-07: dedup_devices on empty list returns empty.
    #[test]
    fn dedup_empty() {
        let devices: Vec<DeviceInfo> = vec![];
        let result = dedup_devices(devices);
        assert!(result.is_empty());
    }

    /// AUD-FL-08: filter_devices on all-filtered returns empty.
    #[test]
    fn filter_all_removed() {
        let devices = vec![
            DeviceInfo {
                name: "surround40".into(),
            },
            DeviceInfo {
                name: "front".into(),
            },
            DeviceInfo {
                name: "dmix:CARD=Intel".into(),
            },
        ];
        let result = filter_devices(devices);
        assert!(result.is_empty());
    }
}
