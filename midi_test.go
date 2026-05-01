package vocoder

import (
	"sync"
	"testing"
)

// --- ActiveNotes unit tests (no MIDI hardware required) ---

func TestActiveNotesOnAndOff(t *testing.T) {
	a := NewActiveNotes()

	a.On(60)
	if !a.IsOn(60) {
		t.Fatal("expected note 60 to be active after On(60)")
	}
	if a.Count() != 1 {
		t.Fatalf("expected Count()=1, got %d", a.Count())
	}

	a.Off(60)
	if a.IsOn(60) {
		t.Fatal("expected note 60 to be inactive after Off(60)")
	}
	if a.Count() != 0 {
		t.Fatalf("expected Count()=0, got %d", a.Count())
	}
}

func TestActiveNotesList(t *testing.T) {
	a := NewActiveNotes()
	a.On(60)
	a.On(62)
	a.On(64)

	notes := a.List()
	if len(notes) != 3 {
		t.Fatalf("expected 3 active notes, got %d", len(notes))
	}

	// Check that all expected notes are present (order is not guaranteed).
	want := map[uint8]bool{60: true, 62: true, 64: true}
	for _, n := range notes {
		if !want[n] {
			t.Fatalf("unexpected note %d in active list", n)
		}
	}
}

func TestActiveNotesClear(t *testing.T) {
	a := NewActiveNotes()
	a.On(60)
	a.On(62)
	a.On(64)
	a.Clear()

	if a.Count() != 0 {
		t.Fatalf("expected 0 active notes after Clear(), got %d", a.Count())
	}
	notes := a.List()
	if len(notes) != 0 {
		t.Fatalf("expected empty list after Clear(), got %v", notes)
	}
}

func TestActiveNotesDuplicateOn(t *testing.T) {
	a := NewActiveNotes()
	a.On(60)
	a.On(60) // duplicate
	if a.Count() != 1 {
		t.Fatalf("expected 1 active note after duplicate On(60), got %d", a.Count())
	}
}

func TestActiveNotesOffNotPresent(t *testing.T) {
	a := NewActiveNotes()
	a.Off(60) // removing a note that was never added should not panic
	if a.Count() != 0 {
		t.Fatalf("expected 0 active notes, got %d", a.Count())
	}
}

func TestActiveNotesConcurrent(t *testing.T) {
	a := NewActiveNotes()
	const n = 128
	var wg sync.WaitGroup

	// Concurrently add all 128 possible note numbers.
	for i := uint8(0); i < n; i++ {
		wg.Add(1)
		go func(note uint8) {
			a.On(note)
			wg.Done()
		}(i)
	}
	wg.Wait()

	if a.Count() != n {
		t.Fatalf("expected %d active notes, got %d", n, a.Count())
	}

	// Concurrently remove all notes.
	for i := uint8(0); i < n; i++ {
		wg.Add(1)
		go func(note uint8) {
			a.Off(note)
			wg.Done()
		}(i)
	}
	wg.Wait()

	if a.Count() != 0 {
		t.Fatalf("expected 0 active notes after removal, got %d", a.Count())
	}
}

func TestActiveNotesBoundaryValues(t *testing.T) {
	a := NewActiveNotes()
	// Note 0 (lowest)
	a.On(0)
	if !a.IsOn(0) {
		t.Fatal("expected note 0 to be active")
	}
	// Note 127 (highest)
	a.On(127)
	if !a.IsOn(127) {
		t.Fatal("expected note 127 to be active")
	}
	if a.Count() != 2 {
		t.Fatalf("expected 2 active notes, got %d", a.Count())
	}
}
