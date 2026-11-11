package main

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"net/url"
	"reflect"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
)

func corsConfiguration(ctx context.Context, clients *fixture) (err error) {
	key := "go-cors"

	defer func() {
		cleanup, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()

		_, cleanupErr := clients.first.DeleteBucketCors(cleanup, &s3.DeleteBucketCorsInput{Bucket: aws.String("photos")})

		if cleanupErr != nil {
			err = errors.Join(err, fail("owned CORS cleanup failed"))
		}

		err = errors.Join(err, clients.remove(key))
	}()

	rules := []types.CORSRule{
		{
			ID:             aws.String("first"),
			AllowedOrigins: []string{"https://app.example"},
			AllowedMethods: []string{"PUT"},
		},
		{
			ID:             aws.String("go-browser-😀"),
			AllowedOrigins: []string{"https://app.example"},
			AllowedMethods: []string{"GET", "HEAD"},
			AllowedHeaders: []string{"content-type"},
			ExposeHeaders:  []string{"ETag"},
			MaxAgeSeconds:  aws.Int32(-2147483648),
		},
	}

	_, err = clients.first.PutBucketCors(ctx, &s3.PutBucketCorsInput{
		Bucket:            aws.String("photos"),
		CORSConfiguration: &types.CORSConfiguration{CORSRules: rules},
	})

	if err != nil {
		return fail("Go SDK CORS replacement failed")
	}

	configuration, err := clients.peer.GetBucketCors(ctx, &s3.GetBucketCorsInput{Bucket: aws.String("photos")})

	if err != nil || !reflect.DeepEqual(configuration.CORSRules, rules) {
		return fail("peer CORS rules differ in order, Unicode or signed max age")
	}

	payload := []byte("private Go CORS object")

	_, err = clients.first.PutObject(ctx, &s3.PutObjectInput{
		Bucket: aws.String("photos"),
		Key:    &key,
		Body:   bytes.NewReader(payload),
	})

	if err != nil {
		return fail("Go CORS fixture object publication failed")
	}

	for _, client := range []*s3.Client{clients.first, clients.peer} {
		if err := corsRead(ctx, clients, client, key, payload, true); err != nil {
			return err
		}
	}

	_, err = clients.peer.DeleteBucketCors(ctx, &s3.DeleteBucketCorsInput{Bucket: aws.String("photos")})

	if err != nil {
		return fail("Go SDK CORS deletion failed")
	}

	_, err = clients.first.GetBucketCors(ctx, &s3.GetBucketCorsInput{Bucket: aws.String("photos")})

	if err := expectAPI(err, "NoSuchCORSConfiguration", http.StatusNotFound); err != nil {
		return err
	}

	for _, client := range []*s3.Client{clients.first, clients.peer} {
		if err := corsRead(ctx, clients, client, key, payload, false); err != nil {
			return err
		}
	}

	return nil
}

func corsRead(ctx context.Context, clients *fixture, client *s3.Client, key string, payload []byte, configured bool) error {
	presigned, err := s3.NewPresignClient(client).PresignGetObject(ctx,
		&s3.GetObjectInput{Bucket: aws.String("photos"), Key: &key},
		func(options *s3.PresignOptions) { options.Expires = time.Minute },
	)

	if err != nil {
		return fail("Go CORS read presigning failed")
	}

	request, err := http.NewRequestWithContext(ctx, presigned.Method, presigned.URL, nil)

	if err != nil {
		return fail("invalid signed CORS request")
	}

	request.Header = presigned.SignedHeader.Clone()
	request.Header.Set("Origin", "https://app.example")
	response, err := clients.http.Do(request)

	if err != nil {
		return fail("signed CORS transport failed")
	}

	content, err := readBody(response.Body, len(payload))

	if err != nil {
		return err
	}

	if response.StatusCode != http.StatusOK || !bytes.Equal(content, payload) {
		return fail("signed CORS read changed private bytes")
	}

	origin, credentials, expose := "", "", ""

	if configured {
		origin, credentials, expose = "https://app.example", "true", "ETag"
	}

	if response.Header.Get("Access-Control-Allow-Origin") != origin ||
		response.Header.Get("Access-Control-Allow-Credentials") != credentials ||
		response.Header.Get("Access-Control-Expose-Headers") != expose {
		return fail("actual response did not use the first complete CORS rule or current configuration")
	}

	unsigned, err := url.Parse(presigned.URL)

	if err != nil {
		return fail("invalid owned object URL")
	}

	unsigned.RawQuery = ""
	request, err = http.NewRequestWithContext(ctx, http.MethodGet, unsigned.String(), nil)

	if err != nil {
		return fail("invalid unsigned CORS request")
	}

	request.Header.Set("Origin", "https://app.example")
	response, err = clients.http.Do(request)

	if err != nil {
		return fail("unsigned CORS transport failed")
	}

	if err := response.Body.Close(); err != nil {
		return fail("unsigned CORS response close failed")
	}

	if response.StatusCode != http.StatusForbidden {
		return fail("CORS configuration granted storage authority")
	}

	return nil
}
