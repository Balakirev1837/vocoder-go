// Package vocoder provides audio I/O using github.com/gen2brain/malgo (miniaudio).
//
// It offers functions to list available input and output audio devices (with
// ALSA subdevice filtering), and to start low-latency duplex audio streaming
// with a user-supplied processing callback.
package vocoder

import (
	"fmt"
	"strings"
	"unsafe"

	"github.com/gen2brain/malgo"
)

// DeviceInfo holds information about an available audio device.
type DeviceInfo struct {
	// Name is the human-readable device name as reported by the host.
	Name string
	// ID is the malgo device identifier used to select this device.
	ID malgo.DeviceID
}

// alsaSkipPatterns lists ALSA device name substrings that typically represent
// non-working or duplicate subdevices (e.g. surround40, front, iec958, dmix).
// Any device whose name contains one of these substrings is excluded from the
// list returned to the caller.
var alsaSkipPatterns = []string{
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
}

// isRelevantDeviceName returns true if the device name looks like a usable
// ALSA device.  Devices whose names match a known non-working pattern are
// rejected.
func isRelevantDeviceName(name string) bool {
	lower := strings.ToLower(name)
	for _, pat := range alsaSkipPatterns {
		if strings.Contains(lower, pat) {
			return false
		}
	}
	return true
}

// dedupDevices removes duplicate devices by name, keeping the first
// occurrence.
func dedupDevices(devices []DeviceInfo) []DeviceInfo {
	seen := make(map[string]bool)
	result := make([]DeviceInfo, 0, len(devices))
	for _, d := range devices {
		if !seen[d.Name] {
			seen[d.Name] = true
			result = append(result, d)
		}
	}
	return result
}

// filterDevices filters out irrelevant devices and deduplicates the remainder.
func filterDevices(devices []DeviceInfo) []DeviceInfo {
	filtered := make([]DeviceInfo, 0, len(devices))
	for _, d := range devices {
		if isRelevantDeviceName(d.Name) {
			filtered = append(filtered, d)
		}
	}
	return dedupDevices(filtered)
}

// ListInputDevices returns a list of available audio input (capture) devices.
// Devices matching known ALSA subdevice patterns are filtered out.
func ListInputDevices() ([]DeviceInfo, error) {
	ctx, err := malgo.InitContext(nil, malgo.ContextConfig{}, nil)
	if err != nil {
		return nil, fmt.Errorf("could not init malgo context: %w", err)
	}
	defer func() {
		_ = ctx.Uninit()
		ctx.Free()
	}()

	devices, err := ctx.Devices(malgo.Capture)
	if err != nil {
		return nil, fmt.Errorf("could not enumerate capture devices: %w", err)
	}

	list := make([]DeviceInfo, 0, len(devices))
	for _, d := range devices {
		list = append(list, DeviceInfo{
			Name: d.Name(),
			ID:   d.ID,
		})
	}
	return filterDevices(list), nil
}

// ListOutputDevices returns a list of available audio output (playback)
// devices.  Devices matching known ALSA subdevice patterns are filtered out.
func ListOutputDevices() ([]DeviceInfo, error) {
	ctx, err := malgo.InitContext(nil, malgo.ContextConfig{}, nil)
	if err != nil {
		return nil, fmt.Errorf("could not init malgo context: %w", err)
	}
	defer func() {
		_ = ctx.Uninit()
		ctx.Free()
	}()

	devices, err := ctx.Devices(malgo.Playback)
	if err != nil {
		return nil, fmt.Errorf("could not enumerate playback devices: %w", err)
	}

	list := make([]DeviceInfo, 0, len(devices))
	for _, d := range devices {
		list = append(list, DeviceInfo{
			Name: d.Name(),
			ID:   d.ID,
		})
	}
	return filterDevices(list), nil
}

// ProcessFunc is the audio processing callback invoked for each audio block.
//
//   - modulator contains the captured input samples (interleaved f32).
//   - output is the buffer the callback must fill with output samples
//     (interleaved f32).
//   - channels is the number of audio channels.
type ProcessFunc func(modulator []float32, output []float32, channels uint32)

// AudioConfig holds configuration for audio I/O tuned for low latency.
type AudioConfig struct {
	// SampleRate in Hz. 0 means use the device default.
	SampleRate uint32
	// BufferSize is the period size in frames. 0 means use a sensible default.
	BufferSize uint32
}

// AudioStreams holds the running audio input and output streams.
// Call Close() to stop streaming and release resources.
type AudioStreams struct {
	ctx    *malgo.AllocatedContext
	device *malgo.Device
}

// StartAudio opens the default audio input and output devices and starts a
// low-latency duplex stream.  The process callback is called on every audio
// block with the captured input and a buffer to fill with output.
//
// If config.SampleRate is 0, 48000 is used.
// If config.BufferSize is 0, 128 frames is used.
func StartAudio(config AudioConfig, process ProcessFunc) (*AudioStreams, error) {
	ctx, err := malgo.InitContext(nil, malgo.ContextConfig{}, nil)
	if err != nil {
		return nil, fmt.Errorf("could not init malgo context: %w", err)
	}

	deviceConfig := malgo.DefaultDeviceConfig(malgo.Duplex)
	deviceConfig.Capture.Format = malgo.FormatF32
	deviceConfig.Capture.Channels = 2
	deviceConfig.Playback.Format = malgo.FormatF32
	deviceConfig.Playback.Channels = 2

	if config.SampleRate > 0 {
		deviceConfig.SampleRate = config.SampleRate
	} else {
		deviceConfig.SampleRate = 48000
	}
	if config.BufferSize > 0 {
		deviceConfig.PeriodSizeInFrames = config.BufferSize
	} else {
		deviceConfig.PeriodSizeInFrames = 128
	}
	deviceConfig.Periods = 2
	deviceConfig.PerformanceProfile = malgo.LowLatency

	onData := func(pOutputSamples, pInputSamples []byte, framecount uint32) {
		const channels = uint32(2)
		sampleCount := framecount * channels

		inputSamples := bytesToFloat32Slice(pInputSamples, sampleCount)
		outputSamples := bytesToFloat32Slice(pOutputSamples, sampleCount)

		process(inputSamples, outputSamples, channels)
	}

	device, err := malgo.InitDevice(ctx.Context, deviceConfig, malgo.DeviceCallbacks{
		Data: onData,
	})
	if err != nil {
		_ = ctx.Uninit()
		ctx.Free()
		return nil, fmt.Errorf("could not init duplex device: %w", err)
	}

	if err := device.Start(); err != nil {
		device.Uninit()
		_ = ctx.Uninit()
		ctx.Free()
		return nil, fmt.Errorf("could not start duplex device: %w", err)
	}

	return &AudioStreams{
		ctx:    ctx,
		device: device,
	}, nil
}

// Close stops the audio streams and releases all resources.
func (as *AudioStreams) Close() {
	if as.device != nil {
		as.device.Uninit()
		as.device = nil
	}
	if as.ctx != nil {
		_ = as.ctx.Uninit()
		as.ctx.Free()
		as.ctx = nil
	}
}

// bytesToFloat32Slice converts a byte slice to a float32 slice without
// copying. The returned slice shares memory with the input. sampleCount is
// the number of float32 values expected in the byte slice.
func bytesToFloat32Slice(data []byte, sampleCount uint32) []float32 {
	if len(data) == 0 || sampleCount == 0 {
		return nil
	}
	return unsafe.Slice((*float32)(unsafe.Pointer(&data[0])), sampleCount)
}
