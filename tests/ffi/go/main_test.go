package main

import (
	"os"
	"testing"
)

func TestPersistenceThroughInstalledCABI(t *testing.T) {
	directory, err := os.MkdirTemp("", "mikrio-cgo-test-")

	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() {
		if t.Failed() {
			t.Logf("retained failed store: %s", directory)
			return
		}

		if err := os.RemoveAll(directory); err != nil {
			t.Errorf("remove successful store: %v", err)
		}
	})

	if err := roundTrip(directory); err != nil {
		t.Fatal(err)
	}
}

func TestConcurrentWritersShareStoreAndSurvivePeerClose(t *testing.T) {
	directory, err := os.MkdirTemp("", "mikrio-cgo-concurrent-")

	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() {
		if t.Failed() {
			t.Logf("retained failed store: %s", directory)
			return
		}

		if err := os.RemoveAll(directory); err != nil {
			t.Errorf("remove successful store: %v", err)
		}
	})

	if err := concurrentRoundTrip(directory); err != nil {
		t.Fatal(err)
	}
}
