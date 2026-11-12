//! Authored byte fixtures exercising the existing public reader, not a mux roundtrip.
use super::*;
use oxideav_core::{Demuxer, NullCodecResolver};
use std::io::Cursor;

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|word| word.to_be_bytes()).collect()
}

fn joined(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn limits() -> OpenLimits {
    OpenLimits {
        max_samples: 32,
        max_tracks: 4,
        max_sample_descriptions: 8,
        max_table_entries: 256,
        max_metadata_bytes: 16384,
        max_workspace_bytes: 1024 * 1024,
        max_depth: 16,
    }
}

fn description(config: u8, extra: &[u8]) -> Vec<u8> {
    let mut visual = vec![0; 78];
    visual[6..8].copy_from_slice(&1u16.to_be_bytes());
    visual[24..26].copy_from_slice(&1920u16.to_be_bytes());
    visual[26..28].copy_from_slice(&1080u16.to_be_bytes());
    visual.extend(boxed(b"avcC", &[1, config, 0, 31, 255]));
    visual.extend_from_slice(extra);
    boxed(b"avc1", &visual)
}

fn track(id: u32, descriptions: &[Vec<u8>], stbl_extra: &[u8], track_extra: &[u8]) -> Vec<u8> {
    let mut header = vec![0; 84];
    header[12..16].copy_from_slice(&id.to_be_bytes());
    header[40..44].copy_from_slice(&0x00010000u32.to_be_bytes());
    header[56..60].copy_from_slice(&0x00010000u32.to_be_bytes());
    header[72..76].copy_from_slice(&0x40000000u32.to_be_bytes());
    header[76..80].copy_from_slice(&(1920u32 << 16).to_be_bytes());
    header[80..84].copy_from_slice(&(1080u32 << 16).to_be_bytes());
    let mut media = vec![0; 24];
    media[12..16].copy_from_slice(&90_000u32.to_be_bytes());
    let mut handler = vec![0; 12];
    handler[8..12].copy_from_slice(b"vide");
    let mut stsd = words(&[0, descriptions.len() as u32]);
    stsd.extend(descriptions.concat());
    let mut stbl = boxed(b"stsd", &stsd);
    stbl.extend(boxed(b"stts", &words(&[0, 0])));
    stbl.extend(boxed(b"stsz", &words(&[0, 0, 0])));
    stbl.extend_from_slice(stbl_extra);
    let mut body = joined(&[
        boxed(b"tkhd", &header),
        boxed(
            b"mdia",
            &joined(&[
                boxed(b"mdhd", &media),
                boxed(b"hdlr", &handler),
                boxed(b"minf", &boxed(b"stbl", &stbl)),
            ]),
        ),
    ]);
    body.extend_from_slice(track_extra);
    boxed(b"trak", &body)
}

fn init(tracks: &[Vec<u8>], extra: &[u8]) -> Vec<u8> {
    let mut movie = vec![0; 100];
    movie[12..16].copy_from_slice(&1000u32.to_be_bytes());
    let defaults: Vec<u8> = (1..=tracks.len())
        .flat_map(|id| boxed(b"trex", &words(&[0, id as u32, 1, 3000, 4, 0])))
        .collect();
    let mut body = boxed(b"mvhd", &movie);
    body.extend(tracks.concat());
    body.extend(boxed(b"mvex", &defaults));
    body.extend_from_slice(extra);
    joined(&[boxed(b"ftyp", b"iso6\0\0\0\0iso6"), boxed(b"moov", &body)])
}

fn basic_init() -> Vec<u8> {
    init(&[track(1, &[description(100, &[])], &[], &[])], &[])
}

fn fragment_with(trafs: &[Vec<u8>], extra: &[u8]) -> Vec<u8> {
    let mut body = boxed(b"mfhd", &words(&[0, 1]));
    body.extend(trafs.concat());
    body.extend_from_slice(extra);
    boxed(b"moof", &body)
}

fn traf(
    id: u32,
    tfhd_flags: u32,
    sdi: Option<u32>,
    dts: u64,
    version: u8,
    cts: &[u32],
    extra: &[u8],
) -> Vec<u8> {
    let mut tfhd = words(&[tfhd_flags | if sdi.is_some() { 2 } else { 0 }, id]);
    if let Some(sdi) = sdi {
        tfhd.extend(sdi.to_be_bytes());
    }
    let mut tfdt = words(&[0x01000000]);
    tfdt.extend(dts.to_be_bytes());
    let mut run = words(&[u32::from(version) << 24 | 0x801, cts.len() as u32, 4096]);
    run.extend(words(cts));
    let mut body = joined(&[
        boxed(b"tfhd", &tfhd),
        boxed(b"tfdt", &tfdt),
        boxed(b"trun", &run),
    ]);
    body.extend_from_slice(extra);
    boxed(b"traf", &body)
}

fn basic_fragment(dts: u64, version: u8, cts: &[u32]) -> Vec<u8> {
    fragment_with(&[traf(1, 0x020000, None, dts, version, cts, &[])], &[])
}

fn open(bytes: Vec<u8>, limits: OpenLimits) -> Result<Mp4Demuxer> {
    open_playback_with_limits(Box::new(Cursor::new(bytes)), &NullCodecResolver, limits)
}

fn window(dts: u64, version: u8, cts: &[u32]) -> Vec<u8> {
    let mut moof = basic_fragment(dts, version, cts);
    let at = moof.windows(4).position(|bytes| bytes == b"trun").unwrap() + 12;
    let offset = moof.len() as u32 + 8;
    moof[at..at + 4].copy_from_slice(&offset.to_be_bytes());
    joined(&[basic_init(), moof, boxed(b"mdat", &vec![7; cts.len() * 4])])
}

#[test]
fn playback_limits_preserve_raw_timing_and_normal_reader_semantics() {
    let bytes = window(7, 1, &[(-20i32) as u32, 2]);
    let bounded = open(bytes.clone(), limits()).unwrap();
    let ordinary = open_typed(Box::new(Cursor::new(bytes)), &NullCodecResolver).unwrap();
    assert_eq!(bounded.samples(0), ordinary.samples(0));
    assert_eq!(bounded.samples(0)[0].composition_time, -13);
    assert_eq!(bounded.samples(0)[1].decode_time, 3007);
    assert_eq!(bounded.track_header(0), ordinary.track_header(0));
    assert_eq!(bounded.sample_entry(0, 1), ordinary.sample_entry(0, 1));
}

#[test]
fn playback_limits_reject_each_aggregate_ceiling() {
    let bytes = window(0, 0, &[0, 0]);
    for selected in [
        OpenLimits {
            max_samples: 1,
            ..limits()
        },
        OpenLimits {
            max_tracks: 0,
            ..limits()
        },
        OpenLimits {
            max_sample_descriptions: 0,
            ..limits()
        },
        OpenLimits {
            max_table_entries: 1,
            ..limits()
        },
        OpenLimits {
            max_metadata_bytes: 1,
            ..limits()
        },
        OpenLimits {
            max_workspace_bytes: 1,
            ..limits()
        },
        OpenLimits {
            max_depth: 1,
            ..limits()
        },
        OpenLimits {
            max_depth: 0,
            ..limits()
        },
    ] {
        assert!(open(bytes.clone(), selected).is_err());
    }
}

#[test]
fn playback_timestamps_reject_overflow_and_keep_unsigned_cts() {
    let parsed = open(window(7, 0, &[u32::MAX]), limits()).unwrap();
    assert_eq!(
        parsed.samples(0)[0].composition_time,
        7 + i64::from(u32::MAX)
    );
    for time in [u64::MAX, i64::MAX as u64, i64::MAX as u64 - 1] {
        assert!(open(window(time, 0, &[1]), limits()).is_err());
    }
}

#[test]
fn playback_canonical_sgpd_v2_has_standard_offsets_and_group_evidence() {
    // FullBox, grouping_type, default_length, default_index, count, one roll entry.
    let canonical = [
        2, 0, 0, 0, b'r', b'o', b'l', b'l', 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 1, 255, 254,
    ];
    let parsed = parse_sgpd(&canonical).unwrap();
    assert_eq!(parsed.default_sample_description_index, Some(3));
    assert_eq!(parsed.entries, [vec![255, 254]]);
    let built = crate::sample_groups::build_sgpd(&crate::sample_groups::SampleGroupDescription {
        grouping_type: *b"roll",
        default_sample_description_index: Some(3),
        entries: vec![vec![255, 254]],
    });
    assert_eq!(&built[8..], &canonical);
    let groups = boxed(b"sgpd", &canonical);
    let moof = fragment_with(&[traf(1, 0x020000, None, 0, 0, &[0], &groups)], &[]);
    let reader = open(joined(&[basic_init(), moof]), limits()).unwrap();
    assert_eq!(
        reader.traf_sample_groups()[0].sgpd[0].entries,
        [vec![255, 254]]
    );
    let mut forged = canonical;
    forged[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    let bad = init(
        &[track(
            1,
            &[description(100, &[])],
            &boxed(b"sgpd", &forged),
            &[],
        )],
        &[],
    );
    assert!(open(bad, limits()).is_err());
}

#[test]
fn playback_all_sample_descriptions_and_geometry_are_preserved() {
    let entries = [
        description(66, &[]),
        description(100, &boxed(b"mdcv", &[3; 24])),
    ];
    let bytes = init(&[track(1, &entries, &[], &[])], &[]);
    let reader = open(bytes, limits()).unwrap();
    assert_eq!(reader.sample_entry_count(0), 2);
    assert!(
        reader
            .sample_entry(0, 2)
            .unwrap()
            .payload
            .windows(4)
            .any(|part| part == b"mdcv")
    );
    assert_eq!(reader.track_header(0).unwrap().width, 1920 << 16);
}

#[test]
fn playback_protection_precedes_skipping_rich_metadata_and_nested_configs() {
    for kind in [b"pssh", b"senc", b"saiz", b"tenc", b"cmov"] {
        assert!(open(joined(&[basic_init(), boxed(kind, &[])]), limits()).is_err());
    }
    let bad = init(
        &[track(
            1,
            &[description(66, &[]), description(100, &boxed(b"sinf", &[]))],
            &[],
            &[],
        )],
        &[],
    );
    assert!(open(bad, limits()).is_err());
    let uuid = boxed(b"uuid", &PIFF_TENC_USERTYPE);
    assert!(
        open(
            init(
                &[track(
                    1,
                    &[description(100, &[])],
                    &[],
                    &boxed(b"udta", &uuid)
                )],
                &[]
            ),
            limits()
        )
        .is_err()
    );
    let seig = boxed(
        b"sgpd",
        &joined(&[words(&[0]), b"seig".to_vec(), words(&[0])]),
    );
    assert!(
        open(
            init(&[track(1, &[description(100, &[])], &seig, &[])], &[]),
            limits()
        )
        .is_err()
    );
}

#[test]
fn playback_skips_irrelevant_heif_hints_and_rich_options() {
    let bytes = joined(&[
        basic_init(),
        boxed(b"meco", &[255; 128]),
        boxed(b"pdin", &[255; 128]),
    ]);
    let reader = open(bytes, limits()).unwrap();
    assert!(reader.pdin_entries().is_none());
    assert!(reader.streams()[0].params.options.is_empty());
}

#[test]
fn playback_prefix_truncation_never_reaches_success() {
    let bytes = window(0, 0, &[0]);
    // A complete init alone is valid; test truncations within metadata boxes only.
    let init = basic_init();
    for end in 0..init.len() {
        assert!(
            open(init[..end].to_vec(), limits()).is_err(),
            "prefix {end}"
        );
    }
    assert!(open(bytes[..bytes.len() - 1].to_vec(), limits()).is_err());
}

#[test]
fn playback_defaults_only_and_ancillary_counts_fail_before_record_expansion() {
    let defaults = joined(&[
        boxed(b"tfhd", &words(&[0x020000, 1])),
        boxed(b"tfdt", &words(&[0, 0])),
        boxed(b"trun", &words(&[1, u32::MAX, 4096])),
    ]);
    let error = open(
        joined(&[
            basic_init(),
            fragment_with(&[boxed(b"traf", &defaults)], &[]),
        ]),
        limits(),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("sample limit"));
    for kind in [b"stts", b"stsc", b"stco", b"co64", b"stss", b"ctts"] {
        let table = boxed(kind, &words(&[0, u32::MAX]));
        let error = open(
            init(&[track(1, &[description(100, &[])], &table, &[])], &[]),
            limits(),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("table-entry limit"), "{kind:?}");
    }
    let sizes = boxed(b"stsz", &words(&[0, 1, u32::MAX]));
    assert!(
        open(
            init(&[track(1, &[description(100, &[])], &sizes, &[])], &[]),
            limits()
        )
        .is_err()
    );
    let mut twice = defaults;
    let end = twice.len();
    twice[end - 8..].copy_from_slice(&words(&[20, 4096]));
    twice.extend(boxed(b"trun", &words(&[0, 20])));
    assert!(
        open(
            joined(&[basic_init(), fragment_with(&[boxed(b"traf", &twice)], &[])]),
            limits()
        )
        .is_err()
    );
}

#[test]
fn playback_group_options_and_original_edit_units_remain_available() {
    let sgpd = boxed(
        b"sgpd",
        &[
            1, 0, 0, 0, b'r', b'o', b'l', b'l', 0, 0, 0, 2, 0, 0, 0, 1, 255, 254,
        ],
    );
    let sbgp = boxed(
        b"sbgp",
        &joined(&[words(&[0]), b"roll".to_vec(), words(&[1, 2, 1])]),
    );
    let csgp = boxed(
        b"csgp",
        &joined(&[words(&[0]), b"roll".to_vec(), words(&[1]), vec![0x12, 0x10]]),
    );
    let edit = boxed(
        b"edts",
        &boxed(b"elst", &words(&[0, 1, 500, 2112, 0x10000])),
    );
    let bytes = init(
        &[track(
            1,
            &[description(100, &[])],
            &joined(&[sgpd, sbgp, csgp]),
            &edit,
        )],
        &[],
    );
    let reader = open(bytes, limits()).unwrap();
    let options = &reader.streams()[0].params.options;
    assert_eq!(options.get("sgpd_0"), Some("roll fffe"));
    assert_eq!(options.get("sbgp_0"), Some("roll 2:1"));
    assert_eq!(options.get("csgp_0"), Some("roll 2*1"));
    assert_eq!(reader.edit_list(0)[0].segment_duration, 500);
    assert_eq!(reader.edit_list(0)[0].media_time, 2112);
    assert_eq!(reader.movie_timescale(), 1000);
}

#[test]
fn playback_rejects_handler_mismatch_nesting_and_forged_nested_lengths() {
    let mut wrong = track(1, &[description(100, &[])], &[], &[]);
    let pos = wrong.windows(4).position(|bytes| bytes == b"vide").unwrap();
    wrong[pos..pos + 4].copy_from_slice(b"soun");
    assert!(open(init(&[wrong], &[]), limits()).is_err());
    let forged = [u32::MAX.to_be_bytes().as_slice(), b"avcC"].concat();
    assert!(
        open(
            init(&[track(1, &[description(100, &forged)], &[], &[])], &[]),
            limits()
        )
        .is_err()
    );
    let mut nested = boxed(b"free", &[]);
    for _ in 0..20 {
        nested = boxed(b"wave", &nested);
    }
    assert!(
        open(
            init(&[track(1, &[description(100, &nested)], &[], &[])], &[]),
            limits()
        )
        .is_err()
    );
}

#[test]
fn playback_offset_and_edit_arithmetic_returns_errors() {
    for (base, delta) in [(u64::MAX, 1i32), (u64::MAX - 2, 0)] {
        let mut header = words(&[1, 1]);
        header.extend(base.to_be_bytes());
        let body = joined(&[
            boxed(b"tfhd", &header),
            boxed(b"tfdt", &words(&[0, 0])),
            boxed(b"trun", &words(&[1, 1, delta as u32])),
        ]);
        assert!(
            open(
                joined(&[basic_init(), fragment_with(&[boxed(b"traf", &body)], &[])]),
                limits()
            )
            .is_err()
        );
    }
    let mut edit = words(&[0x01000000, 1]);
    edit.extend(u64::MAX.to_be_bytes());
    edit.extend(0i64.to_be_bytes());
    edit.extend(0x10000u32.to_be_bytes());
    assert!(
        open(
            init(
                &[track(
                    1,
                    &[description(100, &[])],
                    &[],
                    &boxed(b"edts", &boxed(b"elst", &edit))
                )],
                &[]
            ),
            limits()
        )
        .is_err()
    );
    let mut cursor = Cursor::new(Vec::<u8>::new());
    skip_box_body(
        &mut cursor,
        &BoxHeader {
            fourcc: *b"free",
            total_size: Some((1u64 << 63) + 8),
            header_len: 8,
        },
    )
    .unwrap();
    assert_eq!(cursor.position(), 1u64 << 63);
}

#[test]
fn playback_gapless_arithmetic_rejects_unrepresentable_evidence() {
    assert!(scale_gapless(u64::MAX, 2, 1).is_err());
    let mut entry = vec![0; 28];
    entry[6..8].copy_from_slice(&1u16.to_be_bytes());
    entry[24..28].copy_from_slice(&(48000u32 << 16).to_be_bytes());
    let mut audio = track(1, &[boxed(b"mp4a", &entry)], &[], &[]);
    let at = audio.windows(4).position(|bytes| bytes == b"vide").unwrap();
    audio[at..at + 4].copy_from_slice(b"soun");
    let item = boxed(
        b"----",
        &joined(&[
            boxed(
                b"mean",
                &[words(&[0]), b"com.apple.iTunes".to_vec()].concat(),
            ),
            boxed(b"name", &[words(&[0]), b"iTunSMPB".to_vec()].concat()),
            boxed(
                b"data",
                &[
                    words(&[1, 0]),
                    b"00000000 FFFFFFFFFFFFFFFF 00000000 00000001".to_vec(),
                ]
                .concat(),
            ),
        ]),
    );
    let metadata = boxed(
        b"udta",
        &boxed(b"meta", &[words(&[0]), boxed(b"ilst", &item)].concat()),
    );
    let error = open(init(&[audio], &metadata), limits()).err().unwrap();
    assert!(error.to_string().contains("gapless total"));
}

#[test]
fn playback_workspace_arithmetic_and_peak_boundary_are_checked() {
    assert!(
        Stats {
            metadata: usize::MAX,
            depth: 1,
            ..Stats::default()
        }
        .workspace()
        .is_err()
    );
    let bytes = window(0, 0, &[0; 32]);
    let mut input = Cursor::new(bytes.clone());
    // Count the exact admitted records with the same allocation-free walker.
    let mut stats = Stats::default();
    let mut raw = bytes.as_slice();
    while let Some((kind, body)) = preflight::next(&mut raw).unwrap() {
        stats.entries(1, limits()).unwrap();
        if matches!(kind, MOOV | MOOF) {
            stats.metadata += body.len();
            preflight::walk(body, kind, 1, None, &mut stats, limits()).unwrap();
        }
    }
    let peak = stats.workspace().unwrap();
    assert!(
        super::preflight(
            &mut input,
            OpenLimits {
                max_workspace_bytes: peak,
                ..limits()
            }
        )
        .is_ok()
    );
    assert!(
        open(
            bytes,
            OpenLimits {
                max_workspace_bytes: peak - 1,
                ..limits()
            }
        )
        .is_err()
    );
}

#[test]
fn playback_ctts_preserves_large_unsigned_and_negative_signed_offsets() {
    assert_eq!(
        parse_ctts(&words(&[0, 1, 1, u32::MAX])).unwrap(),
        [(1, i64::from(u32::MAX))]
    );
    assert_eq!(
        parse_ctts(&words(&[0x01000000, 1, 1, u32::MAX])).unwrap(),
        [(1, -1)]
    );
}

#[test]
fn playback_preflight_body_uses_one_admitted_allocation_at_growth_boundaries() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Observed {
        bytes: Cursor<Vec<u8>>,
        largest: Arc<AtomicUsize>,
    }
    impl std::io::Read for Observed {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.largest.fetch_max(output.len(), Ordering::Relaxed);
            std::io::Read::read(&mut self.bytes, output)
        }
    }
    impl std::io::Seek for Observed {
        fn seek(&mut self, offset: SeekFrom) -> std::io::Result<u64> {
            self.bytes.seek(offset)
        }
    }
    for length in [8191, 8192, 8193, 16383, 16384, 16385, 1024 * 1024] {
        let bytes = boxed(b"moov", &boxed(b"free", &vec![0; length - 8]));
        let largest = Arc::new(AtomicUsize::new(0));
        let input = Observed {
            bytes: Cursor::new(bytes),
            largest: largest.clone(),
        };
        let selected = OpenLimits {
            max_metadata_bytes: 16 * 1024 * 1024,
            max_table_entries: 1_000_000,
            max_depth: 32,
            max_workspace_bytes: length + 4096,
            ..limits()
        };
        let error = open_playback_with_limits(Box::new(input), &NullCodecResolver, selected)
            .err()
            .unwrap();
        assert!(error.to_string().contains("workspace limit"));
        // The body read follows one exact reservation, rather than repeated
        // 4096-byte reads that grow and overlap old/replacement buffers.
        assert_eq!(largest.load(Ordering::Relaxed), length, "body {length}");
    }
}

