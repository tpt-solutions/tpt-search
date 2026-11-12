# tpt-search (Go)

Go binding for tpt-search. It calls the C ABI in `crates/tpt-search-capi` through cgo.

```go
engine := tpt.NewEngine()
defer engine.Close()

_ = engine.CreateIndex("docs", schema) // schema uses the HTTP API's JSON shape
_ = engine.Upsert("docs", "d1", map[string]any{"title": "common rare marker"})

result, err := engine.Search("docs", `MATCH(title, "rare")`, 10)
if errors.Is(err, tpt.ErrNotFound) { /* ... */ }
```

Errors wrap sentinel values (`ErrNotFound`, `ErrConflict`, `ErrInvalidInput`, `ErrTimeout`,
`ErrClosed`, ...). Use `errors.Is`; `errors.As` with `*tpt.Error` gives the message.

## Building

The binding needs the native library and a C compiler for cgo.

1. Build the library:

   ```sh
   cargo build -p tpt-search-capi            # Linux/macOS: target/debug/libtpt_search_capi.*
   ```

2. Point cgo at it and run the tests:

   ```sh
   CGO_LDFLAGS="-L/path/to/target/debug" go test ./...
   ```

   At run time the shared library must be found: `LD_LIBRARY_PATH` on Linux,
   `DYLD_LIBRARY_PATH` on macOS, or the directory on `PATH` on Windows.

### Windows (GNU toolchain)

cgo on Windows needs MinGW's gcc, and the Rust library must target `x86_64-pc-windows-gnu`
so its import library matches:

```sh
rustup target add x86_64-pc-windows-gnu
cargo build -p tpt-search-capi --target x86_64-pc-windows-gnu
```

With MSYS2 installed, the tests run as:

```powershell
$env:PATH = "<repo>\target\x86_64-pc-windows-gnu\debug;C:\msys64\mingw64\bin;$env:PATH"
$env:CGO_ENABLED = "1"; $env:CC = "gcc"
$env:CGO_LDFLAGS = "-L<repo>/target/x86_64-pc-windows-gnu/debug"
go test -race ./...
```

Two Windows details matter. cgo rejects `-L` paths that contain spaces, so use the 8.3
short path (`(New-Object -ComObject Scripting.FileSystemObject).GetFolder(...).ShortPath`)
when the repository lives under a directory with spaces. And the test binary loads
`tpt_search_capi.dll` from `PATH`, so the library directory must be on `PATH` before `go test`
starts it.

The C header is `crates/tpt-search-capi/include/tpt_search.h`. It is included through cgo's
`CFLAGS` in `tpt.go`, so no header needs copying.
