package main

/*
#include <stdlib.h>
#include "mikrio.h"
*/
import "C"

import (
	"bytes"
	"errors"
)

func checkObject(store *C.mikrio_v1_store_t, bucket, key C.mikrio_v1_span_t, expected []byte) (result error) {
	read := C.mikrio_v1_read_input_t{
		struct_size: C.sizeof_mikrio_v1_read_input_t,
		bucket:      bucket,
		key:         key,
	}

	var facts *C.mikrio_v1_read_output_t
	var body *C.mikrio_v1_body_t
	var failure *C.mikrio_v1_error_t
	status := C.mikrio_v1_store_get_object(store, &read, &facts, &body, &failure)

	if err := statusError(status, failure); err != nil {
		return err
	}

	defer C.mikrio_v1_read_output_release(facts)

	defer func() {
		var failure *C.mikrio_v1_error_t
		status := C.mikrio_v1_body_close(body, &failure)
		result = errors.Join(result, statusError(status, failure))
		C.mikrio_v1_body_release(body)
	}()

	if uint64(C.mikrio_v1_read_output_view(facts).content_length) != uint64(len(expected)) {
		return errors.New("reopened object length differs")
	}

	buffer := C.malloc(4096)
	defer C.free(buffer)

	offset := 0

	for {
		var count C.uint64_t
		status = C.mikrio_v1_body_read_chunk(body, (*C.uint8_t)(buffer), 4096, &count, &failure)

		if err := statusError(status, failure); err != nil {
			return err
		}

		if count == 0 {
			break
		}

		if count > 4096 || uint64(count) > uint64(len(expected)-offset) {
			return errors.New("body exceeded the caller buffer or expected length")
		}

		actual := C.GoBytes(buffer, C.int(count))

		if !bytes.Equal(actual, expected[offset:offset+int(count)]) {
			return errors.New("reopened object bytes differ")
		}

		offset += int(count)
	}

	if offset != len(expected) {
		return errors.New("short object read")
	}

	return nil
}
