package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"io"
	"net/http"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

func seekableObjects(ctx context.Context, clients *fixture) error {
	for _, key := range []string{"go-basic", "go-path/../x//%2Fé/"} {
		if err := literalObject(ctx, clients, key); err != nil {
			return err
		}
	}

	if clients.transport.checksums.Load()+clients.transport.trailers.Load() != 2 {
		return fail("normal SDK CRC32 headers or TLS trailers were not transmitted")
	}

	return nil
}

func literalObject(ctx context.Context, clients *fixture, key string) (err error) {
	defer func() { err = errors.Join(err, clients.remove(key)) }()
	payload := bytes.Repeat([]byte("go-sdk-integrity\x00"), 16384)
	checksumBytes := binary.BigEndian.AppendUint32(nil, crc32.ChecksumIEEE(payload))
	checksum := base64.StdEncoding.EncodeToString(checksumBytes)

	put, err := clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket:      aws.String("photos"),
		Key:         &key,
		Body:        bytes.NewReader(payload),
		ContentType: aws.String("application/octet-stream"),
		Metadata:    map[string]string{"client": "go"},
	})

	if err != nil || aws.ToString(put.ChecksumCRC32) != checksum {
		return fail("seekable PUT or independent CRC32 response check failed")
	}

	get, err := clients.peer.GetObject(ctx, &s3.GetObjectInput{
		Bucket:       aws.String("photos"),
		Key:          &key,
		ChecksumMode: types.ChecksumModeEnabled,
	})

	if err != nil {
		return fail("peer GET failed for exact literal key")
	}

	content, err := readBody(get.Body, len(payload))

	if err != nil {
		return err
	}

	if !bytes.Equal(content, payload) || aws.ToString(get.ChecksumCRC32) != checksum ||
		get.Metadata["client"] != "go" || aws.ToString(get.ETag) != aws.ToString(put.ETag) {
		return fail("peer GET bytes, checksum or metadata differ")
	}

	head, err := clients.peer.HeadObject(ctx, &s3.HeadObjectInput{
		Bucket:       aws.String("photos"),
		Key:          &key,
		ChecksumMode: types.ChecksumModeEnabled,
	})

	if err != nil || aws.ToInt64(head.ContentLength) != int64(len(payload)) ||
		aws.ToString(head.ChecksumCRC32) != checksum || aws.ToString(head.ETag) != aws.ToString(put.ETag) {
		return fail("peer HEAD differs from selected PUT")
	}

	return nil
}

func conditionalPresign(ctx context.Context, clients *fixture) (err error) {
	key := "go-create-only"
	defer func() { err = errors.Join(err, clients.remove(key)) }()
	payload := []byte("immutable-intent")

	_, err = clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket:      aws.String("photos"),
		Key:         &key,
		Body:        bytes.NewReader(payload),
		IfNoneMatch: aws.String("*"),
	})

	if err != nil {
		return fail("initial create-only PUT failed")
	}

	_, err = clients.peer.PutObject(ctx, &s3.PutObjectInput{
		Bucket:      aws.String("photos"),
		Key:         &key,
		Body:        bytes.NewReader([]byte("replacement")),
		IfNoneMatch: aws.String("*"),
	})

	if err = expectAPI(err, "PreconditionFailed", http.StatusPreconditionFailed); err != nil {
		return err
	}

	presigned, err := s3.NewPresignClient(clients.peer).PresignGetObject(ctx,
		&s3.GetObjectInput{Bucket: aws.String("photos"), Key: &key},
		func(options *s3.PresignOptions) { options.Expires = time.Minute },
	)

	if err != nil {
		return fail("Go SDK presigning failed")
	}

	request, err := http.NewRequestWithContext(ctx, presigned.Method, presigned.URL, nil)

	if err != nil {
		return fail("invalid presigned request")
	}

	request.Header = presigned.SignedHeader.Clone()
	response, err := clients.http.Do(request)

	if err != nil {
		return fail("presigned GET transport failed")
	}

	content, err := readBody(response.Body, len(payload))

	if err != nil {
		return err
	}

	if response.StatusCode != http.StatusOK || !bytes.Equal(content, payload) {
		return fail("presigned GET did not preserve the conditional predecessor")
	}

	return nil
}

func nonseekableHTTP(ctx context.Context, clients *fixture) error {
	// Expose only Reader: the default checksum middleware must reject before
	// the transport sees this request, without collecting or weakening it.
	body := struct{ io.Reader }{bytes.NewReader([]byte("nonseekable"))}

	_, err := clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket:        aws.String("photos"),
		Key:           aws.String("go-nonseekable"),
		Body:          body,
		ContentLength: aws.Int64(11),
	})

	if clients.transport.attempts.Load() != 0 {
		return fail("nonseekable plaintext PUT reached the transport")
	}

	return expectNonseekableHTTP(err)
}

func expectNonseekableHTTP(err error) error {
	const expected = "compute input header checksum failed, unseekable stream is not supported without TLS and trailing checksum"

	for cause := err; cause != nil; cause = errors.Unwrap(cause) {
		if cause.Error() == expected {
			return nil
		}
	}

	return fail("nonseekable plaintext PUT did not report the selected checksum limitation")
}

func revokedKey(ctx context.Context, clients *fixture) error {
	for _, client := range []*s3.Client{clients.first, clients.peer} {
		response, err := client.GetObject(ctx, &s3.GetObjectInput{
			Bucket: aws.String("photos"),
			Key:    aws.String("native-seed"),
		})

		if err == nil {
			closeErr := response.Body.Close()

			if closeErr != nil {
				return fail("revoked GET succeeded and response close failed")
			}

			return fail("revoked key retained access")
		}

		if err := expectAPI(err, "AccessDenied", http.StatusForbidden); err != nil {
			return err
		}
	}

	return nil
}
