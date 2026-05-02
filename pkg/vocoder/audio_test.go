package vocoder

import (
	"testing"
)

// --- AUD-FL: Audio device filtering tests ---

// AUD-FL-01: isRelevantDeviceName rejects surround devices.
func TestFilterRejectsSurround(t *testing.T) {
	for _, name := range []string{"surround40", "surround51", "Surround71"} {
		if isRelevantDeviceName(name) {
			t.Errorf("expected %q to be rejected", name)
		}
	}
}

// AUD-FL-02: isRelevantDeviceName rejects front, iec958, dmix, dsnoop.
func TestFilterRejectsKnownBad(t *testing.T) {
	for _, name := range []string{
		"front",
		"iec958:CARD=Intel",
		"dmix:CARD=Intel",
		"dsnoop:CARD=Intel",
	} {
		if isRelevantDeviceName(name) {
			t.Errorf("expected %q to be rejected", name)
		}
	}
}

// AUD-FL-03: isRelevantDeviceName keeps sysdefault, default, hw, usb, pulse, pipewire.
func TestFilterKeepsRelevant(t *testing.T) {
	for _, name := range []string{
		"sysdefault:CARD=Intel",
		"default",
		"hw:CARD=Intel,DEV=0",
		"usb:CARD=USB",
		"pulse",
		"pipewire",
	} {
		if !isRelevantDeviceName(name) {
			t.Errorf("expected %q to be kept", name)
		}
	}
}

// AUD-FL-04: dedupDevices removes duplicates, keeps first occurrence.
func TestDedupRemovesDuplicates(t *testing.T) {
	devices := []DeviceInfo{
		{Name: "default"},
		{Name: "hw:CARD=Intel"},
		{Name: "default"},
	}
	result := dedupDevices(devices)
	if len(result) != 2 {
		t.Fatalf("expected 2 devices, got %d", len(result))
	}
	if result[0].Name != "default" {
		t.Errorf("expected first device 'default', got %q", result[0].Name)
	}
	if result[1].Name != "hw:CARD=Intel" {
		t.Errorf("expected second device 'hw:CARD=Intel', got %q", result[1].Name)
	}
}

// AUD-FL-05: filterDevices combines filtering and dedup.
func TestFilterDevicesCombined(t *testing.T) {
	devices := []DeviceInfo{
		{Name: "default"},
		{Name: "surround40"},
		{Name: "front"},
		{Name: "sysdefault:CARD=Intel"},
		{Name: "default"},
	}
	result := filterDevices(devices)
	if len(result) != 2 {
		t.Fatalf("expected 2 devices, got %d", len(result))
	}
	if result[0].Name != "default" {
		t.Errorf("expected first device 'default', got %q", result[0].Name)
	}
	if result[1].Name != "sysdefault:CARD=Intel" {
		t.Errorf("expected second device 'sysdefault:CARD=Intel', got %q", result[1].Name)
	}
}

// AUD-FL-06: isRelevantDeviceName rejects null, file:, rate, speexrate.
func TestFilterRejectsMiscBad(t *testing.T) {
	for _, name := range []string{
		"null",
		"file:/tmp/out.wav",
		"rate_converter",
		"speexrate",
	} {
		if isRelevantDeviceName(name) {
			t.Errorf("expected %q to be rejected", name)
		}
	}
}

// AUD-FL-07: dedupDevices on empty list returns empty.
func TestDedupEmpty(t *testing.T) {
	devices := []DeviceInfo{}
	result := dedupDevices(devices)
	if len(result) != 0 {
		t.Fatalf("expected 0 devices, got %d", len(result))
	}
}

// AUD-FL-08: filterDevices on all-filtered returns empty.
func TestFilterAllRemoved(t *testing.T) {
	devices := []DeviceInfo{
		{Name: "surround40"},
		{Name: "front"},
		{Name: "dmix:CARD=Intel"},
	}
	result := filterDevices(devices)
	if len(result) != 0 {
		t.Fatalf("expected 0 devices, got %d", len(result))
	}
}
