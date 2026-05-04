package vocoder

import (
	"fmt"
	"os/exec"
	"strings"
)

// IsVirtualMicSupported reports whether the pactl command-line tool is
// available on the system PATH, which is required for virtual microphone
// setup and teardown.
func IsVirtualMicSupported() bool {
	_, err := exec.LookPath("pactl")
	return err == nil
}

// SetupVirtualMic creates a PulseAudio null sink named "VocoderVirtualMic"
// using pactl. It returns the module ID printed by pactl to stdout, which
// can be passed to TeardownVirtualMic to remove the sink.
func SetupVirtualMic() (string, error) {
	out, err := exec.Command("pactl", "load-module", "module-null-sink",
		"sink_name=VocoderVirtualMic",
		`sink_properties=device.description="Vocoder_Virtual_Mic"`,
	).Output()
	if err != nil {
		return "", fmt.Errorf("pactl load-module module-null-sink: %w", err)
	}
	return strings.TrimSpace(string(out)), nil
}

// TeardownVirtualMic unloads a previously loaded PulseAudio module identified
// by moduleID (the value returned by SetupVirtualMic).
func TeardownVirtualMic(moduleID string) error {
	if err := exec.Command("pactl", "unload-module", moduleID).Run(); err != nil {
		return fmt.Errorf("pactl unload-module %s: %w", moduleID, err)
	}
	return nil
}
