package main

/*
#include <stdlib.h>
#include "mikrio.h"
*/
import "C"

import (
	"bytes"
	"errors"
	"fmt"
)

const concurrentWriterCount = 8

func concurrentRoundTrip(path string) (result error) {
	store, err := openStore(path)

	if err != nil {
		return err
	}

	defer func() {
		result = errors.Join(result, closeStore(store))
		C.mikrio_v1_store_release(store)
	}()

	bucket, releaseBucket := span("concurrent-cgo")
	defer releaseBucket()

	input := C.mikrio_v1_bucket_input_t{
		struct_size: C.sizeof_mikrio_v1_bucket_input_t,
		bucket:      bucket,
	}

	var failure *C.mikrio_v1_error_t
	status := C.mikrio_v1_store_create_bucket(store, &input, &failure)

	if err := statusError(status, failure); err != nil {
		return err
	}

	peer, err := openStore(path)

	if err != nil {
		return err
	}

	defer func() {
		result = errors.Join(result, closeStore(peer))
		C.mikrio_v1_store_release(peer)
	}()

	ready := make(chan struct{}, concurrentWriterCount)
	publish := make(chan struct{})
	completed := make(chan error, concurrentWriterCount)

	for index := range concurrentWriterCount {
		go func() {
			completed <- publishConcurrentObject(store, bucket, index, ready, publish)
		}()
	}

	// Every successful begin owns a distinct writer before any write starts.
	// Even a failed begin signals readiness, and every worker settles before close.
	for range concurrentWriterCount {
		<-ready
	}

	close(publish)

	for range concurrentWriterCount {
		result = errors.Join(result, <-completed)
	}

	if result != nil {
		return result
	}

	if err := closeStore(store); err != nil {
		return err
	}

	for index := range concurrentWriterCount {
		if err := checkConcurrentObject(peer, bucket, index); err != nil {
			return err
		}
	}

	return nil
}

func concurrentKey(index int) string {
	return fmt.Sprintf("concurrent/%02d/雪", index)
}

func concurrentPayload(index int) []byte {
	return bytes.Repeat([]byte{byte(index), 0, 255, 7}, 16385)
}

func publishConcurrentObject(store *C.mikrio_v1_store_t, bucket C.mikrio_v1_span_t, index int, ready chan<- struct{}, publish <-chan struct{}) error {
	key, releaseKey := span(concurrentKey(index))
	defer releaseKey()

	input := C.mikrio_v1_put_input_t{
		struct_size: C.sizeof_mikrio_v1_put_input_t,
		bucket:      bucket,
		key:         key,
	}

	var writer *C.mikrio_v1_writer_t
	var failure *C.mikrio_v1_error_t
	status := C.mikrio_v1_store_begin_put_object(store, &input, &writer, &failure)

	if err := statusError(status, failure); err != nil {
		ready <- struct{}{}
		return fmt.Errorf("begin object %d: %w", index, err)
	}

	defer C.mikrio_v1_writer_release(writer)
	ready <- struct{}{}
	<-publish
	payload := concurrentPayload(index)
	buffer := C.CBytes(payload)
	defer C.free(buffer)

	chunk := C.mikrio_v1_span_t{
		data: (*C.uint8_t)(buffer),
		len:  C.uint64_t(len(payload)),
	}

	status = C.mikrio_v1_writer_write(writer, chunk, &failure)

	if err := statusError(status, failure); err != nil {
		return fmt.Errorf("write object %d: %w", index, err)
	}

	var output *C.mikrio_v1_put_output_t
	status = C.mikrio_v1_writer_finish(writer, &output, &failure)

	if err := statusError(status, failure); err != nil {
		return fmt.Errorf("finish object %d: %w", index, err)
	}

	C.mikrio_v1_put_output_release(output)
	return nil
}

func checkConcurrentObject(store *C.mikrio_v1_store_t, bucket C.mikrio_v1_span_t, index int) error {
	key, releaseKey := span(concurrentKey(index))
	defer releaseKey()

	if err := checkObject(store, bucket, key, concurrentPayload(index)); err != nil {
		return fmt.Errorf("read object %d after peer close: %w", index, err)
	}

	return nil
}
