# Release download counts at the 2026-08-07 prune

GitHub destroys a release's download counters when the release is deleted, and there is
no API that returns them afterwards. This is the record, captured immediately before
`v0.1.0`–`v0.8.0` were deleted. Binary counts only — each release also carried a
`.sha256` per platform, whose counts tracked the binaries within one or two.

| Release | macOS | Linux musl | Windows | Total | Kept? |
|---|--:|--:|--:|--:|---|
| v0.9.1 | 0 | 2 | 0 | 2 | kept |
| v0.9.0 | 8 | 8 | 7 | 23 | kept |
| v0.8.0 | 5 | 5 | 2 | 12 | deleted |
| v0.7.3 | 2 | 1 | 2 | 5 | deleted |
| v0.7.2 | 2 | 1 | 1 | 4 | deleted |
| v0.7.1 | 6 | 3 | 3 | 12 | deleted |
| v0.7.0 | 12 | 10 | 0 | 22 | deleted |
| v0.6.0 | 1 | 0 | 0 | 1 | deleted |
| v0.5.2 | 0 | 0 | 0 | 0 | deleted |
| v0.5.1 | 0 | 0 | 0 | 0 | deleted |
| v0.5.0 | 0 | 0 | 0 | 0 | deleted |
| v0.1.1 | 1 | 1 | 1 | 3 | deleted |
| v0.1.0 | 0 | 0 | 0 | 0 | deleted |

Totals at the prune: **84 binary downloads** across all thirteen releases — 59 on the
eleven deleted, 25 on the two kept. By platform: macOS 37, Linux musl 31, Windows 16.
v0.5.0's Windows `0` is not a download count: that release never carried a Windows asset at
all, which is the defect the 2026-08-02 review recorded against it. v0.7.0's Windows `0` is a
genuine zero — the asset existed and nobody took it.
