// Package vocoder provides MIDI input handling using gitlab.com/gomidi/midi/v2.
//
// It offers functions to list available MIDI input ports, start listening to a
// specific port by name, and track active (held) notes in a thread-safe manner
// suitable for polyphonic input.
package vocoder

import (
	"fmt"
	"sync"

	"gitlab.com/gomidi/midi/v2"
	"gitlab.com/gomidi/midi/v2/drivers/rtmididrv"
)

// ActiveNotes tracks which MIDI note numbers are currently held down.
// It is safe for concurrent use from multiple goroutines.
type ActiveNotes struct {
	mu    sync.Mutex
	notes map[uint8]bool
}

// NewActiveNotes returns an initialised ActiveNotes with no notes held.
func NewActiveNotes() *ActiveNotes {
	return &ActiveNotes{notes: make(map[uint8]bool)}
}

// On marks a note as active (held). This should be called on Note On events
// with velocity > 0.
func (a *ActiveNotes) On(note uint8) {
	a.mu.Lock()
	a.notes[note] = true
	a.mu.Unlock()
}

// Off removes a note from the active set. This should be called on Note Off
// events and Note On events with velocity 0.
func (a *ActiveNotes) Off(note uint8) {
	a.mu.Lock()
	delete(a.notes, note)
	a.mu.Unlock()
}

// List returns a snapshot of all currently active note numbers.
// The returned slice is in no particular order.
func (a *ActiveNotes) List() []uint8 {
	a.mu.Lock()
	result := make([]uint8, 0, len(a.notes))
	for note := range a.notes {
		result = append(result, note)
	}
	a.mu.Unlock()
	return result
}

// IsOn reports whether a given note is currently active.
func (a *ActiveNotes) IsOn(note uint8) bool {
	a.mu.Lock()
	on := a.notes[note]
	a.mu.Unlock()
	return on
}

// Clear removes all notes from the active set.
func (a *ActiveNotes) Clear() {
	a.mu.Lock()
	a.notes = make(map[uint8]bool)
	a.mu.Unlock()
}

// Count returns the number of currently active notes.
func (a *ActiveNotes) Count() int {
	a.mu.Lock()
	n := len(a.notes)
	a.mu.Unlock()
	return n
}

// ListInputPorts returns the human-readable names of every available MIDI
// input port on the system. It returns an empty slice (not nil) when no
// ports are present.
func ListInputPorts() ([]string, error) {
	drv, err := rtmididrv.New()
	if err != nil {
		return nil, fmt.Errorf("creating MIDI driver: %w", err)
	}
	defer drv.Close()

	ins := midi.GetInPorts()

	names := make([]string, 0, len(ins))
	for _, in := range ins {
		names = append(names, in.String())
	}
	return names, nil
}

// ListenToPort opens the MIDI input port whose name matches portName and
// starts receiving events. Note On and Note Off messages update the supplied
// ActiveNotes; all other messages are ignored.
//
// It returns a stop function that must be called to disconnect and release
// resources.
func ListenToPort(portName string, active *ActiveNotes) (stop func(), err error) {
	drv, err := rtmididrv.New()
	if err != nil {
		return nil, fmt.Errorf("creating MIDI driver: %w", err)
	}

	ins := midi.GetInPorts()

	// Find the port with the matching name.
	var targetIdx = -1
	for i, in := range ins {
		if in.String() == portName {
			targetIdx = i
			break
		}
	}
	if targetIdx < 0 {
		drv.Close()
		return nil, fmt.Errorf("MIDI input port %q not found", portName)
	}

	listenStop, err := midi.ListenTo(ins[targetIdx], func(msg midi.Message, _ int32) {
		var ch, key, vel uint8
		switch {
		case msg.GetNoteOn(&ch, &key, &vel):
			// Note On with velocity 0 is equivalent to Note Off (MIDI convention).
			if vel > 0 {
				active.On(key)
			} else {
				active.Off(key)
			}
		case msg.GetNoteOff(&ch, &key, &vel):
			active.Off(key)
		}
	})
	if err != nil {
		drv.Close()
		return nil, fmt.Errorf("listening to MIDI port %q: %w", portName, err)
	}

	return func() {
		listenStop()
		drv.Close()
	}, nil
}
