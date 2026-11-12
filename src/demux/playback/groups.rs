// Structural validation precedes the legacy fragment parser's tolerant policy.
use super::preflight::u32_at;
use super::*;

pub(super) fn validate(kind: [u8; 4], body: &[u8], count: usize, header: usize) -> Result<()> {
    if kind == SBGP {
        if body.len() != add(header, mul(count, 8)?)? {
            return Err(Error::invalid("MP4: truncated playback sbgp"));
        }
        return Ok(());
    }
    if body[0] == 0 {
        if matches!(body.get(4..8), Some(b"roll") | Some(b"prol")) {
            if body.len() != add(header, mul(count, 2)?)? {
                return Err(Error::invalid("MP4: truncated playback roll/prol"));
            }
        } else if (count == 0 && body.len() != header) || (count > 0 && body.len() == header) {
            return Err(Error::invalid("MP4: missing playback group payload"));
        }
        return Ok(());
    }
    let width = u32_at(body, 8)? as usize;
    if width != 0 {
        if body.len() != add(header, mul(count, width)?)? {
            return Err(Error::invalid("MP4: truncated playback sgpd"));
        }
        return Ok(());
    }
    let mut offset = header;
    for _ in 0..count {
        let length = u32_at(body, offset)? as usize;
        offset = add(add(offset, 4)?, length)?;
        if offset > body.len() {
            return Err(Error::invalid("MP4: truncated playback variable sgpd"));
        }
    }
    if offset != body.len() {
        return Err(Error::invalid("MP4: playback sgpd length mismatch"));
    }
    Ok(())
}
