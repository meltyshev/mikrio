package main

import (
	"context"
	"errors"
	"strings"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

func listingPages(ctx context.Context, clients *fixture) (err error) {
	keys := []string{"go-list/a/one", "go-list/a/two", "go-list/b +雪", "go-list/c"}

	defer func() {
		for _, key := range keys {
			err = errors.Join(err, clients.remove(key))
		}
	}()

	for _, key := range keys {
		_, err := clients.first.PutObject(ctx, &s3.PutObjectInput{
			Bucket: aws.String("photos"),
			Key:    aws.String(key),
			Body:   strings.NewReader("abc"),
		})

		if err != nil {
			return fail("Go listing fixture publication failed")
		}
	}

	paginator := s3.NewListObjectsV2Paginator(clients.peer, &s3.ListObjectsV2Input{
		Bucket:    aws.String("photos"),
		Prefix:    aws.String("go-list/"),
		Delimiter: aws.String("/"),
		MaxKeys:   aws.Int32(1),
	})

	pageCount := 0

	for paginator.HasMorePages() {
		page, err := paginator.NextPage(ctx)

		if err != nil {
			return fail("Go listing continuation failed")
		}

		pageCount++

		if pageCount > 3 || aws.ToInt32(page.KeyCount) != 1 || aws.ToBool(page.IsTruncated) != (pageCount < 3) {
			return fail("Go listing count, truncation or progress differs")
		}

		if pageCount == 1 {
			if len(page.Contents) != 0 || len(page.CommonPrefixes) != 1 || aws.ToString(page.CommonPrefixes[0].Prefix) != "go-list/a/" {
				return fail("Go listing lost a prefix-only page")
			}

			continue
		}

		if len(page.Contents) != 1 || len(page.CommonPrefixes) != 0 {
			return fail("Go listing changed object or prefix collections")
		}

		object := page.Contents[0]

		if aws.ToString(object.Key) != keys[pageCount] || aws.ToInt64(object.Size) != 3 ||
			aws.ToString(object.ETag) != `"900150983cd24fb0d6963f7d28e17f72"` || object.LastModified == nil {
			return fail("Go listing lost literal key or detached object facts")
		}
	}

	if pageCount != 3 {
		return fail("Go paginator stopped before the final page")
	}

	return nil
}
