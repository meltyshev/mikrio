package main

import (
	"errors"
	"testing"
)

func TestEndpointRejectsExternalAndAmbiguousOrigins(t *testing.T) {
	for _, endpoint := range []string{
		"", "https://127.0.0.1:1234", "http://example.com:1234", "http://localhost:1234",
		"http://127.0.0.1", "http://127.0.0.1:0", "http://127.0.0.1:65536",
		"http://user:secret@127.0.0.1:1234", "http://127.0.0.1:1234/bucket",
		"http://127.0.0.1:1234?", "http://127.0.0.1:1234?signature=secret",
		"http://127.0.0.1:1234#fragment", "http://127.0.0.1:1234/%2f",
	} {
		t.Run(endpoint, func(t *testing.T) {
			if _, err := loopbackEndpoint(endpoint, "http"); err == nil {
				t.Fatal("unsafe endpoint was accepted")
			}
		})
	}

	const valid = "http://127.0.0.1:1234"

	if actual, err := loopbackEndpoint(valid, "http"); err != nil || actual != valid {
		t.Fatal("owned root endpoint was changed or rejected")
	}

	if actual, err := loopbackEndpoint("https://127.0.0.1:1234", "https"); err != nil || actual != "https://127.0.0.1:1234" {
		t.Fatal("owned TLS root endpoint was changed or rejected")
	}

	if _, err := loopbackEndpoint(valid, "https"); err == nil {
		t.Fatal("TLS fixture accepted a plaintext origin")
	}
}

func TestFailureReportingRetainsCleanupWithoutLeakingForeignErrors(t *testing.T) {
	joined := errors.Join(fail("primary failed"), fail("cleanup failed"))

	if actual := failureLabels(joined); actual != "primary failed; cleanup failed" {
		t.Fatalf("lost owned failure: %s", actual)
	}

	if actual := failureLabels(errors.New("secret foreign error")); actual != "unclassified fixture failure" {
		t.Fatal("foreign error content was exposed")
	}
}
