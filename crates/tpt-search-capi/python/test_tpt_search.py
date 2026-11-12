"""Checks the C ABI from Python with ctypes, against the built tpt-search-capi library.

Build the library first (`cargo build -p tpt-search-capi`), then run:

    python test_tpt_search.py

Set TPT_SEARCH_LIB to point at a library elsewhere. The script needs only the standard
library.
"""

import ctypes
import json
import os
import pathlib
import sys
import unittest

HERE = pathlib.Path(__file__).resolve().parent
TARGET = HERE.parent.parent.parent / "target" / "debug"


def library_path() -> pathlib.Path:
    override = os.environ.get("TPT_SEARCH_LIB")
    if override:
        return pathlib.Path(override)
    names = {
        "win32": "tpt_search_capi.dll",
        "darwin": "libtpt_search_capi.dylib",
    }
    return TARGET / names.get(sys.platform, "libtpt_search_capi.so")


OK, ERR_INVALID_ARGUMENT, ERR_NOT_FOUND, ERR_INVALID_INPUT, ERR_CONFLICT = 0, 1, 2, 3, 4

SCHEMA = {
    "name": "docs",
    "primary_key": "id",
    "fields": {
        "id": {"name": "id", "field_type": "Text", "indexed": False, "stored": True,
               "analyzer": None, "vector_config": None},
        "title": {"name": "title", "field_type": "Text", "indexed": True, "stored": True,
                  "analyzer": None, "vector_config": None},
    },
}


class Tpt:
    """Declares the argument and return types of each exported function."""

    def __init__(self, path: pathlib.Path):
        self.lib = ctypes.CDLL(str(path))
        c_char_p = ctypes.c_char_p
        pp = ctypes.POINTER(ctypes.c_char_p)
        lib = self.lib

        lib.tpt_engine_new.restype = ctypes.c_void_p
        lib.tpt_engine_new.argtypes = []
        lib.tpt_engine_free.restype = None
        lib.tpt_engine_free.argtypes = [ctypes.c_void_p]
        lib.tpt_create_index.restype = ctypes.c_int
        lib.tpt_create_index.argtypes = [ctypes.c_void_p, c_char_p, c_char_p]
        lib.tpt_drop_index.restype = ctypes.c_int
        lib.tpt_drop_index.argtypes = [ctypes.c_void_p, c_char_p]
        lib.tpt_upsert.restype = ctypes.c_int
        lib.tpt_upsert.argtypes = [ctypes.c_void_p, c_char_p, c_char_p, c_char_p]
        lib.tpt_delete.restype = ctypes.c_int
        lib.tpt_delete.argtypes = [ctypes.c_void_p, c_char_p, c_char_p,
                                   ctypes.POINTER(ctypes.c_bool)]
        lib.tpt_get.restype = ctypes.c_int
        lib.tpt_get.argtypes = [ctypes.c_void_p, c_char_p, c_char_p, pp]
        lib.tpt_search.restype = ctypes.c_int
        lib.tpt_search.argtypes = [ctypes.c_void_p, c_char_p, c_char_p, ctypes.c_size_t, pp]
        lib.tpt_string_free.restype = None
        lib.tpt_string_free.argtypes = [c_char_p]
        lib.tpt_last_error.restype = c_char_p
        lib.tpt_last_error.argtypes = []
        lib.tpt_version.restype = c_char_p
        lib.tpt_version.argtypes = []

    def take(self, out: ctypes.c_char_p) -> str:
        """Copies an owned string out of the library and frees it."""
        text = out.value.decode("utf-8")
        self.lib.tpt_string_free(out)
        return text

    def last_error(self) -> str:
        return self.lib.tpt_last_error().decode("utf-8")


def b(text: str) -> bytes:
    return text.encode("utf-8")


class CApiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tpt = Tpt(library_path())

    def setUp(self):
        self.engine = self.tpt.lib.tpt_engine_new()
        self.assertTrue(self.engine)
        self.assertEqual(
            self.tpt.lib.tpt_create_index(self.engine, b"docs", b(json.dumps(SCHEMA))),
            OK,
            self.tpt.last_error(),
        )
        for doc_id, title in [
            ("d1", "common rare marker"),
            ("d2", "common words here"),
            ("d3", "common words elsewhere"),
        ]:
            code = self.tpt.lib.tpt_upsert(
                self.engine, b"docs", b(doc_id), b(json.dumps({"title": title}))
            )
            self.assertEqual(code, OK, self.tpt.last_error())

    def tearDown(self):
        self.tpt.lib.tpt_engine_free(self.engine)

    def search(self, query: str, limit: int = 0):
        out = ctypes.c_char_p()
        code = self.tpt.lib.tpt_search(
            self.engine, b"docs", b(query), limit, ctypes.byref(out)
        )
        if code != OK:
            return code, None
        return code, json.loads(self.tpt.take(out))

    def test_version_is_reported(self):
        version = self.tpt.lib.tpt_version().decode("utf-8")
        self.assertRegex(version, r"^\d+\.\d+\.\d+")

    def test_search_returns_api_shaped_hits(self):
        code, body = self.search('MATCH(title, "rare")')
        self.assertEqual(code, OK, self.tpt.last_error())
        self.assertEqual(body["total_hits"], 1)
        hit = body["hits"][0]
        self.assertEqual(hit["id"], "d1")
        self.assertEqual(hit["document"]["title"], "common rare marker")
        self.assertIsInstance(hit["highlights"], list)

    def test_get_returns_the_stored_document(self):
        out = ctypes.c_char_p()
        code = self.tpt.lib.tpt_get(self.engine, b"docs", b"d2", ctypes.byref(out))
        self.assertEqual(code, OK, self.tpt.last_error())
        self.assertEqual(
            json.loads(self.tpt.take(out))["fields"]["title"], "common words here"
        )

    def test_delete_reports_presence(self):
        present = ctypes.c_bool(False)
        code = self.tpt.lib.tpt_delete(
            self.engine, b"docs", b"d1", ctypes.byref(present)
        )
        self.assertEqual(code, OK)
        self.assertTrue(present.value)

        missing = ctypes.c_bool(True)
        code = self.tpt.lib.tpt_delete(
            self.engine, b"docs", b"d1", ctypes.byref(missing)
        )
        self.assertEqual(code, OK)
        self.assertFalse(missing.value)

    def test_missing_document_is_not_found(self):
        out = ctypes.c_char_p()
        code = self.tpt.lib.tpt_get(self.engine, b"docs", b"nope", ctypes.byref(out))
        self.assertEqual(code, ERR_NOT_FOUND)
        self.assertEqual(self.tpt.last_error(), "no document `nope`")

    def test_duplicate_index_is_a_conflict(self):
        code = self.tpt.lib.tpt_create_index(self.engine, b"docs", b(json.dumps(SCHEMA)))
        self.assertEqual(code, ERR_CONFLICT)

    def test_bad_query_is_rejected_with_a_message(self):
        code, _ = self.search("MATCH(")
        self.assertEqual(code, ERR_INVALID_INPUT)
        self.assertTrue(self.tpt.last_error().startswith("parse error"))

    def test_bad_document_is_rejected(self):
        code = self.tpt.lib.tpt_upsert(
            self.engine, b"docs", b"d9", b(json.dumps({"missing": "x"}))
        )
        self.assertEqual(code, ERR_INVALID_INPUT)
        self.assertIn("unknown field `missing`", self.tpt.last_error())

    def test_null_engine_is_an_argument_error(self):
        code = self.tpt.lib.tpt_create_index(None, b"x", b(json.dumps(SCHEMA)))
        self.assertEqual(code, ERR_INVALID_ARGUMENT)
        self.assertEqual(self.tpt.last_error(), "engine is null")

    def test_oversized_limit_is_rejected(self):
        code, _ = self.search('MATCH(title, "common")', limit=1001)
        self.assertEqual(code, ERR_INVALID_INPUT)

    def test_successful_call_clears_the_error(self):
        self.search("MATCH(")
        self.assertNotEqual(self.tpt.last_error(), "")
        code, _ = self.search('MATCH(title, "rare")')
        self.assertEqual(code, OK)
        self.assertEqual(self.tpt.last_error(), "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
