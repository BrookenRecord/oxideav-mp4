//! Resource preflight for the existing finite playback-reader entry point.
mod groups;
mod preflight;

use super::*;
use std::mem::size_of;

/// Finite caller-selected ceilings for `open_playback_with_limits`.
/// All ceilings are inclusive; zero count/byte ceilings admit no corresponding
/// allocations. max_depth must be 1..=32. Limits apply to one immutable seekable
/// input window and opening the reader, not later caller-owned accessor results,
/// packet payloads, input storage or arbitrary CodecResolver allocations.
/// Configured ceiling refusals return `Error::ResourceExhausted`; invalid limit
/// arguments and malformed input return `Error::InvalidData`.
#[derive(Clone, Copy, Debug)]
pub struct OpenLimits {
    /// Aggregate resident stsz/stz2 and fragmented trun sample count.
    pub max_samples: usize,
    /// Track boxes in the movie header.
    pub max_tracks: usize,
    /// Aggregate stsd descriptions, including inactive descriptions.
    pub max_sample_descriptions: usize,
    /// Aggregate visited boxes and declared ancillary/table entries.
    pub max_table_entries: usize,
    /// Aggregate buffered moov/moof bodies, in bytes, including embedded configs.
    pub max_metadata_bytes: usize,
    /// Conservative peak parser/reader workspace capacity bytes; excludes input
    /// storage, allocator bookkeeping and error diagnostics. Checked before parsed
    /// records are allocated. Reserve this amount before opening the reader.
    pub max_workspace_bytes: usize,
    /// Inclusive nested metadata/sample-entry box depth, in 1..=32.
    pub max_depth: usize,
}

pub(super) struct Plan {
    pub samples: usize,
}

#[derive(Default)]
struct Stats {
    samples: usize,
    tracks: usize,
    descriptions: usize,
    entries: usize,
    metadata: usize,
    depth: usize,
}

fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or_else(|| Error::invalid("MP4: playback allocation arithmetic overflow"))
}

fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b)
        .ok_or_else(|| Error::invalid("MP4: playback allocation arithmetic overflow"))
}

impl Stats {
    fn entries(&mut self, count: usize, limits: OpenLimits) -> Result<()> {
        self.entries = add(self.entries, count)?;
        if self.entries > limits.max_table_entries {
            return Err(Error::resource_exhausted("MP4: playback table-entry limit"));
        }
        Ok(())
    }

    fn samples(&mut self, count: usize, limits: OpenLimits) -> Result<()> {
        self.samples = add(self.samples, count)?;
        if self.samples > limits.max_samples {
            return Err(Error::resource_exhausted("MP4: playback sample limit"));
        }
        self.entries(count, limits)
    }

    // Each term corresponds to simultaneous owners in the existing open path.
    // See PLAYBACK-LIMITS.md for the per-allocation derivation.
    fn workspace(&self) -> Result<usize> {
        let copies = add(self.depth, 27)?;
        let mut bytes = add(4096, mul(self.metadata, copies)?)?;
        let sample_width = size_of::<SampleRef>()
            + size_of::<TrunSample>()
            + size_of::<(i64, i64)>()
            + size_of::<i64>()
            + 3 * size_of::<u32>();
        bytes = add(bytes, mul(self.samples, sample_width)?)?;
        let track_width = 4 * size_of::<Track>()
            + size_of::<StreamInfo>()
            + size_of::<usize>()
            + size_of::<i64>()
            + size_of::<Option<u8>>()
            + size_of::<Option<PiffTencBox>>()
            + 2 * size_of::<u32>()
            + size_of::<Option<GaplessTiming>>()
            + size_of::<TrackHeader>()
            + size_of::<Vec<StsdEntry>>()
            + size_of::<Vec<EditListEntry>>()
            + 2 * size_of::<HeaderTimestamps>()
            + 1024;
        bytes = add(bytes, mul(self.tracks, track_width)?)?;
        // Geometric vectors request <2x their live count after the initial
        // four-record capacity; 4x covers that initial allocation as well.
        // The sum covers every admitted table/group/metadata record family;
        // charging all families per entry deliberately overestimates co-ownership.
        let entry_width = 4
            * (size_of::<StsdEntry>()
                + size_of::<SgpdBox>()
                + size_of::<SbgpBox>()
                + size_of::<CsgpBox>()
                + size_of::<CsgpPattern>()
                + 3 * size_of::<EditListEntry>()
                + 3 * size_of::<(u32, u32, u32)>()
                + size_of::<(String, String)>()
                + size_of::<TrafSampleGroupRecord>()
                + size_of::<EmptyDurationRecord>()
                + size_of::<MoofRecord>()
                + size_of::<Option<(i64, u32)>>()
                + 4 * (size_of::<u32>() + 1))
            + 256;
        add(bytes, mul(self.entries, entry_width)?)
    }
}

pub(super) fn preflight(input: &mut dyn ReadSeek, limits: OpenLimits) -> Result<Plan> {
    if limits.max_depth == 0 || limits.max_depth > 32 {
        return Err(Error::invalid("MP4: unsupported playback nesting limit"));
    }
    if input.stream_position()? != 0 {
        return Err(Error::invalid("MP4: playback input must start at zero"));
    }
    let end = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;
    let mut stats = Stats::default();
    let mut moov_seen = false;
    while input.stream_position()? < end {
        let header =
            read_box_header(input)?.ok_or_else(|| Error::invalid("MP4: missing playback box"))?;
        stats.entries(1, limits)?;
        let start = input.stream_position()?;
        let length = header.payload_size().unwrap_or(end - start);
        let next = start
            .checked_add(length)
            .filter(|next| *next <= end)
            .ok_or_else(|| Error::invalid("MP4: playback box exceeds input"))?;
        let mut prefix = [0; 16];
        let prefix_len = (length.min(16)) as usize;
        input.read_exact(&mut prefix[..prefix_len])?;
        preflight::clear(header.fourcc, &prefix[..prefix_len])?;
        input.seek(SeekFrom::Start(start))?;
        if matches!(header.fourcc, MOOV | MOOF) {
            if header.total_size.is_none() {
                return Err(Error::invalid("MP4: open-ended playback metadata"));
            }
            if header.fourcc == MOOV {
                if moov_seen {
                    return Err(Error::invalid("MP4: duplicate playback moov"));
                }
                moov_seen = true;
            }
            let length = usize::try_from(length)
                .map_err(|_| Error::invalid("MP4: metadata exceeds usize"))?;
            stats.metadata = add(stats.metadata, length)?;
            if stats.metadata > limits.max_metadata_bytes
                || add(length, 4096)? > limits.max_workspace_bytes
            {
                return Err(Error::resource_exhausted(
                    "MP4: playback metadata byte limit",
                ));
            }
            // This preflight owns no prior body. Allocate once after proving the
            // complete extent and cap; geometric growth could overlap allocations.
            let mut body = Vec::new();
            body.try_reserve_exact(length).map_err(|_| {
                Error::resource_exhausted("MP4: playback preflight allocation failed")
            })?;
            body.resize(length, 0);
            input.read_exact(&mut body)?;
            preflight::walk(&body, header.fourcc, 1, None, &mut stats, limits)?;
        }
        input.seek(SeekFrom::Start(next))?;
    }
    if stats.workspace()? > limits.max_workspace_bytes {
        return Err(Error::resource_exhausted("MP4: playback workspace limit"));
    }
    input.seek(SeekFrom::Start(0))?;
    Ok(Plan {
        samples: stats.samples,
    })
}

#[cfg(test)]
mod tests;
