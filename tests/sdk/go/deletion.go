package main

import (
	"context"
	"errors"
	"net/http"
	"strings"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
	smithyhttp "github.com/aws/smithy-go/transport/http"
)

func deleteBatch(ctx context.Context, clients *fixture) (err error) {
	key := "go-batch"
	defer func() { err = errors.Join(err, clients.remove(key)) }()

	put, err := clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket: aws.String("photos"),
		Key:    &key,
		Body:   strings.NewReader("payload"),
	})

	if err != nil || put.ETag == nil {
		return fail("Go batch fixture publication failed")
	}

	output, err := clients.peer.DeleteObjects(ctx, &s3.DeleteObjectsInput{
		Bucket: aws.String("photos"),
		Delete: &types.Delete{
			Objects: []types.ObjectIdentifier{
				{Key: &key, ETag: aws.String(`"wrong"`)},
				{Key: &key, ETag: put.ETag},
				{Key: &key, ETag: put.ETag},
				{Key: &key},
			},
		},
	})

	if err != nil {
		return fail("Go batch default digest or request failed")
	}

	if len(output.Deleted) != 2 || len(output.Errors) != 2 || aws.ToString(output.Deleted[0].Key) != key || aws.ToString(output.Deleted[1].Key) != key ||
		aws.ToString(output.Errors[0].Code) != "PreconditionFailed" || aws.ToString(output.Errors[1].Code) != "NoSuchKey" {
		return fail("Go batch duplicate conditions differ")
	}

	quiet, err := clients.peer.DeleteObjects(ctx, &s3.DeleteObjectsInput{
		Bucket: aws.String("photos"),
		Delete: &types.Delete{
			Quiet: aws.Bool(true),
			Objects: []types.ObjectIdentifier{
				{Key: &key},
				{Key: &key, ETag: aws.String("*")},
			},
		},
	})

	if err != nil || len(quiet.Deleted) != 0 || len(quiet.Errors) != 1 || aws.ToString(quiet.Errors[0].Code) != "NoSuchKey" {
		return fail("Go quiet batch result differs")
	}

	_, err = clients.first.HeadObject(ctx, &s3.HeadObjectInput{Bucket: aws.String("photos"), Key: &key})
	var response *smithyhttp.ResponseError

	if !errors.As(err, &response) || response.HTTPStatusCode() != http.StatusNotFound {
		return fail("Go batch deletion did not leave the expected missing object")
	}

	return nil
}
