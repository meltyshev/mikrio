package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

type defaultExchange struct {
	Operation  string            `json:"operation"`
	Method     string            `json:"method"`
	Headers    map[string]string `json:"headers"`
	BodyBytes  int               `json:"body_bytes"`
	BodySHA256 string            `json:"body_sha256"`
}

type defaultTransport struct {
	testing   *testing.T
	operation string
	exchanges []defaultExchange
}

func (transport *defaultTransport) Do(request *http.Request) (*http.Response, error) {
	transport.testing.Helper()

	if request.URL.Scheme != "http" || request.URL.Host != "127.0.0.1:1" {
		transport.testing.Fatal("SDK request escaped the owned transport")
	}

	var body []byte

	if request.Body != nil {
		defer func() {
			if err := request.Body.Close(); err != nil {
				transport.testing.Error("failed to close the SDK request body")
			}
		}()

		var err error
		body, err = io.ReadAll(io.LimitReader(request.Body, 64*1024+1))

		if err != nil || len(body) > 64*1024 {
			transport.testing.Fatal("unexpected SDK request body")
		}
	}

	headers := map[string]string{}

	for _, name := range []string{
		"content-md5", "content-type", "x-amz-checksum-algorithm", "x-amz-checksum-type",
		"x-amz-sdk-checksum-algorithm", "x-amz-checksum-crc32", "x-amz-checksum-crc32c",
		"x-amz-checksum-crc64nvme", "x-amz-checksum-sha1", "x-amz-checksum-sha256",
		"x-amz-copy-source", "x-amz-copy-source-range", "x-amz-metadata-directive", "x-amz-tagging-directive",
	} {
		if value := request.Header.Get(name); value != "" {
			headers[name] = value
		}
	}

	digest := sha256.Sum256(body)

	transport.exchanges = append(transport.exchanges, defaultExchange{
		Operation:  transport.operation,
		Method:     request.Method,
		Headers:    headers,
		BodyBytes:  len(body),
		BodySHA256: hex.EncodeToString(digest[:]),
	})

	response := map[string]string{
		"CreateMultipartUpload":   `<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><UploadId>owned-upload</UploadId></InitiateMultipartUploadResult>`,
		"CopyObject":              `<CopyObjectResult><ETag>"fixture"</ETag><LastModified>2026-10-01T00:00:00Z</LastModified></CopyObjectResult>`,
		"UploadPartCopy":          `<CopyPartResult><ETag>"fixture"</ETag><LastModified>2026-10-01T00:00:00Z</LastModified></CopyPartResult>`,
		"CompleteMultipartUpload": `<CompleteMultipartUploadResult><Bucket>bucket</Bucket><Key>key</Key><ETag>"fixture-1"</ETag></CompleteMultipartUploadResult>`,
	}[transport.operation]

	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": {"application/xml"}, "Etag": {`"fixture"`}},
		Body:       io.NopCloser(strings.NewReader(response)),
		Request:    request,
	}, nil
}

func TestDefaultChecksumRequestsReachOwnedTransport(t *testing.T) {
	transport := &defaultTransport{testing: t}
	client := defaultClient(transport)
	testDefaultRequests(t, transport, client)
}

func defaultClient(transport s3.HTTPClient) *s3.Client {
	return s3.New(s3.Options{
		BaseEndpoint:     aws.String("http://127.0.0.1:1"),
		Region:           "us-east-1",
		UsePathStyle:     true,
		RetryMaxAttempts: 1,
		HTTPClient:       transport,
		// Match the standard configuration policy and the real listener fixture.
		RequestChecksumCalculation: aws.RequestChecksumCalculationWhenSupported,
		ResponseChecksumValidation: aws.ResponseChecksumValidationWhenSupported,
		Credentials: aws.CredentialsProviderFunc(func(context.Context) (aws.Credentials, error) {
			return aws.Credentials{AccessKeyID: "synthetic-key", SecretAccessKey: "synthetic-secret"}, nil
		}),
	})
}

