# AutoNAT Allocation Sites

## Result

Two isolated debugger captures identified seven 52-byte allocations each.
All seven had matching deallocations before normal process exit.
This identifies isolated owners, not the full daemon's retained reconnect steps.

| Allocation IDs | Owner | Count per capture |
| --- | --- | ---: |
| 1, 5 | Listen-address hash sets | 2 |
| 2, 3 | Inner and outer peer-address LRU tables | 2 |
| 4 | Dial address-deduplication set | 1 |
| 6 | Pending outbound request-ID set | 1 |
| 7 | Pending inbound request-ID set | 1 |

Both captures completed ten requests, ten outbound refusals and ten inbound
refusals. Each test finished in 10.41 seconds. Release order was
`4, 6, 7, 2, 3, 5, 1`; both lifetime summaries were `7 7 []`.

## Scope And Bounds

| Property | Setting |
| --- | --- |
| Source | `3e3d0c4e`; existing allocation-review test binary |
| Environment | Fresh user/network namespace; loopback only |
| Debugger | Cached GDB 17.2; x86_64 Linux register ABI |
| Workload | Existing AutoNAT refusal ownership test; original timer prewarm |
| Deadline | 15 seconds, then TERM with 2-second kill grace |
| Output | 128 KiB file limit; 24 frames/site; names truncated to 240 characters |
| Storage before captures | 8,258,012 KiB across `/tmp/p2p-vpn-*` |
| Builds, downloads, deployments | None |

The debugger launched only its owned test process. It did not attach to any
service or physical device. Debuginfod and auto-loading were disabled.
Captured frames contain function names, not arguments or packet contents.

## Reproduction

The [capture manifest](autonat-allocation-sites.json) contains exact tool/artifact
hashes, raw log hashes and the complete shell command, including embedded GDB
Python. Its absolute paths refer to the cached review environment.

1. Verify the recorded binary and debugger hashes; check the temporary-storage cap.
2. Run the manifest's `command`, redirecting output to a new owned log file.
3. Require normal inferior exit, a passing test and no Python/debugger errors.
4. Require seven allocations, seven matching frees of size 52 and an empty live-ID list.
5. Confirm the three event counts are ten; retain the raw log even on failure.

The breakpoint is enabled only after entering the test function.
An explicit `$rdi == 52` check inside the Python callback selects allocations.
A finish breakpoint records each returned pointer internally; deallocation
matches that pointer to an opaque serial number without logging its address.

## Preliminary Attempts

| Capture | Outcome |
| --- | --- |
| Sites 1 | `malloc` unresolved; test passed but no allocation evidence |
| Sites 2 | Intended breakpoint condition did not filter Python callbacks; invalid size evidence; quota stopped the inferior |
| Sites 3 | Explicit register filter worked; seven sites; test passed without lifetime tracking |
| Lifetime 1 and 2 | Explicit filter plus matched deallocation; both passed |

All raw logs remain local and are hashed in the manifest.
The first two attempts are not counted as successful attribution.

## Limits And Next Step

- This is an exploratory diagnostic, not a general heap profiler or timing benchmark.
- It tracks only 52-byte `__rust_alloc` calls after test entry, not reallocations or zeroed allocations.
- The exploratory quota raises a Python error; timeout and file-size limits provide the hard bounds.
- Both valid captures stayed below that quota and showed no debugger errors.
- No production source, allocator, dependency or security setting changed.

The pending-request sets are now concrete candidates for the daemon's 52-byte
steps. Matching sizes alone cannot establish that ownership. Trace the full
daemon across connection retirement before classifying its retained growth.

The separate [global timer investigation](global-timer-allocation-review.md)
already attributes the isolated 224-byte teardown differential. Neither result
closes full-daemon reconnect or packet-pressure allocation acceptance.
