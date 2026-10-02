//! BLTE: the chunked, optionally compressed container every CASC file is stored in.

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

/// Decode a BLTE blob into the file's plain bytes.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(data.len() >= 8 && &data[..4] == b"BLTE", "not a BLTE blob");
    let header_size = be32(&data[4..]) as usize;

    // headerSize == 0: a single chunk covering the rest of the blob.
    if header_size == 0 {
        let mut out = Vec::new();
        chunk(&data[8..], &mut out)?;
        return Ok(out);
    }

    ensure!(data.len() >= header_size && header_size >= 12, "truncated BLTE header");
    let count = (be32(&data[8..]) & 0x00FF_FFFF) as usize;
    ensure!(12 + count * 24 <= header_size, "BLTE chunk table overflows header");

    let mut out = Vec::new();
    let mut pos = header_size;
    for i in 0..count {
        let e = &data[12 + i * 24..];
        let (comp, decomp) = (be32(e) as usize, be32(&e[4..]) as usize);
        let body = data.get(pos..pos + comp).context("truncated BLTE chunk")?;
        let before = out.len();
        chunk(body, &mut out)?;
        ensure!(out.len() - before == decomp, "BLTE chunk {i}: size mismatch");
        pos += comp;
    }
    Ok(out)
}

fn chunk(body: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let (&mode, rest) = body.split_first().context("empty BLTE chunk")?;
    match mode {
        b'N' => out.extend_from_slice(rest),
        b'Z' => {
            flate2::read::ZlibDecoder::new(rest).read_to_end(out).context("BLTE zlib")?;
        }
        b'F' => out.extend(decode(rest)?),
        b'E' => bail!("encrypted BLTE chunk"),
        m => bail!("unsupported BLTE chunk mode {:?}", m as char),
    }
    Ok(())
}
