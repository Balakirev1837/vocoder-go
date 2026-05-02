package main

import (
	"fmt"
	"os"

	"vocoder/pkg/vocoder"
)

func main() {
	// Enumerate available audio devices.
	audioInDevs, err := vocoder.ListInputDevices()
	if err != nil {
		fmt.Fprintf(os.Stderr, "warning: could not list audio input devices: %v\n", err)
	}
	audioOutDevs, err := vocoder.ListOutputDevices()
	if err != nil {
		fmt.Fprintf(os.Stderr, "warning: could not list audio output devices: %v\n", err)
	}

	// Enumerate available MIDI input ports.
	midiInPorts, err := vocoder.ListInputPorts()
	if err != nil {
		fmt.Fprintf(os.Stderr, "warning: could not list MIDI input ports: %v\n", err)
	}

	// Convert device lists to string slices.
	audioIn := deviceNames(audioInDevs)
	audioOut := deviceNames(audioOutDevs)
	midiIn := midiNames(midiInPorts)

	// Initialise shared state (config, DSP, active notes).
	state := vocoder.NewSharedState()

	// Launch the Bubble Tea TUI (blocks until quit).
	if _, err := vocoder.RunTUI(state, audioIn, audioOut, midiIn); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

// deviceNames extracts the name strings from a slice of DeviceInfo.
func deviceNames(devices []vocoder.DeviceInfo) []string {
	names := make([]string, len(devices))
	for i, d := range devices {
		names[i] = d.Name
	}
	return names
}

// midiNames is a no-op passthrough for MIDI port name strings.
func midiNames(ports []string) []string {
	return ports
}
