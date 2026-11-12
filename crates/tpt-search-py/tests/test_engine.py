"""Tests for the tpt_search Python extension.

Install the extension first (for example `maturin develop` or `pip install` a built wheel),
then run `python -m pytest tests` or `python -m unittest discover -s tests`.
"""

import unittest

import tpt_search
from tpt_search import (
    ConflictError,
    Engine,
    InvalidInputError,
    NotFoundError,
    TptError,
)

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


def engine_with_docs() -> Engine:
    engine = Engine()
    engine.create_index("docs", SCHEMA)
    engine.upsert("docs", "d1", {"title": "common rare marker"})
    engine.upsert("docs", "d2", {"title": "common words here"})
    engine.upsert("docs", "d3", {"title": "common words elsewhere"})
    return engine


class EngineTests(unittest.TestCase):
    def setUp(self):
        self.engine = engine_with_docs()

    def test_version_is_exposed(self):
        self.assertRegex(tpt_search.__version__, r"^\d+\.\d+\.\d+")

    def test_search_returns_api_shaped_dict(self):
        body = self.engine.search("docs", 'MATCH(title, "rare")')
        self.assertIsInstance(body, dict)
        self.assertEqual(body["total_hits"], 1)
        hit = body["hits"][0]
        self.assertEqual(hit["id"], "d1")
        self.assertEqual(hit["document"]["title"], "common rare marker")
        self.assertIsInstance(hit["highlights"], list)

    def test_highlight_spans_are_byte_ranges(self):
        body = self.engine.search("docs", 'MATCH(title, "rare")')
        spans = body["hits"][0]["highlights"][0]
        self.assertEqual(spans["field"], "title")
        start, end = spans["spans"][0]
        self.assertEqual("common rare marker"[start:end], "rare")

    def test_limit_caps_the_hits_but_not_total_hits(self):
        body = self.engine.search("docs", 'MATCH(title, "common")', limit=2)
        self.assertEqual(len(body["hits"]), 2)
        self.assertEqual(body["total_hits"], 3)

    def test_get_returns_a_dict_or_none(self):
        doc = self.engine.get("docs", "d2")
        self.assertEqual(doc["fields"]["title"], "common words here")
        self.assertIsNone(self.engine.get("docs", "nope"))

    def test_delete_reports_presence(self):
        self.assertTrue(self.engine.delete("docs", "d1"))
        self.assertFalse(self.engine.delete("docs", "d1"))
        self.assertIsNone(self.engine.get("docs", "d1"))

    def test_upsert_replaces_a_document(self):
        self.engine.upsert("docs", "d1", {"title": "replaced text"})
        self.assertEqual(self.engine.get("docs", "d1")["fields"]["title"], "replaced text")
        self.assertEqual(self.engine.search("docs", 'MATCH(title, "rare")')["total_hits"], 0)

    def test_indexes_lists_names_sorted(self):
        self.engine.create_index("alpha", {**SCHEMA, "name": "alpha"})
        self.assertEqual(self.engine.indexes(), ["alpha", "docs"])

    def test_drop_index_removes_it(self):
        self.engine.drop_index("docs")
        self.assertEqual(self.engine.indexes(), [])
        with self.assertRaises(NotFoundError):
            self.engine.drop_index("docs")


class ErrorTests(unittest.TestCase):
    def setUp(self):
        self.engine = engine_with_docs()

    def test_duplicate_index_is_a_conflict(self):
        with self.assertRaises(ConflictError) as ctx:
            self.engine.create_index("docs", SCHEMA)
        self.assertIn("already exists", str(ctx.exception))

    def test_errors_share_a_base_class(self):
        with self.assertRaises(TptError):
            self.engine.get("missing", "d1")
        self.assertTrue(issubclass(ConflictError, TptError))
        self.assertTrue(issubclass(InvalidInputError, TptError))

    def test_unknown_index_is_not_found(self):
        with self.assertRaises(NotFoundError):
            self.engine.search("nope", 'MATCH(title, "x")')
        with self.assertRaises(NotFoundError):
            self.engine.upsert("nope", "d", {"title": "x"})

    def test_bad_query_is_invalid_input(self):
        with self.assertRaises(InvalidInputError) as ctx:
            self.engine.search("docs", "MATCH(")
        self.assertIn("parse error", str(ctx.exception))

    def test_bad_schema_is_invalid_input(self):
        with self.assertRaises(InvalidInputError):
            Engine().create_index("x", {"name": "x"})

    def test_unknown_field_in_document_is_invalid_input(self):
        with self.assertRaises(InvalidInputError) as ctx:
            self.engine.upsert("docs", "d9", {"missing": "x"})
        self.assertIn("unknown field `missing`", str(ctx.exception))

    def test_oversized_limit_is_invalid_input(self):
        with self.assertRaises(InvalidInputError):
            self.engine.search("docs", 'MATCH(title, "common")', limit=1001)

    def test_non_json_document_is_rejected(self):
        with self.assertRaises(TypeError):
            self.engine.upsert("docs", "d9", {"title": object()})


if __name__ == "__main__":
    unittest.main(verbosity=2)
