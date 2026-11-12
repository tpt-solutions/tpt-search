package tpt

import (
	"errors"
	"sync"
	"testing"
)

// schema is the shape the HTTP API accepts for an index.
var schema = map[string]any{
	"name":        "docs",
	"primary_key": "id",
	"fields": map[string]any{
		"id":    field("id", false),
		"title": field("title", true),
	},
}

func field(name string, indexed bool) map[string]any {
	return map[string]any{
		"name":          name,
		"field_type":    "Text",
		"indexed":       indexed,
		"stored":        true,
		"analyzer":      nil,
		"vector_config": nil,
	}
}

func engineWithDocs(t *testing.T) *Engine {
	t.Helper()
	e := NewEngine()
	t.Cleanup(e.Close)
	if err := e.CreateIndex("docs", schema); err != nil {
		t.Fatalf("CreateIndex: %v", err)
	}
	docs := map[string]string{
		"d1": "common rare marker",
		"d2": "common words here",
		"d3": "common words elsewhere",
	}
	for id, title := range docs {
		if err := e.Upsert("docs", id, map[string]any{"title": title}); err != nil {
			t.Fatalf("Upsert %s: %v", id, err)
		}
	}
	return e
}

func wantKind(t *testing.T, err, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("error = %v, want kind %v", err, kind)
	}
}

func TestSearchReturnsHitsWithHighlights(t *testing.T) {
	e := engineWithDocs(t)
	result, err := e.Search("docs", `MATCH(title, "rare")`, 0)
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	if result.TotalHits != 1 || len(result.Hits) != 1 {
		t.Fatalf("got %d hits (total %d), want 1", len(result.Hits), result.TotalHits)
	}
	hit := result.Hits[0]
	if hit.ID != "d1" || hit.Document["title"] != "common rare marker" {
		t.Fatalf("unexpected hit: %+v", hit)
	}
	spans := hit.Highlights[0].Spans
	if len(spans) == 0 {
		t.Fatalf("no highlight spans")
	}
	if got := "common rare marker"[spans[0][0]:spans[0][1]]; got != "rare" {
		t.Fatalf("span covers %q, want \"rare\"", got)
	}
}

func TestLimitCapsHitsButNotTotal(t *testing.T) {
	e := engineWithDocs(t)
	result, err := e.Search("docs", `MATCH(title, "common")`, 2)
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	if len(result.Hits) != 2 || result.TotalHits != 3 {
		t.Fatalf("hits=%d total=%d, want 2 and 3", len(result.Hits), result.TotalHits)
	}
}

func TestGetAndDelete(t *testing.T) {
	e := engineWithDocs(t)
	doc, err := e.Get("docs", "d2")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if doc["title"] != "common words here" {
		t.Fatalf("doc = %v", doc)
	}
	if _, err := e.Get("docs", "nope"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("Get missing: %v, want ErrNotFound", err)
	}

	existed, err := e.Delete("docs", "d2")
	if err != nil || !existed {
		t.Fatalf("Delete = %v, %v; want true, nil", existed, err)
	}
	existed, err = e.Delete("docs", "d2")
	if err != nil || existed {
		t.Fatalf("second Delete = %v, %v; want false, nil", existed, err)
	}
}

func TestUpsertReplacesDocument(t *testing.T) {
	e := engineWithDocs(t)
	if err := e.Upsert("docs", "d1", map[string]any{"title": "replaced text"}); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	result, err := e.Search("docs", `MATCH(title, "rare")`, 0)
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	if result.TotalHits != 0 {
		t.Fatalf("old text still matches: %d hits", result.TotalHits)
	}
}

func TestErrorKinds(t *testing.T) {
	e := engineWithDocs(t)

	err := e.CreateIndex("docs", schema)
	wantKind(t, err, ErrConflict)

	err = e.DropIndex("missing")
	wantKind(t, err, ErrNotFound)

	_, err = e.Search("docs", "MATCH(", 0)
	wantKind(t, err, ErrInvalidInput)

	_, err = e.Search("nope", `MATCH(title, "x")`, 0)
	wantKind(t, err, ErrNotFound)

	err = e.Upsert("docs", "d9", map[string]any{"missing": "x"})
	wantKind(t, err, ErrInvalidInput)

	err = NewEngine().CreateIndex("x", map[string]any{"name": "x"})
	wantKind(t, err, ErrInvalidInput)

	_, err = e.Search("docs", `MATCH(title, "common")`, MaxLimit+1)
	wantKind(t, err, ErrInvalidInput)

	_, err = e.Search("docs", `MATCH(title, "common")`, -1)
	wantKind(t, err, ErrInvalidInput)
}

func TestErrorMessageIsReported(t *testing.T) {
	e := engineWithDocs(t)
	_, err := e.Search("docs", "MATCH(", 0)
	var tptErr *Error
	if !errors.As(err, &tptErr) {
		t.Fatalf("error %v is not *tpt.Error", err)
	}
	if tptErr.Message == "" {
		t.Fatalf("empty message")
	}
}

func TestClosedEngineRejectsCalls(t *testing.T) {
	e := NewEngine()
	e.Close()
	e.Close() // idempotent
	wantKind(t, e.CreateIndex("docs", schema), ErrClosed)
	_, err := e.Search("docs", "x", 0)
	wantKind(t, err, ErrClosed)
}

func TestConcurrentSearchesAgree(t *testing.T) {
	e := engineWithDocs(t)
	var wg sync.WaitGroup
	errs := make(chan error, 64)
	for i := 0; i < 16; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 8; j++ {
				result, err := e.Search("docs", `MATCH(title, "common")`, 0)
				if err != nil {
					errs <- err
					return
				}
				if result.TotalHits != 3 {
					errs <- errors.New("wrong total")
					return
				}
				// A failing query on the same goroutine must not leave a stale error behind.
				if _, err := e.Search("docs", "MATCH(", 0); !errors.Is(err, ErrInvalidInput) {
					errs <- errors.New("bad query did not report invalid input")
					return
				}
			}
		}()
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Error(err)
	}
}

func TestVersionIsReported(t *testing.T) {
	if Version() == "" {
		t.Fatal("empty version")
	}
}