#[test]
fn playback_stsc_rejects_zero_progress_and_invalid_description_before_expansion() {
    for (first, samples, description_index) in [(1, 0, 1), (0, 1, 1), (1, 1, 0), (1, 1, 2)] {
        let tables = joined(&[
            boxed(b"stts", &words(&[0, 1, 1, 3000])),
            boxed(b"stsz", &words(&[0, 4, 1])),
            boxed(
                b"stsc",
                &words(&[0, 2, first, samples, description_index, u32::MAX, 1, 1]),
            ),
            boxed(b"stco", &words(&[0, 1, 0])),
        ]);
        let bytes = init(&[track(1, &[description(100, &[])], &tables, &[])], &[]);
        // Assert the pre-expansion rejection first. Without the guard this
        // assertion fails, so the historical four-billion-iteration path is
        // never executed by the regression test itself.
        let error = super::preflight(&mut Cursor::new(bytes.clone()), limits())
            .err()
            .unwrap();
        assert!(error.to_string().contains("stsc run"));
        assert!(open(bytes, limits()).is_err());
    }
}

#[test]
fn playback_rejects_truncated_fragment_group_evidence_instead_of_dropping_it() {
    let canonical = [
        2, 0, 0, 0, b'r', b'o', b'l', b'l', 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1,
    ];
    let groups = [
        boxed(b"sgpd", &canonical),
        boxed(
            b"sgpd",
            &joined(&[words(&[0]), b"prol".to_vec(), words(&[1])]),
        ),
        boxed(
            b"sgpd",
            &joined(&[words(&[0x01000000]), b"roll".to_vec(), words(&[0, 1, 2])]),
        ),
        boxed(
            b"sbgp",
            &joined(&[words(&[0]), b"roll".to_vec(), words(&[1])]),
        ),
    ];
    for (index, group) in groups.into_iter().enumerate() {
        let bytes = joined(&[
            basic_init(),
            fragment_with(&[traf(1, 0x020000, None, 0, 0, &[0], &group)], &[]),
        ]);
        assert!(open(bytes.clone(), limits()).is_err());
        // The bounded entry is strict; ordinary tolerant fragment policy stays.
        let normal = open_typed(Box::new(Cursor::new(bytes)), &NullCodecResolver).unwrap();
        if index == 0 {
            assert!(normal.traf_sample_groups().is_empty());
        }
        assert!(
            open(
                init(&[track(1, &[description(100, &[])], &group, &[])], &[]),
                limits()
            )
            .is_err()
        );
    }
}

#[test]
fn playback_stsc_admits_a_positive_bounded_run() {
    let tables = joined(&[
        boxed(b"stts", &words(&[0, 1, 1, 3000])),
        boxed(b"stsz", &words(&[0, 4, 1])),
        boxed(b"stsc", &words(&[0, 1, 1, 1, 1])),
        boxed(b"stco", &words(&[0, 1, 0])),
    ]);
    let reader = open(
        init(&[track(1, &[description(100, &[])], &tables, &[])], &[]),
        limits(),
    )
    .unwrap();
    assert_eq!(reader.samples(0).len(), 1);
    assert_eq!(reader.samples(0)[0].duration, 3000);
}
