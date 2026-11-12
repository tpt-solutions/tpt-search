# tpt-search-py

Python bindings for `tpt-search`, built as a PyO3 extension module named `tpt_search` and
packaged with maturin.

> **Status:** Implemented and tested. Not yet published (`publish = false`).

## Features

- `Engine`: `create_index`, `drop_index`, `indexes`, `upsert`, `get`, `delete`, `search`
- Schemas, documents and results are plain dicts and lists, in the HTTP API's shapes
- Typed exceptions under `TptError`: `NotFoundError`, `InvalidInputError`, `ConflictError`,
  `QueryTimeoutError`
- `search` releases the GIL while the query runs

```python
from tpt_search import Engine

engine = Engine()
engine.create_index("docs", {
    "name": "docs",
    "primary_key": "id",
    "fields": {
        "id": {"name": "id", "field_type": "Text", "indexed": False, "stored": True,
               "analyzer": None, "vector_config": None},
        "title": {"name": "title", "field_type": "Text", "indexed": True, "stored": True,
                  "analyzer": None, "vector_config": None},
    },
})
engine.upsert("docs", "d1", {"title": "common rare marker"})
hits = engine.search("docs", 'MATCH(title, "rare")')["hits"]
```

Build and test:

```sh
maturin build --release                    # wheel in the workspace's target/wheels/
pip install ../../target/wheels/tpt_search-*.whl
python -m unittest discover -s tests -v    # run from this directory
```

## License

Dual-licensed under either of

- [MIT license](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

at your option.
