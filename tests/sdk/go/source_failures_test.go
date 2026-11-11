package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

type failedSource struct{ cause error }

func (source failedSource) Read([]byte) (int, error) { return 0, source.cause }
func (failedSource) Seek(int64, int) (int64, error)  { return 0, nil }

func TestSourceFormsAndEarlyFailuresPreserveNormalChecksumPolicy(t *testing.T) {
	producerError := errors.New("owned producer failed")
	path := filepath.Join(t.TempDir(), "source")

	if err := os.WriteFile(path, []byte("123456789"), 0600); err != nil {
		t.Fatal(err)
	}

	file, err := os.Open(path)

	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() {
		if err := file.Close(); err != nil {
			t.Error(err)
		}
	})

	cases := []struct {
		name     string
		body     io.Reader
		cancel   bool
		cause    error
		requests int
	}{
		{name: "file", body: file, requests: 1},
		{name: "nonseekable-http", body: struct{ io.Reader }{strings.NewReader("123456789")}},
		{name: "producer-failure", body: failedSource{producerError}, cause: producerError},
		{name: "canceled-before-call", body: strings.NewReader("123456789"), cancel: true, cause: context.Canceled},
	}

	observations := []map[string]any{}

	for _, scenario := range cases {
		t.Run(scenario.name, func(t *testing.T) {
			transport := &defaultTransport{testing: t, operation: "PutObject"}
			client := defaultClient(transport)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()

			if scenario.cancel {
				cancel()
			}

			_, err := client.PutObject(ctx, &s3.PutObjectInput{
				Bucket: aws.String("bucket"),
				Key:    aws.String("key"),
				Body:   scenario.body,
			})

			if len(transport.exchanges) != scenario.requests {
				t.Fatal("unexpected transport dispatch")
			}

			if scenario.requests == 1 {
				if err != nil || transport.exchanges[0].Headers["x-amz-checksum-crc32"] != "y/Q5Jg==" {
					t.Fatal("file source lost normal integrity behavior")
				}
			} else {
				if err == nil {
					t.Fatal("invalid or canceled source unexpectedly succeeded")
				}

				if scenario.cause != nil && !errors.Is(err, scenario.cause) {
					t.Fatal("SDK lost the producer or cancellation cause")
				}

				if scenario.cause == nil && expectNonseekableHTTP(err) != nil {
					t.Fatal("nonseekable HTTP source lost its checksum rejection")
				}
			}

			observations = append(observations, map[string]any{
				"source":   scenario.name,
				"requests": len(transport.exchanges),
				"rejected": err != nil,
			})
		})
	}

	if destination := os.Getenv("mikrio_sdk_defaults_record"); destination != "" {
		record, err := json.MarshalIndent(map[string]any{
			"origin": "Go v2; authored responses; no service execution",
			"cases":  observations,
		}, "", "  ")

		if err != nil || os.WriteFile(filepath.Join(filepath.Dir(destination), "go-transfer-sources.json"), append(record, '\n'), 0600) != nil {
			t.Fatal("failed to retain source observations")
		}
	}
}
