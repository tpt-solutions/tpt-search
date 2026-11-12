# Changelog

All notable changes to this crate are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Checksummed, fsynced write-ahead log.
- Segment files with memory-mapped reads and a segment manager with merge policies.
- Snapshot and restore with checksum manifests.
