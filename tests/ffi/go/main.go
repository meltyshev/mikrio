// A private cgo consumer of the installed C library. No engine implementation or
// Go pointers are retained by C. Build paths are supplied through CGO_* variables.
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
	"os"
	"unsafe"
)

func span(value string) (C.mikrio_v1_span_t, func()) {
	buffer := C.CString(value)

	view := C.mikrio_v1_span_t{
		data: (*C.uint8_t)(unsafe.Pointer(buffer)),
		len:  C.uint64_t(len(value)),
	}

	return view, func() { C.free(unsafe.Pointer(buffer)) }
}

func statusError(status C.uint32_t, failure *C.mikrio_v1_error_t) error {
	if failure == nil && status != 0 {
		return fmt.Errorf("C status %d did not provide an owned error", status)
	}

	defer C.mikrio_v1_error_release(failure)

	if status == 0 {
		if failure != nil {
			return errors.New("successful C call returned an unexpected error")
		}

		return nil
	}

	view := C.mikrio_v1_error_view(failure)
	// The ABI bounds error strings to 1024 Unicode scalars, safely below C.int.
	code := C.GoStringN((*C.char)(unsafe.Pointer(view.code.data)), C.int(view.code.len))
	message := C.GoStringN((*C.char)(unsafe.Pointer(view.message.data)), C.int(view.message.len))

	return fmt.Errorf("%s: %s (mutation outcome %d)", code, message, view.mutation_outcome)
}

func openStore(path string) (*C.mikrio_v1_store_t, error) {
	directory, release := span(path)
	defer release()

	options := C.mikrio_v1_open_options_t{
		struct_size: C.sizeof_mikrio_v1_open_options_t,
		directory:   directory,
	}

	var store *C.mikrio_v1_store_t
	var failure *C.mikrio_v1_error_t
	status := C.mikrio_v1_store_open(&options, &store, &failure)
	return store, statusError(status, failure)
}

func closeStore(store *C.mikrio_v1_store_t) error {
	var failure *C.mikrio_v1_error_t
	status := C.mikrio_v1_store_close(store, &failure)
	return statusError(status, failure)
}

func roundTrip(path string) (result error) {
	store, err := openStore(path)

	if err != nil {
		return err
	}

	defer func() {
		result = errors.Join(result, closeStore(store))
		C.mikrio_v1_store_release(store)
	}()

	bucket, releaseBucket := span("cgo-bucket")
	defer releaseBucket()
	key, releaseKey := span("literal/%2F/雪")
	defer releaseKey()

	input := C.mikrio_v1_bucket_input_t{
		struct_size: C.sizeof_mikrio_v1_bucket_input_t,
		bucket:      bucket,
	}

	var failure *C.mikrio_v1_error_t
	status := C.mikrio_v1_store_create_bucket(store, &input, &failure)

	if err := statusError(status, failure); err != nil {
		return err
	}

	put := C.mikrio_v1_put_input_t{
		struct_size: C.sizeof_mikrio_v1_put_input_t,
		bucket:      bucket,
		key:         key,
	}

	var writer *C.mikrio_v1_writer_t
	status = C.mikrio_v1_store_begin_put_object(store, &put, &writer, &failure)

	if err := statusError(status, failure); err != nil {
		return err
	}

	defer C.mikrio_v1_writer_release(writer)
	payload := bytes.Repeat([]byte{0, 1, 255, 7}, 32769)
	buffer := C.CBytes(payload)
	defer C.free(buffer)

	chunk := C.mikrio_v1_span_t{
		data: (*C.uint8_t)(buffer),
		len:  C.uint64_t(len(payload)),
	}

	status = C.mikrio_v1_writer_write(writer, chunk, &failure)

	if err := statusError(status, failure); err != nil {
		return err
	}

	var output *C.mikrio_v1_put_output_t
	status = C.mikrio_v1_writer_finish(writer, &output, &failure)

	if err := statusError(status, failure); err != nil {
		return err
	}

	C.mikrio_v1_put_output_release(output)

	if err := closeStore(store); err != nil {
		return err
	}

	reopened, err := openStore(path)

	if err != nil {
		return err
	}

	defer func() {
		result = errors.Join(result, closeStore(reopened))
		C.mikrio_v1_store_release(reopened)
	}()

	return checkObject(reopened, bucket, key, payload)
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: mikrio-cgo-example STORE_DIRECTORY")
		os.Exit(2)
	}

	if err := roundTrip(os.Args[1]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	fmt.Println("PASS cgo_open_write_read_close_reopen")
}
