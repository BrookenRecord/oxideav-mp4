use super::*;

pub(super) fn u32_at(body: &[u8], offset: usize) -> Result<u32> {
    let raw = body
        .get(offset..add(offset, 4)?)
        .ok_or_else(|| Error::invalid("MP4: truncated playback field"))?;
    Ok(u32::from_be_bytes(raw.try_into().unwrap()))
}

pub(super) fn clear(kind: [u8; 4], body: &[u8]) -> Result<()> {
    if matches!(
        &kind,
        b"encv"
            | b"enca"
            | b"enct"
            | b"encs"
            | b"sinf"
            | b"schi"
            | b"tenc"
            | b"senc"
            | b"pssh"
            | b"saiz"
            | b"saio"
    ) || (matches!(kind, SBGP | SGPD | CSGP) && body.get(4..8) == Some(b"seig"))
        || (kind == UUID
            && body.get(..16).is_some_and(|uuid| {
                uuid == PIFF_PSSH_USERTYPE
                    || uuid == PIFF_TENC_USERTYPE
                    || uuid == PIFF_SENC_USERTYPE
            }))
    {
        return Err(Error::unsupported("MP4: protected playback media"));
    }
    if kind == *b"cmov" {
        return Err(Error::unsupported("MP4: compressed playback metadata"));
    }
    Ok(())
}

pub(super) fn next<'a>(bytes: &mut &'a [u8]) -> Result<Option<([u8; 4], &'a [u8])>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut cursor = std::io::Cursor::new(*bytes);
    let header =
        read_box_header(&mut cursor)?.ok_or_else(|| Error::invalid("MP4: missing playback box"))?;
    let size = usize::try_from(
        header
            .total_size
            .ok_or_else(|| Error::invalid("MP4: open-ended nested playback box"))?,
    )
    .map_err(|_| Error::invalid("MP4: playback box exceeds usize"))?;
    let full = bytes
        .get(..size)
        .ok_or_else(|| Error::invalid("MP4: truncated playback box"))?;
    let body = &full[header.header_len as usize..];
    *bytes = &bytes[size..];
    clear(header.fourcc, body)?;
    Ok(Some((header.fourcc, body)))
}

fn handler(mut bytes: &[u8]) -> Result<MediaType> {
    while let Some((kind, body)) = next(&mut bytes)? {
        if kind == MDIA {
            let mut media = body;
            while let Some((kind, body)) = next(&mut media)? {
                if kind == HDLR {
                    return match body.get(8..12) {
                        Some(b"vide") => Ok(MediaType::Video),
                        Some(b"soun") => Ok(MediaType::Audio),
                        _ => Err(Error::unsupported("MP4: unsupported playback handler")),
                    };
                }
            }
        }
    }
    Err(Error::invalid("MP4: missing playback handler"))
}

#[derive(Clone, Copy)]
pub(super) struct TrackContext {
    media_type: MediaType,
    descriptions: usize,
}

fn description_count(mut bytes: &[u8], path: &[[u8; 4]]) -> Result<usize> {
    let mut count = 0;
    while let Some((kind, body)) = next(&mut bytes)? {
        if kind != path[0] {
            continue;
        }
        count = add(
            count,
            if path.len() == 1 {
                u32_at(body, 4)? as usize
            } else {
                description_count(body, &path[1..])?
            },
        )?;
    }
    Ok(count)
}

