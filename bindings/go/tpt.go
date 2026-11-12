// Package tpt is a Go binding for tpt-search, a hybrid search engine that combines text,
// vectors and graphs.
//
// The binding calls the C ABI in crates/tpt-search-capi through cgo. Build the library
// first (see README.md), then:
//
//	engine := tpt.NewEngine()
//	defer engine.Close()
//	err := engine.CreateIndex("docs", schema)
//	err = engine.Upsert("docs", "d1", map[string]any{"title": "common rare marker"})
//	result, err := engine.Search("docs", `MATCH(title, "rare")`, 10)
package tpt

/*
#cgo CFLAGS: -I${SRCDIR}/../../crates/tpt-search-capi/include
#cgo LDFLAGS: -ltpt_search_capi
#include <stdlib.h>
#include "tpt_search.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// Sentinel errors. Use errors.Is to test for them; the returned error carries the message.
var (
	ErrNotFound        = errors.New("tpt: not found")
	ErrInvalidArgument = errors.New("tpt: invalid argument")
	ErrInvalidInput    = errors.New("tpt: invalid input")
	ErrConflict        = errors.New("tpt: conflict")
	ErrTimeout         = errors.New("tpt: query timed out")
	ErrInternal        = errors.New("tpt: internal error")
	ErrClosed          = errors.New("tpt: engine is closed")
)

// Error is a failed call. Kind is one of the sentinel errors and Message describes the cause.
type Error struct {
	Kind    error
	Message string
}

func (e *Error) Error() string { return e.Kind.Error() + ": " + e.Message }

// Unwrap returns the sentinel, so errors.Is works.
func (e *Error) Unwrap() error { return e.Kind }

// Engine holds named indexes. It is safe for concurrent use. Call Close when finished;
// an unreleased engine is also freed by the garbage collector.
type Engine struct {
	mu sync.RWMutex // guards h against Close racing with calls
	h  *C.TptEngine
}

// NewEngine creates an engine with no indexes.
func NewEngine() *Engine {
	e := &Engine{h: C.tpt_engine_new()}
	runtime.SetFinalizer(e, func(e *Engine) { e.Close() })
	return e
}

// Close releases the engine and every index in it. Calling Close more than once is safe.
func (e *Engine) Close() {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.h != nil {
		C.tpt_engine_free(e.h)
		e.h = nil
	}
}

// call runs one C call under the read lock. The error message is read on the same OS thread
// that made the call, because the library keeps it in thread-local storage.
func (e *Engine) call(op func(h *C.TptEngine) C.int) error {
	e.mu.RLock()
	defer e.mu.RUnlock()
	if e.h == nil {
		return ErrClosed
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	return checkStatus(op(e.h))
}

// checkStatus turns a status code into an error, using the library's last-error message.
func checkStatus(code C.int) error {
	if code == C.TPT_OK {
		return nil
	}
	message := C.GoString(C.tpt_last_error())
	var kind error
	switch code {
	case C.TPT_ERR_INVALID_ARGUMENT:
		kind = ErrInvalidArgument
	case C.TPT_ERR_NOT_FOUND:
		kind = ErrNotFound
	case C.TPT_ERR_INVALID_INPUT:
		kind = ErrInvalidInput
	case C.TPT_ERR_CONFLICT:
		kind = ErrConflict
	case C.TPT_ERR_TIMEOUT:
		kind = ErrTimeout
	default:
		kind = ErrInternal
	}
	return &Error{Kind: kind, Message: message}
}

// CreateIndex creates an index from a schema. schema is marshalled to JSON, so pass a struct
// or a map with the same shape the HTTP API uses. Returns ErrConflict if the name is taken.
func (e *Engine) CreateIndex(name string, schema any) error {
	text, err := json.Marshal(schema)
	if err != nil {
		return &Error{Kind: ErrInvalidInput, Message: "schema: " + err.Error()}
	}
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	cSchema := C.CString(string(text))
	defer C.free(unsafe.Pointer(cSchema))
	return e.call(func(h *C.TptEngine) C.int {
		return C.tpt_create_index(h, cName, cSchema)
	})
}

// DropIndex removes an index. Returns ErrNotFound if it does not exist.
func (e *Engine) DropIndex(name string) error {
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	return e.call(func(h *C.TptEngine) C.int {
		return C.tpt_drop_index(h, cName)
	})
}

// Upsert inserts or replaces a document. doc maps field names to values.
func (e *Engine) Upsert(index, id string, doc map[string]any) error {
	text, err := json.Marshal(doc)
	if err != nil {
		return &Error{Kind: ErrInvalidInput, Message: "document: " + err.Error()}
	}
	cIndex := C.CString(index)
	defer C.free(unsafe.Pointer(cIndex))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	cBody := C.CString(string(text))
	defer C.free(unsafe.Pointer(cBody))
	return e.call(func(h *C.TptEngine) C.int {
		return C.tpt_upsert(h, cIndex, cID, cBody)
	})
}

// Get reads a document. Returns ErrNotFound if the index or the document is missing.
func (e *Engine) Get(index, id string) (map[string]any, error) {
	cIndex := C.CString(index)
	defer C.free(unsafe.Pointer(cIndex))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	var out *C.char
	err := e.call(func(h *C.TptEngine) C.int {
		return C.tpt_get(h, cIndex, cID, &out)
	})
	if err != nil {
		return nil, err
	}
	defer C.tpt_string_free(out)
	var doc struct {
		Fields map[string]any `json:"fields"`
	}
	if err := json.Unmarshal([]byte(C.GoString(out)), &doc); err != nil {
		return nil, &Error{Kind: ErrInternal, Message: "decoding document: " + err.Error()}
	}
	return doc.Fields, nil
}

// Delete removes a document. The result reports whether it was present.
func (e *Engine) Delete(index, id string) (bool, error) {
	cIndex := C.CString(index)
	defer C.free(unsafe.Pointer(cIndex))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	var existed C.bool
	err := e.call(func(h *C.TptEngine) C.int {
		return C.tpt_delete(h, cIndex, cID, &existed)
	})
	return bool(existed), err
}

// Hit is one search result.
type Hit struct {
	ID         string         `json:"id"`
	Document   map[string]any `json:"document"`
	Highlights []Highlight    `json:"highlights"`
}

// Highlight gives the byte ranges of a field that matched the query.
type Highlight struct {
	Field string   `json:"field"`
	Spans [][2]int `json:"spans"`
}

// SearchResult is the outcome of a query.
type SearchResult struct {
	TotalHits int    `json:"total_hits"`
	TookMs    uint64 `json:"took_ms"`
	Hits      []Hit  `json:"hits"`
}

// MaxLimit is the largest limit Search accepts.
const MaxLimit = C.TPT_MAX_LIMIT

// Search runs a query. limit is the number of hits to return: zero selects the default,
// and values above MaxLimit or below zero are rejected.
func (e *Engine) Search(index, query string, limit int) (*SearchResult, error) {
	if limit < 0 {
		return nil, &Error{Kind: ErrInvalidInput, Message: "limit must not be negative"}
	}
	cIndex := C.CString(index)
	defer C.free(unsafe.Pointer(cIndex))
	cQuery := C.CString(query)
	defer C.free(unsafe.Pointer(cQuery))
	var out *C.char
	err := e.call(func(h *C.TptEngine) C.int {
		return C.tpt_search(h, cIndex, cQuery, C.size_t(limit), &out)
	})
	if err != nil {
		return nil, err
	}
	defer C.tpt_string_free(out)
	var result SearchResult
	if err := json.Unmarshal([]byte(C.GoString(out)), &result); err != nil {
		return nil, fmt.Errorf("tpt: decoding search result: %w", err)
	}
	return &result, nil
}

// Version returns the version of the native library.
func Version() string {
	return C.GoString(C.tpt_version())
}
