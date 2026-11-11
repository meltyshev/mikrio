// The owned SDK probe uses only explicitly provisioned loopback credentials.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"strings"
	"time"
)

type scenario struct {
	name string
	run  func(context.Context, *fixture) error
}

var scenarios = []scenario{
	{"seekable_checksums_preserve_literal_keys", seekableObjects},
	{"conditional_write_and_presigned_read", conditionalPresign},
	{"listing_paginator_preserves_groups_and_literal_keys", listingPages},
	{"bucket_paginator_preserves_region_and_continuation", bucketPages},
	{"batch_deletion_preserves_conditions_duplicates_and_quiet", deleteBatch},
	{"ranges_and_response_overrides", readOptions},
	{"cors_configuration_and_private_presigned_read", corsConfiguration},
	{"nonseekable_http_fails_before_transmission", nonseekableHTTP},
	{"nonseekable_tls_streams_with_trailing_checksum", nonseekableTLS},
	{"revoked_key_denied_on_both_listeners", revokedKey},
}

type result struct {
	Selector string `json:"selector"`
	Status   string `json:"status"`
	Executed int    `json:"executed"`
	Skipped  int    `json:"skipped"`
	Failure  string `json:"failure,omitempty"`
}

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(arguments []string) int {
	if (len(arguments) == 1 && arguments[0] == "--list") ||
		(len(arguments) == 2 && arguments[0] == "--list" && arguments[1] == "--tls") {
		useTLS := len(arguments) == 2
		var names []string

		for _, test := range scenarios {
			if (useTLS && test.name == "nonseekable_http_fails_before_transmission") ||
				(!useTLS && test.name == "nonseekable_tls_streams_with_trailing_checksum") {
				continue
			}

			names = append(names, test.name)
		}

		if json.NewEncoder(os.Stdout).Encode(names) != nil {
			return 1
		}

		return 0
	}

	if len(arguments) != 2 || arguments[0] != "--case" {
		return 2
	}

	for _, test := range scenarios {
		if test.name != arguments[1] {
			continue
		}

		row := result{Selector: "go:http-listener:" + test.name, Status: "failing"}
		err := executeScenario(test)
		row.Executed = 1

		if err == nil {
			row.Status = "passing"
		} else {
			// Only owned assertion labels reach output. SDK errors can contain
			// signed URLs or credentials and must never be formatted here.
			row.Failure = failureLabels(err)
		}

		if json.NewEncoder(os.Stdout).Encode(row) != nil || err != nil {
			return 1
		}

		return 0
	}

	return 2
}

func failureLabels(err error) string {
	if joined, ok := err.(interface{ Unwrap() []error }); ok {
		labels := make([]string, 0, len(joined.Unwrap()))

		for _, cause := range joined.Unwrap() {
			labels = append(labels, failureLabels(cause))
		}

		return strings.Join(labels, "; ")
	}

	var assertion *failure

	if errors.As(err, &assertion) {
		return assertion.Error()
	}

	return "unclassified fixture failure"
}

func executeScenario(test scenario) error {
	clients, err := openFixture()

	if err != nil {
		return err
	}

	defer clients.close()
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	return test.run(ctx, clients)
}