pub(super) fn walk(
    mut bytes: &[u8],
    parent: [u8; 4],
    depth: usize,
    media: Option<TrackContext>,
    stats: &mut Stats,
    limits: OpenLimits,
) -> Result<()> {
    if depth > limits.max_depth {
        return Err(Error::resource_exhausted("MP4: playback depth limit"));
    }
    stats.depth = stats.depth.max(depth);
    while let Some((kind, body)) = next(&mut bytes)? {
        stats.entries(1, limits)?;
        match kind {
            MVHD | MDHD | TKHD => {
                let version = body
                    .first()
                    .copied()
                    .ok_or_else(|| Error::invalid("MP4: empty playback header"))?;
                if version > 1 {
                    return Err(Error::unsupported("MP4: playback header version"));
                }
                let size = match kind {
                    MVHD => {
                        if version == 0 {
                            100
                        } else {
                            112
                        }
                    }
                    MDHD => {
                        if version == 0 {
                            24
                        } else {
                            36
                        }
                    }
                    _ => {
                        if version == 0 {
                            84
                        } else {
                            96
                        }
                    }
                };
                if body.len() != size {
                    return Err(Error::invalid("MP4: truncated playback header"));
                }
                if kind != TKHD && u32_at(body, if version == 0 { 12 } else { 20 })? == 0 {
                    return Err(Error::invalid("MP4: zero playback timescale"));
                }
            }
            TFDT => {
                if body.first().copied().unwrap_or(255) > 1 {
                    return Err(Error::unsupported("MP4: playback tfdt version"));
                }
                parse_tfdt(body)?;
            }
            TRAK if parent == MOOV => {
                stats.tracks = add(stats.tracks, 1)?;
                if stats.tracks > limits.max_tracks {
                    return Err(Error::resource_exhausted("MP4: playback track limit"));
                }
                walk(
                    body,
                    kind,
                    depth + 1,
                    Some(TrackContext {
                        media_type: handler(body)?,
                        descriptions: description_count(body, &[MDIA, MINF, STBL, STSD])?,
                    }),
                    stats,
                    limits,
                )?;
            }
            MDIA | MINF | STBL | EDTS | MVEX | TRAF => {
                walk(body, kind, depth + 1, media, stats, limits)?
            }
            UDTA | META => {
                let inner = if kind == META {
                    body.get(4..)
                        .ok_or_else(|| Error::invalid("MP4: short playback meta"))?
                } else {
                    body
                };
                walk(inner, kind, depth + 1, media, stats, limits)?;
            }
            ILST if parent == META => walk(body, kind, depth + 1, media, stats, limits)?,
            _ if parent == ILST => walk(body, kind, depth + 1, media, stats, limits)?,
            HDLR if parent == MDIA => {
                let expected = match media.map(|context| context.media_type) {
                    Some(MediaType::Audio) => b"soun",
                    Some(MediaType::Video) => b"vide",
                    _ => return Err(Error::invalid("MP4: missing playback handler context")),
                };
                if body.get(8..12) != Some(expected) {
                    return Err(Error::invalid("MP4: inconsistent playback handlers"));
                }
            }
            STSD if parent == STBL => {
                if body.first().copied().unwrap_or(255) > 1 {
                    return Err(Error::unsupported("MP4: playback stsd version"));
                }
                let count = u32_at(body, 4)? as usize;
                stats.descriptions = add(stats.descriptions, count)?;
                if stats.descriptions > limits.max_sample_descriptions {
                    return Err(Error::resource_exhausted("MP4: playback description limit"));
                }
                stats.entries(count, limits)?;
                let mut entries = &body[8..];
                for _ in 0..count {
                    let (kind, entry) = next(&mut entries)?
                        .ok_or_else(|| Error::invalid("MP4: missing playback sample entry"))?;
                    let family = match &kind {
                        b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" | b"av01"
                        | b"vp09" | b"mp4v" => MediaType::Video,
                        b"mp4a" | b"ac-3" | b"ec-3" | b"Opus" | b"fLaC" | b"alac" | b"sowt"
                        | b"twos" => MediaType::Audio,
                        _ => {
                            return Err(Error::unsupported(
                                "MP4: unsupported playback entry layout",
                            ));
                        }
                    };
                    if media.map(|context| context.media_type) != Some(family) {
                        return Err(Error::invalid("MP4: handler/entry family mismatch"));
                    }
                    if entry.get(6..8) != Some(&[0, 1]) {
                        return Err(Error::unsupported("MP4: external playback data reference"));
                    }
                    let start = if family == MediaType::Video {
                        78
                    } else {
                        let version = entry
                            .get(8..10)
                            .ok_or_else(|| Error::invalid("MP4: short playback audio entry"))?;
                        match u16::from_be_bytes(version.try_into().unwrap()) {
                            0 => 28,
                            1 if body[0] == 1 => 28,
                            1 => 44,
                            2 => 64,
                            _ => {
                                return Err(Error::unsupported(
                                    "MP4: playback audio entry version",
                                ));
                            }
                        }
                    };
                    let children = entry
                        .get(start..)
                        .ok_or_else(|| Error::invalid("MP4: truncated playback sample entry"))?;
                    entry_children(children, depth + 2, stats, limits)?;
                }
                if !entries.is_empty() {
                    return Err(Error::invalid("MP4: playback stsd count mismatch"));
                }
            }
            STSZ | STZ2 if parent == STBL => stats.samples(u32_at(body, 8)? as usize, limits)?,
            TRUN if parent == TRAF => stats.samples(u32_at(body, 4)? as usize, limits)?,
            STSC if parent == STBL => {
                let count = u32_at(body, 4)? as usize;
                stats.entries(count, limits)?;
                if body.len() != add(8, mul(count, 12)?)? {
                    return Err(Error::invalid("MP4: truncated playback stsc"));
                }
                let descriptions = media
                    .ok_or_else(|| Error::invalid("MP4: missing stsc track context"))?
                    .descriptions;
                let mut previous = 0;
                for index in 0..count {
                    let offset = 8 + index * 12;
                    let first = u32_at(body, offset)?;
                    let samples = u32_at(body, offset + 4)?;
                    let description = u32_at(body, offset + 8)? as usize;
                    if first <= previous
                        || samples == 0
                        || description == 0
                        || description > descriptions
                    {
                        return Err(Error::invalid("MP4: invalid playback stsc run"));
                    }
                    previous = first;
                }
            }
            STTS | STCO | CO64 | STSS | CTTS | ELST => {
                stats.entries(u32_at(body, 4)? as usize, limits)?
            }
            SBGP | SGPD => {
                let count_offset = match (kind, body.first().copied()) {
                    (SBGP, Some(0)) | (SGPD, Some(0)) => 8,
                    (SBGP, Some(1)) | (SGPD, Some(1)) => 12,
                    (SGPD, Some(2)) => 16,
                    _ => return Err(Error::unsupported("MP4: playback sample-group version")),
                };
                let count = u32_at(body, count_offset)? as usize;
                stats.entries(count, limits)?;
                super::groups::validate(kind, body, count, count_offset + 4)?;
            }
            CSGP => {
                let flags = u32_at(body, 0)?;
                let offset = if flags & 0x40 != 0 { 12 } else { 8 };
                let count = u32_at(body, offset)? as usize;
                stats.entries(count, limits)?;
                let mut bits = BitCursor::new(body, offset + 4);
                let pattern_width = 4 << ((flags >> 4) & 3);
                let count_width = 4 << ((flags >> 2) & 3);
                let index_width = 4 << (flags & 3);
                let mut indices = 0;
                for _ in 0..count {
                    indices = add(
                        indices,
                        bits.read(pattern_width)
                            .ok_or_else(|| Error::invalid("MP4: truncated playback csgp"))?
                            as usize,
                    )?;
                    stats.entries(
                        bits.read(count_width)
                            .ok_or_else(|| Error::invalid("MP4: truncated playback csgp"))?
                            as usize,
                        limits,
                    )?;
                }
                stats.entries(indices, limits)?;
                for _ in 0..indices {
                    bits.read(index_width)
                        .ok_or_else(|| Error::invalid("MP4: truncated playback csgp indices"))?;
                }
            }
            DINF => {
                let mut refs = body;
                while let Some((kind, body)) = next(&mut refs)? {
                    stats.entries(1, limits)?;
                    if kind != DREF {
                        continue;
                    }
                    stats.entries(u32_at(body, 4)? as usize, limits)?;
                    let count = u32_at(body, 4)? as usize;
                    let mut actual = 0;
                    let mut entries = &body[8..];
                    while let Some((kind, body)) = next(&mut entries)? {
                        actual = add(actual, 1)?;
                        stats.entries(1, limits)?;
                        if kind != *b"url " || body != [0, 0, 0, 1] {
                            return Err(Error::unsupported(
                                "MP4: external playback data reference",
                            ));
                        }
                    }
                    if count == 0 || count != actual {
                        return Err(Error::invalid("MP4: playback dref count mismatch"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn entry_children(
    mut bytes: &[u8],
    depth: usize,
    stats: &mut Stats,
    limits: OpenLimits,
) -> Result<()> {
    if depth > limits.max_depth {
        return Err(Error::resource_exhausted("MP4: playback entry depth limit"));
    }
    stats.depth = stats.depth.max(depth);
    while !bytes.is_empty() {
        if bytes == [0, 0, 0, 0] {
            break;
        }
        let (kind, body) =
            next(&mut bytes)?.ok_or_else(|| Error::invalid("MP4: missing playback entry child"))?;
        stats.entries(1, limits)?;
        if kind == *b"wave" {
            entry_children(body, depth + 1, stats, limits)?;
        }
    }
    Ok(())
}
