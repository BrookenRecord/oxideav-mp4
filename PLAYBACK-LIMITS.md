# Bounded finite playback reader

Prepared against published `oxideav-mp4 0.0.11`, upstream revision
`13d802e708edf5fa39b3abe4827e2995059b9cb9` (MIT, original LICENSE unchanged).
Published archive SHA-256:
`e7766473a510b620ddcd87ea772d2b8193c566d9069799bd362ea695bc9562bd`.
`demux::open_playback_with_limits(input, codecs, OpenLimits)` uses the existing
reader and typed metadata accessors over one finite immutable seekable input.
A caller can provide a virtual init plus one staged HLS object. It owns
input extent/box ceilings, actual mdat-span validation, payload/accessor result
leases, source-edge trim and timeline authority. This entry does not stream a
presentation or infer whole-source EOF. The caller reserves max_workspace_bytes
before opening. Arbitrary caller CodecResolver allocations are outside that cap.

The playback path skips HEIF item collections, hint metadata, optional seek/event
indexes and unrelated per-track option summaries before their handlers allocate.
It keeps every raw sample description (including color/HDR children), original
track/transform fields, raw edits, clear groups and gapless evidence. Track groups
retain their existing options representation; fragment groups retain their typed
accessor. Protection is rejected during preflight, including protected entries,
sinf/tenc/senc/auxiliary information, seig groups and known PIFF protection UUIDs.
`open_typed` keeps the normal rich metadata path.

## Before-allocation gates

Preflight seeks the finite input and borrows parsed metadata slices. Only the
moov/moof body currently inspected is temporarily buffered in that pass. Before
that buffer is allocated, aggregate raw-body bytes must fit max_metadata_bytes,
and body bytes plus the 4096-byte read scratch must fit max_workspace_bytes.
The preflight then makes one fallible exact reservation for that proven extent,
resizes without further allocation, and reads directly into it. It never grows
that body geometrically; therefore old/new reallocations cannot overlap before
admission. The pass then rejects count/depth violations before any Track, stsd, sample,
ancillary table or expanded group records are allocated. Inactive descriptions
and defaults-only truns count. All top-level and nested declared ranges must fit
the supplied extent. The input must be immutable across preflight and parse.

The recursive walker visits the same admitted playback structures as the reader.
It counts boxes, explicit table/group/edit records, CSGP patterns and indices;
CSGP repeat counts are bounded too. The sample ceiling sums stsz/stz2 and trun
counts across tracks/fragments. Description-family checks use the handler before
sample-entry leaf parsing. The depth ceiling (1..32) is checked before recursion.
Raw metadata bodies are bounded even when they contain harmless skipped boxes.

## Peak workspace derivation

All arithmetic in this estimate is checked. Units are bytes of requested storage;
allocator bookkeeping, error diagnostics, input storage, arbitrary resolver work,
and allocations from later caller accessor/payload requests are excluded.
The estimate deliberately sums owners whose maxima may occur at different times.

Let M be aggregate buffered moov/moof body bytes, D actual nesting depth, S total
samples, T tracks, E visited boxes plus declared table/group/edit/index records.
`Stats::workspace` adds the following terms before the normal allocating parse:

| Term | Owners covered |
| --- | --- |
| 4096 | Fixed byte-reader scratch buffer |
| M * (D + 27) | The raw/copy/string owners detailed below |
| S * sample_width | Exactly reserved SampleRef result and maximum TrunSample scratch; resident DTS pair, CTS and three chunk/description arrays |
| T * track_width | Track vector (including its minimum four-element capacity), streams, PCM swap, DTS, IV/default vectors, IDs/timescales, gapless, headers, public entry/edit vector headers and timestamps |
| E * entry_width | All admitted ancillary/group/edit/metadata record families, sync HashSet capacity and bounded rendered group/summary text |

`sample_width`, `track_width` and `entry_width` use size_of for the actual existing
record types at the allocation sites; see the short expression in playback.rs.
The aggregate ancillary width sums each admitted family, then charges that sum
four times per E. Four covers the first Vec growth allocation of four records as
well as later geometric capacity below twice the live record count. The fixed
1024 bytes per track covers short codec IDs, the four gapless options and fixed
movie/header numeric summaries. CodecOptions in core 0.1.43 is a Vec of string
pairs, so it has no hidden tree/node allocation. E's extra 256 bytes covers the
ASCII rendering and temporary decimal/index-list strings: each u32 needs at most
10 decimal digits, u64 at most 20, separators are fixed, and each rendered
record/field is charged through a counted box/table/index record. The Vec/String
record slots themselves are included separately through size_of.

