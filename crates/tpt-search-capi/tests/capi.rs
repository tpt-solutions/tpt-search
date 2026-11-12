//! Exercises the C ABI the way a C caller would: raw pointers, status codes, and JSON out
//! parameters that must be freed.

#![allow(unsafe_code)]

use std::ffi::{c_int, CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use serde_json::Value;
use tpt_search_capi::*;

const SCHEMA: &str = r#"{
    "name": "docs",
    "primary_key": "id",
    "fields": {
        "id": {"name": "id", "field_type": "Text", "indexed": false, "stored": true,
               "analyzer": null, "vector_config": null},
        "title": {"name": "title", "field_type": "Text", "indexed": true, "stored": true,
                  "analyzer": null, "vector_config": null}
    }
}"#;

struct Engine(*mut TptEngine);

impl Engine {
    fn new() -> Self {
        Engine(tpt_engine_new())
    }

    fn handle(&self) -> *const TptEngine {
        self.0
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { tpt_engine_free(self.0) };
    }
}

fn c(text: &str) -> CString {
    CString::new(text).unwrap()
}

/// Takes ownership of a string returned through an out parameter.
fn take(out: *mut c_char) -> String {
    assert!(!out.is_null());
    let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
    unsafe { tpt_string_free(out) };
    text
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(tpt_last_error()) }
        .to_str()
        .unwrap()
        .to_owned()
}

