package vocoder

import (
	"strings"
	"testing"
)

// VM-SUP-01: IsVirtualMicSupported returns a bool without panicking.
func TestIsVirtualMicSupportedNoPanic(t *testing.T) {
	// We just verify it runs without panic; the result depends on the
	// test environment so we don't assert true/false.
	_ = IsVirtualMicSupported()
}

// VM-SUP-02: SetupVirtualMic returns a non-empty trimmed string when pactl
// succeeds. This test is skipped if pactl is not available.
func TestSetupVirtualMicPactlSuccess(t *testing.T) {
	if !IsVirtualMicSupported() {
		t.Skip("pactl not available on this system")
	}
	moduleID, err := SetupVirtualMic()
	if err != nil {
		t.Fatalf("SetupVirtualMic failed: %v", err)
	}
	if moduleID == "" {
		t.Fatal("SetupVirtualMic returned empty module ID")
	}
	if strings.Contains(moduleID, "\n") {
		t.Fatalf("module ID contains newline: %q", moduleID)
	}
	// Clean up.
	if err := TeardownVirtualMic(moduleID); err != nil {
		t.Fatalf("TeardownVirtualMic failed: %v", err)
	}
}

// VM-SUP-03: TeardownVirtualMic returns an error for an invalid module ID.
func TestTeardownVirtualMicInvalidID(t *testing.T) {
	if !IsVirtualMicSupported() {
		t.Skip("pactl not available on this system")
	}
	err := TeardownVirtualMic("999999")
	if err == nil {
		t.Fatal("expected error for invalid module ID, got nil")
	}
}