The D + 27 raw-byte coefficient is a sum of copy ownership bounds:

- One M for retained top-level moov/moof bodies.
- At most D * M for simultaneous ancestor/leaf body copies, plus one M for the
  old and replacement allocation during a read-buffer growth. The byte reader
  requests capacity no greater than its declared n and grows only after input
  bytes arrive; it cannot preallocate an unbacked huge declaration.
- Two M for parsed raw stsd payloads and their final public copy.
- Three M for decoder config, its replacement/temporary esds DSI, and the stream
  parameter copy. Contained sibling config/descriptor bytes share the same M.
- One M for parsed color ICC/unknown payload.
- Twelve M for lossy metadata strings and temporary/freeform key copies. UTF-8
  replacement expands at most threefold; geometric string capacity is below
  twice that length. Freeform mean/name strings and the formatted key can coexist
  at up to 12 times their source bytes; value Cow/copy overlap is at most nine.
- One M for an unescaped video header used to derive existing parameter facts.
- Six M for raw-group hexadecimal text during string growth: final hex is at
  most two characters per source byte, its capacity below fourfold, and old/new
  allocations can overlap below sixfold. Group record text/count slots use E.

Those contributions sum to D + 27. Actual body bytes are disjoint by top-level
ownership; depth bounds repeated ancestor copies. Small OpusHead prefixes,
formatting temporaries and initial String capacity are covered by the counted
track/entry allowances. Both raw and rendered group metadata remain charged while
the final reader is built. Preflight buffer ownership ends before that build.
The samples vector is exactly reserved from preflight; playback offset sorting is
in-place unstable, so it allocates no second index. No previous window is retained
by this reader; concurrent windows must be charged by the caller.

## Correctness changes and compatibility

Unsigned v0 trun/ctts composition offsets widen to i64 before arithmetic. tfdt-v1
beyond i64 is rejected. Fragment/sample/edit/duration offsets and timestamps use
checked arithmetic; unrepresentable edits return an error instead of saturation.
Gapless rate conversion uses u128 intermediates and checks u64 results/totals.
Unsigned seek skips use checked absolute offsets instead of narrowing to i64.

SGPD-v2 has default_length at body offset 8, default_sample_description_index at
12 and entry_count at 16. Both shared parser and paired mux helper now use that
layout. The old shared v2 fixture omitted default_length; it is corrected. The
old mux integration expectation collapsed v2 entries into a single blob; it now
checks their signalled boundaries. An authored canonical byte array tests reader
and writer independently, so a matching roundtrip bug cannot hide this change.

Only focused Rust parser checks are claimed. Software codec decoding, native
hardware playback and complete production integration are outside this patch.

## Independent-review regressions

The first candidate's preflight used the shared geometric reader after checking
only final body capacity. A relocating allocator could exceed the cap before the
later full-workspace rejection. The revised preflight owns one exact admitted
body allocation. Boundary tests cover lengths 8191/8192/8193, 16383/16384/16385 and
the reviewer's 1 MiB fixture; a ReadSeek observer verifies direct whole-body reads
rather than the geometric reader's chunk/growth path. Requested bytes are covered;
allocator bookkeeping remains outside the contract.

STSC preflight now checks exact length, positive/strictly increasing first_chunk,
positive samples_per_chunk and a valid description index for that track. Every
admitted expansion loop therefore either exits or advances a sample, including
large first_chunk gaps; zero-progress runs cannot spin to u32::MAX. The borrowed
track scan obtains the complete stsd description count without record allocation.

Bounded SBGP/SGPD preflight validates complete versioned entry ranges before the
legacy fragment walker. Fixed and variable v1/v2 entries and fixed-size v0
roll/prol evidence cannot be silently dropped on parse errors. Ordinary tolerant
fragment behavior is preserved through the existing open_typed entry.


## Accessor admission and typed refusals

`sample_count(stream_index)` returns the exact count for `samples(stream_index)`
without allocation, I/O, sorting or mutation. Its linear scan visits all samples
in this finite input. Unknown streams return zero. The allocating accessor first
filters references by stream, so its temporary reference vector, stable-sort
scratch and result scale with the selected count, independently of other tracks.
The reader's existing all-track sample index remains covered by opening workspace.

Configured playback ceiling refusals and fallible allocation failures use the
existing core `Error::ResourceExhausted` variant. Malformed structures and checked
wire arithmetic remain `InvalidData`; unsupported format features remain
`Unsupported`. Invalid caller depth limits remain `InvalidData`. Callers can map
these variants without inspecting diagnostic text. The workspace proof is unchanged.