fn engine_with_docs() -> Engine {
    let engine = Engine::new();
    let name = c("docs");
    let schema = c(SCHEMA);
    assert_eq!(
        unsafe { tpt_create_index(engine.handle(), name.as_ptr(), schema.as_ptr()) },
        TPT_OK,
        "{}",
        last_error()
    );
    for (id, title) in [
        ("d1", "common rare marker"),
        ("d2", "common words here"),
        ("d3", "common words elsewhere"),
    ] {
        let body = c(&format!(r#"{{"title": "{title}"}}"#));
        let (i, d) = (c("docs"), c(id));
        assert_eq!(
            unsafe { tpt_upsert(engine.handle(), i.as_ptr(), d.as_ptr(), body.as_ptr()) },
            TPT_OK,
            "{}",
            last_error()
        );
    }
    engine
}

fn search(engine: &Engine, query: &str, limit: usize) -> (c_int, Option<Value>) {
    let (i, q) = (c("docs"), c(query));
    let mut out: *mut c_char = ptr::null_mut();
    let code = unsafe { tpt_search(engine.handle(), i.as_ptr(), q.as_ptr(), limit, &mut out) };
    if code == TPT_OK {
        (code, Some(serde_json::from_str(&take(out)).unwrap()))
    } else {
        (code, None)
    }
}

#[test]
fn engine_handles_null_and_free_safely() {
    unsafe { tpt_engine_free(ptr::null_mut()) };
    unsafe { tpt_string_free(ptr::null_mut()) };
}

#[test]
fn search_returns_hits_with_highlights_in_api_shape() {
    let engine = engine_with_docs();
    let (code, body) = search(&engine, r#"MATCH(title, "rare")"#, 0);
    assert_eq!(code, TPT_OK, "{}", last_error());
    let body = body.unwrap();
    assert_eq!(body["total_hits"], 1);
    let hit = &body["hits"][0];
    assert_eq!(hit["id"], "d1");
    assert_eq!(hit["document"]["title"], "common rare marker");
    assert!(hit["highlights"].is_array());
}

#[test]
fn get_and_delete_round_trip() {
    let engine = engine_with_docs();
    let (i, d) = (c("docs"), c("d2"));
    let mut out: *mut c_char = ptr::null_mut();
    assert_eq!(
        unsafe { tpt_get(engine.handle(), i.as_ptr(), d.as_ptr(), &mut out) },
        TPT_OK
    );
    let doc: Value = serde_json::from_str(&take(out)).unwrap();
    assert_eq!(doc["fields"]["title"], "common words here");

    let mut existed = false;
    assert_eq!(
        unsafe { tpt_delete(engine.handle(), i.as_ptr(), d.as_ptr(), &mut existed) },
        TPT_OK
    );
    assert!(existed);

    let mut out: *mut c_char = ptr::null_mut();
    assert_eq!(
        unsafe { tpt_get(engine.handle(), i.as_ptr(), d.as_ptr(), &mut out) },
        TPT_ERR_NOT_FOUND
    );
    assert!(out.is_null());
    assert_eq!(last_error(), "no document `d2`");
}

#[test]
fn deleting_a_missing_document_reports_false_and_still_succeeds() {
    let engine = engine_with_docs();
    let (i, d) = (c("docs"), c("nope"));
    let mut existed = true;
    assert_eq!(
        unsafe { tpt_delete(engine.handle(), i.as_ptr(), d.as_ptr(), &mut existed) },
        TPT_OK
    );
    assert!(!existed);
}

#[test]
fn creating_an_existing_index_is_a_conflict() {
    let engine = engine_with_docs();
    let (name, schema) = (c("docs"), c(SCHEMA));
    assert_eq!(
        unsafe { tpt_create_index(engine.handle(), name.as_ptr(), schema.as_ptr()) },
        TPT_ERR_CONFLICT
    );
    assert_eq!(last_error(), "index `docs` already exists");
}

#[test]
fn bad_schema_and_bad_documents_are_rejected_with_a_reason() {
    let engine = Engine::new();
    let (name, bad) = (c("x"), c("{not json"));
    assert_eq!(
        unsafe { tpt_create_index(engine.handle(), name.as_ptr(), bad.as_ptr()) },
        TPT_ERR_INVALID_INPUT
    );
    assert!(last_error().starts_with("invalid schema"));

    let engine = engine_with_docs();
    let (i, d, body) = (c("docs"), c("d9"), c(r#"{"missing": "x"}"#));
    assert_eq!(
        unsafe { tpt_upsert(engine.handle(), i.as_ptr(), d.as_ptr(), body.as_ptr()) },
        TPT_ERR_INVALID_INPUT
    );
    assert!(last_error().contains("unknown field `missing`"));
}

#[test]
fn unknown_index_and_bad_query_are_distinct_codes() {
    let engine = engine_with_docs();
    let (i, q) = (c("nope"), c("MATCH(title, \"x\")"));
    let mut out: *mut c_char = ptr::null_mut();
    assert_eq!(
        unsafe { tpt_search(engine.handle(), i.as_ptr(), q.as_ptr(), 0, &mut out) },
        TPT_ERR_NOT_FOUND
    );

    let (code, _) = search(&engine, "MATCH(", 0);
    assert_eq!(code, TPT_ERR_INVALID_INPUT);
    assert!(last_error().starts_with("parse error"));
}

#[test]
fn oversized_limits_are_rejected_before_running() {
    let engine = engine_with_docs();
    let (code, _) = search(&engine, r#"MATCH(title, "common")"#, TPT_MAX_LIMIT + 1);
    assert_eq!(code, TPT_ERR_INVALID_INPUT);
    let (code, body) = search(&engine, r#"MATCH(title, "common")"#, TPT_MAX_LIMIT);
    assert_eq!(code, TPT_OK);
    assert_eq!(body.unwrap()["total_hits"], 3);
}

#[test]
fn null_pointers_are_argument_errors() {
    let engine = engine_with_docs();
    let mut out: *mut c_char = ptr::null_mut();
    assert_eq!(
        unsafe { tpt_search(engine.handle(), ptr::null(), ptr::null(), 0, &mut out) },
        TPT_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { tpt_create_index(ptr::null(), ptr::null(), ptr::null()) },
        TPT_ERR_INVALID_ARGUMENT
    );
    assert_eq!(last_error(), "engine is null");
}

#[test]
fn version_is_available() {
    let version = unsafe { CStr::from_ptr(tpt_version()) }.to_str().unwrap();
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn a_successful_call_clears_the_last_error() {
    let engine = engine_with_docs();
    let (code, _) = search(&engine, "MATCH(", 0);
    assert_eq!(code, TPT_ERR_INVALID_INPUT);
    assert!(!last_error().is_empty());
    let (code, _) = search(&engine, r#"MATCH(title, "rare")"#, 0);
    assert_eq!(code, TPT_OK);
    assert_eq!(last_error(), "");
}
