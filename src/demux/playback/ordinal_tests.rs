//! Public reader ownership checks with authored, deliberately reordered fixtures.
use super::*;

fn grouped_traf(id: u32, dts: u64, runs: &[(u32, u32)], grouped: bool, empty: bool) -> Vec<u8> {
    let mut body = boxed(
        b"tfhd",
        &words(&[0x020000 | if empty { 0x010000 } else { 0 }, id]),
    );
    body.extend(boxed(
        b"tfdt",
        &[words(&[0x01000000]), dts.to_be_bytes().to_vec()].concat(),
    ));
    if grouped {
        // Version 1 roll description and fragment-local mapping; no writer roundtrip.
        body.extend(boxed(
            b"sgpd",
            &[
                vec![1, 0, 0, 0],
                b"roll".to_vec(),
                words(&[2, 1]),
                vec![255, 255],
            ]
            .concat(),
        ));
        body.extend(boxed(
            b"sbgp",
            &[
                words(&[0]),
                b"roll".to_vec(),
                words(&[1, runs.iter().map(|(_, count)| count).sum(), 0x10001]),
            ]
            .concat(),
        ));
    }
    for &(offset, count) in runs {
        body.extend(boxed(b"trun", &words(&[1, count, offset])));
    }
    boxed(b"traf", &body)
}

fn static_track(id: u32, offset: u32) -> Vec<u8> {
    let tables = joined(&[
        boxed(b"stts", &words(&[0, 1, 1, 3000])),
        boxed(b"stsz", &words(&[0, 4, 1])),
        boxed(b"stsc", &words(&[0, 1, 1, 1, 1])),
        boxed(b"stco", &words(&[0, 1, offset])),
    ]);
    track(id, &[description(100, &[])], &tables, &[])
}

#[test]
fn public_group_ranges_survive_offset_sort_repeated_sequences_and_overlapping_dts() {
    let header = init(&[static_track(1, 42000), static_track(2, 41000)], &[]);
    let first = fragment_with(
        &[
            grouped_traf(1, 100, &[(20000, 2), (5000, 1)], true, false),
            grouped_traf(2, 0, &[(15000, 1)], false, false),
        ],
        &[],
    );
    let second = fragment_with(
        &[
            grouped_traf(1, 50, &[(10000, 1)], false, false),
            grouped_traf(1, 50, &[(1000, 1)], true, false),
            grouped_traf(2, 0, &[(30000, 1)], true, false),
            grouped_traf(1, 999, &[], true, false),
        ],
        &[],
    );
    let first_offset = header.len() as u64;
    let second_offset = (header.len() + first.len()) as u64;
    let bytes = joined(&[header, first, second]);
    let bounded = open(bytes.clone(), limits()).unwrap();
    let ordinary = open_typed(Box::new(Cursor::new(bytes)), &NullCodecResolver).unwrap();
    let selected = bounded.samples(0);
    assert_eq!(selected, ordinary.samples(0));
    assert_eq!(
        selected.iter().map(|s| s.ordinal).collect::<Vec<_>>(),
        [0, 2, 3, 4, 6, 7]
    );
    assert_eq!(
        selected.iter().map(|s| s.decode_time).collect::<Vec<_>>(),
        [0, 100, 3100, 6100, 50, 50]
    );
    assert_eq!(
        selected.iter().map(|s| s.offset).collect::<Vec<_>>(),
        [
            42000,
            first_offset + 20000,
            first_offset + 20004,
            first_offset + 5000,
            second_offset + 10000,
            second_offset + 1000
        ]
    );
    assert_eq!(
        bounded
            .samples(1)
            .iter()
            .map(|s| s.ordinal)
            .collect::<Vec<_>>(),
        [1, 5, 8]
    );

    let groups = bounded.traf_sample_groups();
    assert_eq!(
        groups
            .iter()
            .map(|g| (
                g.track_idx,
                g.moof_sequence,
                g.first_sample_ordinal,
                g.sample_count
            ))
            .collect::<Vec<_>>(),
        [(0, 1, 2, 3), (0, 1, 7, 1), (1, 1, 8, 1), (0, 1, 9, 0)]
    );
    for group in groups {
        let end = group
            .first_sample_ordinal
            .checked_add(group.sample_count)
            .unwrap();
        let selected = bounded.samples(group.track_idx);
        let actual = selected
            .iter()
            .filter(|s| (group.first_sample_ordinal..end).contains(&s.ordinal))
            .map(|s| s.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            (group.first_sample_ordinal..end).collect::<Vec<_>>()
        );
        // No different track may acquire these samples through an ordinal match.
        let other = bounded.samples(1 - group.track_idx);
        assert!(
            other
                .iter()
                .all(|s| !(group.first_sample_ordinal..end).contains(&s.ordinal))
        );
    }
}

