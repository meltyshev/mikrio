package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/aws/retry"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/smithy-go"
	smithyhttp "github.com/aws/smithy-go/transport/http"
)

type failure struct{ label string }

func (err *failure) Error() string { return err.label }

func fail(label string) error { return &failure{label} }

type transport struct {
	base      *http.Transport
	attempts  atomic.Uint64
	checksums atomic.Uint64
	trailers  atomic.Uint64
}

func (client *transport) RoundTrip(request *http.Request) (*http.Response, error) {
	client.attempts.Add(1)

	if request.Header.Get("X-Amz-Checksum-Crc32") != "" {
		client.checksums.Add(1)
	}

	if request.Header.Get("X-Amz-Trailer") == "x-amz-checksum-crc32" &&
		request.Header.Get("Content-Encoding") == "aws-chunked" &&
		request.Header.Get("X-Amz-Content-Sha256") == "STREAMING-UNSIGNED-PAYLOAD-TRAILER" {
		client.trailers.Add(1)
	}

	return client.base.RoundTrip(request)
}

type fixture struct {
	first     *s3.Client
	peer      *s3.Client
	http      *http.Client
	transport *transport
}

func openFixture() (*fixture, error) {
	scheme := "http"
	var security *tls.Config

	if rootPath := os.Getenv("mikrio_fixture_tls_ca"); rootPath != "" {
		root, err := os.ReadFile(rootPath)

		if err != nil || len(root) > 64*1024 {
			return nil, fail("owned TLS root is unavailable or oversized")
		}

		pool := x509.NewCertPool()

		if !pool.AppendCertsFromPEM(root) {
			return nil, fail("owned TLS root is invalid")
		}

		security = &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12}
		scheme = "https"
	}

	endpoint, err := loopbackEndpoint(os.Getenv("mikrio_fixture_endpoint"), scheme)

	if err != nil {
		return nil, err
	}

	peer, err := loopbackEndpoint(os.Getenv("mikrio_fixture_peer"), scheme)

	if err != nil {
		return nil, err
	}

	accessKeyID := os.Getenv("mikrio_fixture_key")
	secret := os.Getenv("mikrio_fixture_secret")

	if accessKeyID == "" || secret == "" {
		return nil, fail("missing owned fixture credentials")
	}

	connection := &transport{
		base: &http.Transport{
			DialContext:           (&net.Dialer{Timeout: 3 * time.Second}).DialContext,
			TLSClientConfig:       security,
			TLSHandshakeTimeout:   5 * time.Second,
			MaxIdleConns:          2,
			MaxIdleConnsPerHost:   2,
			MaxConnsPerHost:       8,
			IdleConnTimeout:       5 * time.Second,
			ResponseHeaderTimeout: 10 * time.Second,
			ExpectContinueTimeout: time.Second,
			DisableCompression:    true,
		},
	}

	client := &http.Client{
		Transport: connection,
		Timeout:   15 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}

	options := s3.Options{
		Region:       "us-east-1",
		UsePathStyle: true,
		HTTPClient:   client,
		Credentials: aws.CredentialsProviderFunc(func(context.Context) (aws.Credentials, error) {
			return aws.Credentials{
				AccessKeyID:     accessKeyID,
				SecretAccessKey: secret,
				Source:          "owned-fixture",
			}, nil
		}),
		Retryer: retry.NewStandard(func(options *retry.StandardOptions) {
			options.MaxAttempts = 1
		}),
		RequestChecksumCalculation: aws.RequestChecksumCalculationWhenSupported,
		ResponseChecksumValidation: aws.ResponseChecksumValidationWhenSupported,
	}

	options.BaseEndpoint = aws.String(endpoint)
	first := s3.New(options)
	options.BaseEndpoint = aws.String(peer)

	return &fixture{
		first:     first,
		peer:      s3.New(options),
		http:      client,
		transport: connection,
	}, nil
}

func loopbackEndpoint(raw, scheme string) (string, error) {
	endpoint, err := url.Parse(raw)

	if err != nil {
		return "", fail("invalid loopback endpoint")
	}

	if (scheme != "http" && scheme != "https") || endpoint.Scheme != scheme ||
		endpoint.Hostname() != "127.0.0.1" || endpoint.User != nil ||
		endpoint.Path != "" || endpoint.RawQuery != "" || endpoint.ForceQuery || endpoint.Fragment != "" {
		return "", fail("endpoint must be an owned loopback origin with the selected transport")
	}

	port, err := strconv.Atoi(endpoint.Port())

	if err != nil || port < 1 || port > 65535 {
		return "", fail("invalid loopback endpoint port")
	}

	return endpoint.String(), nil
}

func (clients *fixture) close() { clients.transport.base.CloseIdleConnections() }

func readBody(body io.ReadCloser, expectedBytes int) ([]byte, error) {
	content, readErr := io.ReadAll(io.LimitReader(body, int64(expectedBytes)+1))
	closeErr := body.Close()

	if readErr != nil || closeErr != nil || len(content) != expectedBytes {
		return nil, fail("response body, terminal integrity or close failed")
	}

	return content, nil
}

func expectAPI(err error, code string, status int) error {
	var api smithy.APIError
	var response *smithyhttp.ResponseError

	if !errors.As(err, &api) || api.ErrorCode() != code ||
		!errors.As(err, &response) || response.HTTPStatusCode() != status {
		return fail("unexpected SDK error code or HTTP status")
	}

	return nil
}

func (clients *fixture) remove(key string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	_, err := clients.first.DeleteObject(ctx, &s3.DeleteObjectInput{
		Bucket: aws.String("photos"),
		Key:    &key,
	})

	if err != nil {
		return fail("owned object cleanup failed")
	}

	return nil
}