func testDefaultRequests(t *testing.T, transport *defaultTransport, client *s3.Client) {
	t.Helper()
	ctx := context.Background()
	bucket, key, uploadID := aws.String("bucket"), aws.String("key"), aws.String("owned-upload")
	source := aws.String("source/literal%20/%2B%C3%A9")

	cases := []struct {
		name string
		call func() error
	}{
		{"PutObject", func() error {
			_, err := client.PutObject(ctx, &s3.PutObjectInput{
				Bucket: bucket,
				Key:    key,
				Body:   strings.NewReader("123456789"),
			})

			return err
		}},
		{"CreateMultipartUpload", func() error {
			_, err := client.CreateMultipartUpload(ctx, &s3.CreateMultipartUploadInput{Bucket: bucket, Key: key})
			return err
		}},
		{"UploadPart", func() error {
			_, err := client.UploadPart(ctx, &s3.UploadPartInput{
				Bucket:     bucket,
				Key:        key,
				UploadId:   uploadID,
				PartNumber: aws.Int32(1),
				Body:       strings.NewReader("123456789"),
			})

			return err
		}},
		{"CompleteMultipartUpload", func() error {
			_, err := client.CompleteMultipartUpload(ctx, &s3.CompleteMultipartUploadInput{
				Bucket:   bucket,
				Key:      key,
				UploadId: uploadID,
				MultipartUpload: &types.CompletedMultipartUpload{
					Parts: []types.CompletedPart{
						{PartNumber: aws.Int32(1), ETag: aws.String(`"fixture"`)},
					},
				},
			})

			return err
		}},
		{"CopyObject", func() error {
			_, err := client.CopyObject(ctx, &s3.CopyObjectInput{
				Bucket:     bucket,
				Key:        key,
				CopySource: source,
			})

			return err
		}},
		{"UploadPartCopy", func() error {
			_, err := client.UploadPartCopy(ctx, &s3.UploadPartCopyInput{
				Bucket:     bucket,
				Key:        key,
				UploadId:   uploadID,
				PartNumber: aws.Int32(1),
				CopySource: source,
			})

			return err
		}},
		{"CreateMultipartUpload", func() error {
			_, err := client.CreateMultipartUpload(ctx, &s3.CreateMultipartUploadInput{
				Bucket:            bucket,
				Key:               key,
				ChecksumAlgorithm: types.ChecksumAlgorithmSha256,
				ChecksumType:      types.ChecksumTypeComposite,
			})

			return err
		}},
	}

	for index, scenario := range cases {
		transport.operation = scenario.name

		if err := scenario.call(); err != nil {
			t.Fatalf("request %d (%s) did not reach the owned response", index, scenario.name)
		}
	}

	if len(transport.exchanges) != len(cases) {
		t.Fatal("unexpected request count")
	}

	if destination := os.Getenv("mikrio_sdk_defaults_record"); destination != "" {
		record, err := json.MarshalIndent(map[string]any{
			"origin":      "Go v2 controlled transport; authored responses; no service execution",
			"source_form": "seekable strings.Reader over explicit HTTP endpoint",
			"exchanges":   transport.exchanges,
		}, "", "  ")

		if err != nil || os.WriteFile(destination, append(record, '\n'), 0600) != nil {
			t.Fatal("failed to retain the sanitized transport record")
		}
	}

	for _, index := range []int{0, 2} {
		if transport.exchanges[index].Headers["x-amz-checksum-crc32"] != "y/Q5Jg==" {
			t.Fatalf("request %d changed its default CRC32", index)
		}
	}

	for _, index := range []int{1, 3, 4, 5} {
		for name := range transport.exchanges[index].Headers {
			if strings.Contains(name, "checksum") || strings.HasSuffix(name, "directive") {
				t.Fatalf("request %d introduced a checksum or COPY directive", index)
			}
		}
	}

	for _, index := range []int{4, 5} {
		if transport.exchanges[index].Headers["x-amz-copy-source"] != *source {
			t.Fatal("encoded COPY source changed")
		}
	}

	if transport.exchanges[6].Headers["x-amz-checksum-algorithm"] != "SHA256" || transport.exchanges[6].Headers["x-amz-checksum-type"] != "COMPOSITE" {
		t.Fatal("explicit multipart checksum selection changed")
	}
}
