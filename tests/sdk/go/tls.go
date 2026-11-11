package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"io"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

// This producer owns no complete payload and deliberately has no Seek method.
type patternedReader struct{ remainingBytes int64 }

func (reader *patternedReader) Read(buffer []byte) (int, error) {
	if reader.remainingBytes == 0 {
		return 0, io.EOF
	}

	chunkBytes := int(min(int64(len(buffer)), reader.remainingBytes))

	for index := range buffer[:chunkBytes] {
		buffer[index] = 0xa7
	}

	reader.remainingBytes -= int64(chunkBytes)
	return chunkBytes, nil
}

func nonseekableTLS(ctx context.Context, clients *fixture) (err error) {
	if clients.transport.base.TLSClientConfig == nil {
		return fail("TLS streaming scenario requires its explicitly trusted proxy")
	}

	const key = "go-tls-stream"
	const payloadBytes = 1024*1024 + 17
	defer func() { err = errors.Join(err, clients.remove(key)) }()
	body := &patternedReader{remainingBytes: payloadBytes}

	put, err := clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket:        aws.String("photos"),
		Key:           aws.String(key),
		Body:          body,
		ContentLength: aws.Int64(payloadBytes),
		ContentType:   aws.String("application/octet-stream"),
	})

	if err != nil || body.remainingBytes != 0 || clients.transport.trailers.Load() != 1 {
		return fail("nonseekable TLS PUT did not stream a normal SDK CRC32 trailer")
	}

	// Independent expected bytes are produced only after the upload has settled.
	expected := bytes.Repeat([]byte{0xa7}, payloadBytes)
	checksum := base64.StdEncoding.EncodeToString(binary.BigEndian.AppendUint32(nil, crc32.ChecksumIEEE(expected)))

	if aws.ToString(put.ChecksumCRC32) != checksum {
		return fail("TLS PUT checksum response differs from independent CRC32")
	}

	get, err := clients.peer.GetObject(ctx, &s3.GetObjectInput{
		Bucket:       aws.String("photos"),
		Key:          aws.String(key),
		ChecksumMode: types.ChecksumModeEnabled,
	})

	if err != nil {
		return fail("peer GET failed after nonseekable TLS PUT")
	}

	content, err := readBody(get.Body, payloadBytes)

	if err != nil {
		return err
	}

	if !bytes.Equal(content, expected) || aws.ToString(get.ChecksumCRC32) != checksum ||
		aws.ToString(get.ETag) != aws.ToString(put.ETag) {
		return fail("peer GET bytes or integrity differ after nonseekable TLS PUT")
	}

	return nil
}
