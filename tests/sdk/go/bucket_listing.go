package main

import (
	"context"
	"errors"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

func bucketPages(ctx context.Context, clients *fixture) (err error) {
	created := []string{}

	defer func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()

		for _, name := range created {
			_, cleanupErr := clients.first.DeleteBucket(cleanup, &s3.DeleteBucketInput{Bucket: aws.String(name)})

			if cleanupErr != nil {
				err = errors.Join(err, fail("owned bucket cleanup failed"))
			}
		}
	}()

	for _, name := range []string{"listener-node", "listener-python"} {
		if _, err := clients.first.CreateBucket(ctx, &s3.CreateBucketInput{Bucket: aws.String(name)}); err != nil {
			return fail("Go bucket-list fixture creation failed")
		}

		created = append(created, name)
	}

	paginator := s3.NewListBucketsPaginator(clients.peer, &s3.ListBucketsInput{
		Prefix:     aws.String("listener-"),
		MaxBuckets: aws.Int32(1),
	})

	pageCount := 0

	for paginator.HasMorePages() {
		page, err := paginator.NextPage(ctx)

		if err != nil {
			return fail("Go bucket continuation failed")
		}

		if pageCount >= len(created) || len(page.Buckets) != 1 || aws.ToString(page.Buckets[0].Name) != created[pageCount] {
			return fail("Go bucket pagination changed order or progress")
		}

		pageCount++

		if page.Buckets[0].CreationDate == nil || aws.ToString(page.Buckets[0].BucketRegion) != "us-east-1" ||
			aws.ToString(page.Prefix) != "listener-" || page.Owner == nil || len(aws.ToString(page.Owner.ID)) != 64 || page.Owner.DisplayName != nil ||
			(page.ContinuationToken != nil) != (pageCount == 1) {
			return fail("Go bucket facts or continuation differ")
		}
	}

	if pageCount != 2 {
		return fail("Go bucket paginator stopped early")
	}

	all, err := clients.peer.ListBuckets(ctx, &s3.ListBucketsInput{})

	if err != nil || len(all.Buckets) != 3 || all.ContinuationToken != nil {
		return fail("Go unpaginated bucket result differs")
	}

	for _, bucket := range all.Buckets {
		if bucket.BucketRegion != nil {
			return fail("Go unpaginated bucket region must be absent")
		}
	}

	return nil
}