#[test]
fn empty_and_unknown_trafs_do_not_shift_group_membership() {
    let header = basic_init();
    let moof = fragment_with(
        &[
            grouped_traf(1, 0, &[], true, false),
            grouped_traf(1, 0, &[], true, true),
            grouped_traf(1, 0, &[(1000, 1)], false, false),
            grouped_traf(1, 0, &[(1004, 1)], true, false),
        ],
        &[],
    );
    let parsed = open(joined(&[header.clone(), moof.clone()]), limits()).unwrap();
    assert_eq!(
        parsed
            .samples(0)
            .iter()
            .map(|s| s.ordinal)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        parsed
            .traf_sample_groups()
            .iter()
            .map(|g| (g.first_sample_ordinal, g.sample_count))
            .collect::<Vec<_>>(),
        [(0, 0), (1, 1)]
    );
    assert_eq!(parsed.empty_duration_records().len(), 1);
    let unknown = fragment_with(&[grouped_traf(99, 0, &[(1000, 1)], true, false)], &[]);
    assert!(matches!(
        open(joined(&[header, moof, unknown]), limits()),
        Err(Error::InvalidData(_))
    ));
    // Overflow also returns no reader containing partially emitted group metadata.
    let overflow = fragment_with(
        &[grouped_traf(1, i64::MAX as u64, &[(1000, 1)], true, false)],
        &[],
    );
    assert!(matches!(
        open(joined(&[basic_init(), overflow]), limits()),
        Err(Error::InvalidData(_))
    ));
}

#[test]
fn enlarged_group_records_and_accessor_records_are_in_admission_estimates() {
    // Field widths are checked on this target; caller accounting uses size_of.
    let field_bytes = 5 * std::mem::size_of::<u64>()
        + 2 * std::mem::size_of::<u32>()
        + std::mem::size_of::<bool>();
    assert!(std::mem::size_of::<SampleInfo>() >= field_bytes);
    let prior_group_shape = (
        0u32,
        0u32,
        Vec::<SgpdBox>::new(),
        Vec::<SbgpBox>::new(),
        Vec::<CsgpBox>::new(),
    );
    assert_eq!(
        std::mem::size_of::<TrafSampleGroupRecord>(),
        std::mem::size_of_val(&prior_group_shape) + 2 * std::mem::size_of::<u64>()
    );
    let moof = fragment_with(&[grouped_traf(1, 0, &[(1000, 2)], true, false)], &[]);
    let bytes = joined(&[basic_init(), moof]);
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
    let prior_peak = peak - stats.entries * 4 * 2 * std::mem::size_of::<u64>();
    assert!(
        open(
            bytes.clone(),
            OpenLimits {
                max_workspace_bytes: peak,
                ..limits()
            }
        )
        .is_ok()
    );
    assert!(matches!(
        open(
            bytes,
            OpenLimits {
                max_workspace_bytes: prior_peak,
                ..limits()
            }
        ),
        Err(Error::ResourceExhausted(_))
    ));
}
