use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig};
use std::sync::mpsc::{self, Receiver, Sender};

/// A block of audio samples captured from the input device.
/// Each inner `Vec<f32>` is one channel's worth of samples.
pub type AudioBlock = Vec<Vec<f32>>;

/// Information about an available audio device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// The device name as reported by the host.
    pub name: String,
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
    Ok(list)
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
    Ok(list)
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

        let (tx, rx): (Sender<AudioBlock>, Receiver<AudioBlock>) = mpsc::channel();
        let input_stream = build_input_stream(&input_device, &config, sample_format, tx)?;
        let output_stream =
            build_output_stream(&output_device, &config, sample_format, rx, process)?;

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
    tx: Sender<AudioBlock>,
) -> Result<Stream> {
    let channels = config.channels;
    let err_fn = |err: cpal::StreamError| {
        eprintln!("[audio input error] {}", err);
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let block = deinterleave(data, channels);
                let _ = tx.send(block);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let f32_data: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                let block = deinterleave(&f32_data, channels);
                let _ = tx.send(block);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let f32_data: Vec<f32> = data
                    .iter()
                    .map(|&s| (s as f32 - 32768.0) / 32767.0)
                    .collect();
                let block = deinterleave(&f32_data, channels);
                let _ = tx.send(block);
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
    rx: Receiver<AudioBlock>,
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
                let input_block = rx.try_recv().ok();
                process(input_block.as_ref(), data, channels);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let input_block = rx.try_recv().ok();
                let mut f32_buf = vec![0.0f32; data.len()];
                process(input_block.as_ref(), &mut f32_buf, channels);
                for (out, s) in data.iter_mut().zip(f32_buf.iter()) {
                    *out = (*s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
                }
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                let input_block = rx.try_recv().ok();
                let mut f32_buf = vec![0.0f32; data.len()];
                process(input_block.as_ref(), &mut f32_buf, channels);
                for (out, s) in data.iter_mut().zip(f32_buf.iter()) {
                    *out = (*s * 32767.0 + 32768.0).clamp(0.0, 65535.0) as u16;
                }
            },
            err_fn,
            None,
        )?,
        _ => anyhow::bail!("unsupported sample format {:?}", sample_format),
    };

    Ok(stream)
}

/// Converts interleaved samples into a per-channel `AudioBlock`.
fn deinterleave(interleaved: &[f32], channels: u16) -> AudioBlock {
    let ch = channels as usize;
    let frames = interleaved.len() / ch;
    let mut block = vec![Vec::with_capacity(frames); ch];
    for frame in interleaved.chunks_exact(ch) {
        for (c, sample) in frame.iter().enumerate() {
            block[c].push(*sample);
        }
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn audio_io_config_default() {
        let cfg = AudioIoConfig::default();
        assert!(cfg.sample_rate.is_none());
        assert!(cfg.buffer_size.is_none());
    }
}
