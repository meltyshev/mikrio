package main

import (
	"context"
	"errors"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

func readOptions(ctx context.Context, clients *fixture) (err error) {
	key := "go-range"
	defer func() { err = errors.Join(err, clients.remove(key)) }()

	_, err = clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket:      aws.String("photos"),
		Key:         &key,
		Body:        strings.NewReader("testcontent"),
		ContentType: aws.String("text/plain"),
	})

	if err != nil {
		return fail("Go range fixture publication failed")
	}

	expires := time.Date(2037, 10, 21, 7, 28, 0, 0, time.UTC)

	get, err := clients.peer.GetObject(ctx, &s3.GetObjectInput{
		Bucket:                     aws.String("photos"),
		Key:                        &key,
		Range:                      aws.String("bytes=4-7"),
		ChecksumMode:               types.ChecksumModeEnabled,
		ResponseContentType:        aws.String("application/pdf"),
		ResponseContentEncoding:    aws.String("identity"),
		ResponseContentDisposition: aws.String("attachment"),
		ResponseContentLanguage:    aws.String(""),
		ResponseCacheControl:       aws.String("no-store"),
		ResponseExpires:            &expires,
	})

	if err != nil {
		return fail("Go ranged GET failed")
	}

	content, err := readBody(get.Body, 4)

	if err != nil || string(content) != "cont" || aws.ToInt64(get.ContentLength) != 4 || aws.ToString(get.ContentRange) != "bytes 4-7/11" {
		return fail("Go range bytes or selected bounds differ")
	}

	if aws.ToString(get.ContentType) != "application/pdf" || aws.ToString(get.ContentEncoding) != "identity" ||
		aws.ToString(get.ContentDisposition) != "attachment" || get.ContentLanguage == nil || aws.ToString(get.ContentLanguage) != "" ||
		aws.ToString(get.CacheControl) != "no-store" || get.Expires == nil || !get.Expires.Equal(expires) || get.ChecksumCRC32 != nil || get.ChecksumType != "" {
		return fail("Go range overrides or checksum omission differ")
	}

	head, err := clients.peer.HeadObject(ctx, &s3.HeadObjectInput{
		Bucket:                     aws.String("photos"),
		Key:                        &key,
		Range:                      aws.String("bytes=-7"),
		ChecksumMode:               types.ChecksumModeEnabled,
		ResponseContentType:        aws.String("application/pdf"),
		ResponseContentEncoding:    aws.String("identity"),
		ResponseContentDisposition: aws.String("attachment"),
		ResponseContentLanguage:    aws.String(""),
		ResponseCacheControl:       aws.String("no-store"),
		ResponseExpires:            &expires,
	})

	if err != nil {
		return fail("Go ranged HEAD failed")
	}

	if aws.ToInt64(head.ContentLength) != 7 || aws.ToString(head.ContentRange) != "bytes 4-10/11" {
		return fail("Go HEAD selected bounds differ")
	}

	if aws.ToString(head.ContentType) != "application/pdf" || aws.ToString(head.ContentEncoding) != "identity" ||
		aws.ToString(head.ContentDisposition) != "attachment" || head.ContentLanguage == nil || aws.ToString(head.ContentLanguage) != "" ||
		aws.ToString(head.CacheControl) != "no-store" || head.Expires == nil || !head.Expires.Equal(expires) || head.ChecksumCRC32 != nil || head.ChecksumType != "" {
		return fail("Go HEAD range projection differs")
	}

	original, err := clients.peer.HeadObject(ctx, &s3.HeadObjectInput{Bucket: aws.String("photos"), Key: &key})

	if err != nil || aws.ToString(original.ContentType) != "text/plain" || aws.ToInt64(original.ContentLength) != 11 {
		return fail("Go response overrides changed stored metadata")
	}

	return nil
}
